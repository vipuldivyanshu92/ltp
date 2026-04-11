//! Strict priority scheduler with video EDF and optional telemetry guard.

use std::cmp::Ordering;
use std::collections::{BinaryHeap, VecDeque};

use crate::header::{PriorityClass, PayloadType};

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
        // Max-heap: earlier deadline (smaller key) must sort “greater” so it pops first.
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
        }
    }
}

impl PriorityScheduler {
    pub fn enqueue(&mut self, p: ScheduledPacket) {
        match p.priority {
            PriorityClass::Control => self.control.push_back(p),
            PriorityClass::Haptic => self.haptic.push_back(p),
            PriorityClass::Video => self.video.push(p),
            PriorityClass::Telemetry => self.telemetry.push_back(p),
        }
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
}
