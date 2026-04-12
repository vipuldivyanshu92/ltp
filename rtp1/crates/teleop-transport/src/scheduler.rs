//! Strict priority scheduler with video EDF, deadline purge, and congestion cap.

use std::cmp::Ordering;
use std::collections::{BinaryHeap, VecDeque};

use crate::header::{PriorityClass, PayloadType};

/// When the video queue exceeds this many datagrams, the oldest (earliest
/// deadline) are drained until we're back under the cap. At 1380-byte MTU this
/// is roughly 1.4 MB of buffered video — enough for ~5 full-frame JPEGs of a
/// single camera while still bounding memory and latency.
const VIDEO_QUEUE_CAP: usize = 1024;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ScheduledPacket {
    pub priority: PriorityClass,
    pub payload_type: PayloadType,
    pub datagram: Vec<u8>,
    /// For video slices: deadline (e.g. capture time + budget); lower is earlier.
    pub deadline_key: u64,
}

impl Ord for ScheduledPacket {
    fn cmp(&self, other: &Self) -> Ordering {
        // Max-heap: earlier deadline (smaller key) must sort "greater" so it pops first.
        other
            .deadline_key
            .cmp(&self.deadline_key)
            .then_with(|| (other.priority as u8).cmp(&(self.priority as u8)))
            .then_with(|| self.datagram.cmp(&other.datagram))
    }
}

impl PartialOrd for ScheduledPacket {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

pub struct PriorityScheduler {
    control: VecDeque<ScheduledPacket>,
    haptic: VecDeque<ScheduledPacket>,
    video: BinaryHeap<ScheduledPacket>,
    telemetry: VecDeque<ScheduledPacket>,
    /// Bytes per interval budget for telemetry to avoid starvation (0 = strict no guard).
    pub telemetry_bytes_budget: usize,
    telemetry_sent_this_tick: usize,
    /// Cumulative count of video datagrams dropped by deadline purge or cap overflow.
    pub video_dropped: u64,
}

impl Default for PriorityScheduler {
    fn default() -> Self {
        Self {
            control: VecDeque::new(),
            haptic: VecDeque::new(),
            video: BinaryHeap::new(),
            telemetry: VecDeque::new(),
            telemetry_bytes_budget: 0,
            telemetry_sent_this_tick: 0,
            video_dropped: 0,
        }
    }
}

impl PriorityScheduler {
    pub fn enqueue(&mut self, p: ScheduledPacket) {
        match p.priority {
            PriorityClass::Control => self.control.push_back(p),
            PriorityClass::Haptic => self.haptic.push_back(p),
            PriorityClass::Video => {
                self.video.push(p);
                // Shed oldest-deadline video when queue exceeds cap.
                while self.video.len() > VIDEO_QUEUE_CAP {
                    self.video.pop();
                    self.video_dropped += 1;
                }
            }
            PriorityClass::Telemetry => self.telemetry.push_back(p),
        }
    }

    /// Drop queued video datagrams whose deadline has already passed.
    pub fn purge_expired_video(&mut self, now_ns: u64) -> usize {
        let before = self.video.len();
        let drained: Vec<ScheduledPacket> = self.video.drain().collect();
        for p in drained {
            if p.deadline_key > now_ns {
                self.video.push(p);
            }
        }
        let dropped = before - self.video.len();
        self.video_dropped += dropped as u64;
        dropped
    }

    /// Pop next packet respecting strict priority; telemetry may slip if budget > 0 and higher queues empty.
    pub fn pop_next(&mut self) -> Option<ScheduledPacket> {
        if let Some(p) = self.control.pop_front() {
            return Some(p);
        }
        if let Some(p) = self.haptic.pop_front() {
            return Some(p);
        }
        if let Some(p) = self.video.pop() {
            return Some(p);
        }
        if self.telemetry_bytes_budget > 0 && !self.telemetry.is_empty() {
            if self.telemetry_sent_this_tick < self.telemetry_bytes_budget {
                if let Some(p) = self.telemetry.pop_front() {
                    self.telemetry_sent_this_tick += p.datagram.len();
                    return Some(p);
                }
            }
        }
        self.telemetry.pop_front()
    }

    pub fn reset_telemetry_tick(&mut self) {
        self.telemetry_sent_this_tick = 0;
    }

    /// Datagrams waiting in the send scheduler (not yet accepted by the kernel).
    pub fn pending_len(&self) -> usize {
        self.control.len()
            + self.haptic.len()
            + self.video.len()
            + self.telemetry.len()
    }

    pub fn video_queue_len(&self) -> usize {
        self.video.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pkt(priority: PriorityClass, ptype: PayloadType, deadline: u64, len: usize) -> ScheduledPacket {
        ScheduledPacket {
            priority,
            payload_type: ptype,
            datagram: vec![0u8; len],
            deadline_key: deadline,
        }
    }

    #[test]
    fn control_before_video() {
        let mut s = PriorityScheduler::default();
        s.enqueue(pkt(
            PriorityClass::Video,
            PayloadType::VideoSlice,
            1,
            10,
        ));
        s.enqueue(pkt(
            PriorityClass::Control,
            PayloadType::Control,
            9,
            5,
        ));
        let first = s.pop_next().unwrap();
        assert_eq!(first.priority, PriorityClass::Control);
    }

    #[test]
    fn video_edf() {
        let mut s = PriorityScheduler::default();
        s.enqueue(pkt(
            PriorityClass::Video,
            PayloadType::VideoSlice,
            100,
            1,
        ));
        s.enqueue(pkt(
            PriorityClass::Video,
            PayloadType::VideoSlice,
            10,
            1,
        ));
        let a = s.pop_next().unwrap();
        assert_eq!(a.deadline_key, 10);
    }

    #[test]
    fn pending_len_tracks_queues() {
        let mut s = PriorityScheduler::default();
        assert_eq!(s.pending_len(), 0);
        s.enqueue(pkt(PriorityClass::Video, PayloadType::VideoSlice, 1, 10));
        assert_eq!(s.pending_len(), 1);
        s.enqueue(pkt(PriorityClass::Control, PayloadType::Control, 0, 5));
        assert_eq!(s.pending_len(), 2);
        let _ = s.pop_next();
        assert_eq!(s.pending_len(), 1);
    }

    #[test]
    fn video_cap_sheds_oldest() {
        let mut s = PriorityScheduler::default();
        for i in 0..VIDEO_QUEUE_CAP + 100 {
            s.enqueue(pkt(
                PriorityClass::Video,
                PayloadType::VideoSlice,
                i as u64,
                10,
            ));
        }
        assert_eq!(s.video_queue_len(), VIDEO_QUEUE_CAP);
        assert_eq!(s.video_dropped, 100);
    }

    #[test]
    fn purge_expired_drops_old() {
        let mut s = PriorityScheduler::default();
        s.enqueue(pkt(PriorityClass::Video, PayloadType::VideoSlice, 100, 10));
        s.enqueue(pkt(PriorityClass::Video, PayloadType::VideoSlice, 500, 10));
        s.enqueue(pkt(PriorityClass::Video, PayloadType::VideoSlice, 200, 10));
        let dropped = s.purge_expired_video(250);
        assert_eq!(dropped, 2);
        assert_eq!(s.video_queue_len(), 1);
        let p = s.pop_next().unwrap();
        assert_eq!(p.deadline_key, 500);
    }
}
