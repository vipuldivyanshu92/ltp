//! Dedup, per-stream reorder for control, video reassembly with deadlines.

use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};

use crate::header::{LtpHeader, PayloadType, PriorityClass};

#[derive(Default)]
pub struct DedupTable {
    seen: HashSet<(u16, u32)>,
    order: VecDeque<(u16, u32)>,
    cap: usize,
}

impl DedupTable {
    pub fn new(cap: usize) -> Self {
        Self {
            seen: HashSet::new(),
            order: VecDeque::new(),
            cap,
        }
    }

    /// Returns `true` if this is the first time we accept `(stream_id, seq)`.
    pub fn first_arrival(&mut self, stream_id: u16, seq: u32) -> bool {
        let k = (stream_id, seq);
        if self.seen.contains(&k) {
            return false;
        }
        self.seen.insert(k);
        self.order.push_back(k);
        while self.order.len() > self.cap {
            if let Some(old) = self.order.pop_front() {
                self.seen.remove(&old);
            }
        }
        true
    }
}

pub struct StreamReorder {
    /// `None` until first accepted sequence establishes the stream.
    next_seq: Option<u32>,
    max_reorder: u32,
    buf: BTreeMap<u32, Vec<u8>>,
}

impl StreamReorder {
    pub fn new(max_reorder: u32) -> Self {
        Self {
            next_seq: None,
            max_reorder,
            buf: BTreeMap::new(),
        }
    }

    pub fn push(&mut self, seq: u32, payload: Vec<u8>) -> Vec<Vec<u8>> {
        let mut out = Vec::new();
        if self.next_seq.is_none() {
            self.next_seq = Some(seq);
        }
        let mut expected = self.next_seq.unwrap();
        if seq < expected {
            return out;
        }
        if seq - expected > self.max_reorder {
            self.buf.clear();
            self.next_seq = Some(seq);
            expected = seq;
        }
        self.buf.insert(seq, payload);
        while let Some(p) = self.buf.remove(&expected) {
            out.push(p);
            expected = expected.wrapping_add(1);
        }
        self.next_seq = Some(expected);
        out
    }
}

#[derive(Default)]
struct FrameBuf {
    slices: HashMap<u8, Vec<u8>>,
    expected: u8,
    /// Receiver-local monotonic cutoff (`now_ns` domain) after which the partial frame is dropped.
    /// Set from the first slice using header `(deadline_ns - timestamp_ns)` so host clock skew
    /// between sender and receiver does not instantly expire WAN video.
    drop_after_ns: u64,
}

pub struct VideoReassembly {
    frames: HashMap<u16, FrameBuf>,
    last_complete: Option<(u16, Vec<u8>)>,
}

impl Default for VideoReassembly {
    fn default() -> Self {
        Self {
            frames: HashMap::new(),
            last_complete: None,
        }
    }
}

/// Min / max reassembly window from first arriving slice (`now_ns` on this host).
const VIDEO_REASSEMBLY_BUDGET_MIN_NS: u64 = 50_000_000;
const VIDEO_REASSEMBLY_BUDGET_MAX_NS: u64 = 2_000_000_000;
const VIDEO_REASSEMBLY_BUDGET_DEFAULT_NS: u64 = 500_000_000;

fn video_reassembly_budget_ns(h: &LtpHeader) -> u64 {
    let d = h.deadline_ns();
    let t = h.timestamp_ns;
    if d > t {
        (d - t).clamp(VIDEO_REASSEMBLY_BUDGET_MIN_NS, VIDEO_REASSEMBLY_BUDGET_MAX_NS)
    } else {
        VIDEO_REASSEMBLY_BUDGET_DEFAULT_NS
    }
}

impl VideoReassembly {
    pub fn ingest(
        &mut self,
        h: &LtpHeader,
        payload: &[u8],
        now_ns: u64,
    ) -> Option<Vec<u8>> {
        if h.payload_type != PayloadType::VideoSlice {
            return None;
        }
        let frame_id = h.frame_id;
        let entry = self.frames.entry(frame_id).or_default();
        if entry.expected == 0 && h.slice_count > 0 {
            entry.expected = h.slice_count;
        }
        let budget = video_reassembly_budget_ns(h);
        // Sliding completion window: extend on every slice. A fixed deadline from only the
        // first-arriving slice breaks when UDP delivers slice 1..N-1 before slice 0 and slice 0
        // lands after that original window (WAN reordering); the frame would never complete.
        if entry.drop_after_ns == 0 {
            entry.drop_after_ns = now_ns.saturating_add(budget);
        } else {
            entry.drop_after_ns = entry.drop_after_ns.max(now_ns.saturating_add(budget));
        }
        entry.slices.insert(h.slice_id, payload.to_vec());
        let done = entry.expected > 0 && entry.slices.len() >= entry.expected as usize;
        if done {
            let mut combined = Vec::new();
            for sid in 0..entry.expected {
                if let Some(s) = entry.slices.get(&sid) {
                    combined.extend_from_slice(s);
                }
            }
            self.frames.remove(&frame_id);
            self.last_complete = Some((frame_id, combined.clone()));
            Some(combined)
        } else {
            None
        }
    }

    /// Drop partial frames whose sliding deadline has passed (no slice arrived in time).
    ///
    /// Do **not** call this on every LTP datagram: control/haptic traffic between video slices
    /// advances `now_ns` between polls and would delete in-flight multi-slice assemblies before
    /// later slices arrive. Call from a low-rate housekeeping path if needed.
    pub fn sweep_stale(&mut self, now_ns: u64) {
        let stale: Vec<u16> = self
            .frames
            .iter()
            .filter(|(_, f)| f.drop_after_ns > 0 && now_ns > f.drop_after_ns)
            .map(|(&k, _)| k)
            .collect();
        for k in stale {
            self.frames.remove(&k);
        }
    }
}

#[cfg(test)]
mod video_reassembly_tests {
    use super::VideoReassembly;
    use crate::header::{LtpHeader, PayloadType, PriorityClass};

    fn slice_hdr(
        frame_id: u16,
        slice_id: u8,
        slice_count: u8,
        capture_ns: u64,
        deadline_ns: u64,
    ) -> LtpHeader {
        let mut h = LtpHeader::default();
        h.payload_type = PayloadType::VideoSlice;
        h.priority = PriorityClass::Video;
        h.frame_id = frame_id;
        h.slice_id = slice_id;
        h.slice_count = slice_count;
        h.timestamp_ns = capture_ns;
        h.set_deadline_extension(deadline_ns);
        h
    }

    #[test]
    fn video_completes_when_receiver_clock_far_ahead_of_sender_wall_deadline() {
        let mut v = VideoReassembly::default();
        let robot_capture = 1_000_000_000_000u64;
        let h = slice_hdr(7, 0, 1, robot_capture, robot_capture + 100_000_000);
        let quest_now = robot_capture + 10_000_000_000;
        let jpeg = vec![0xff, 0xd8, 0xff, 0xd9];
        let out = v.ingest(&h, &jpeg, quest_now);
        assert!(
            out.is_some(),
            "single-slice JPEG must reassemble; old logic compared absolute sender deadline to receiver now and dropped everything"
        );
    }

    #[test]
    fn multi_slice_uses_header_budget_on_receiver_timeline() {
        let mut v = VideoReassembly::default();
        let t0 = 5_000_000_000u64;
        let mut h0 = slice_hdr(1, 0, 2, t0, t0 + 200_000_000);
        h0.seq = 1;
        assert!(v.ingest(&h0, b"a", t0).is_none());
        let mut h1 = slice_hdr(1, 1, 2, t0, t0 + 200_000_000);
        h1.seq = 2;
        let out = v.ingest(&h1, b"b", t0 + 50_000_000).unwrap();
        assert_eq!(out, b"ab");
    }

    #[test]
    fn multi_slice_slice_zero_arrives_last_after_reorder_still_completes() {
        let mut v = VideoReassembly::default();
        let t0 = 1_000_000_000u64;
        let mut h1 = slice_hdr(9, 1, 2, t0, t0 + 200_000_000);
        h1.seq = 2;
        assert!(v.ingest(&h1, b"b", t0).is_none());
        // Slice 0 arrives after the header's 200ms budget from t0 alone; fixed-from-first-slice
        // logic would expire before slice 0 and never assemble.
        let late = t0 + 250_000_000;
        let mut h0 = slice_hdr(9, 0, 2, t0, t0 + 200_000_000);
        h0.seq = 1;
        let out = v.ingest(&h0, b"a", late).unwrap();
        assert_eq!(out, b"ab");
    }
}

/// Route parsed packets: control/haptic → reorder; video → reassembly; telemetry → passthrough.
pub struct ReceiveDemux {
    pub dedup: DedupTable,
    pub control_reorder: HashMap<u16, StreamReorder>,
    pub video: VideoReassembly,
    pub max_reorder: u32,
}

impl ReceiveDemux {
    pub fn new(dedup_cap: usize, max_reorder: u32) -> Self {
        Self {
            dedup: DedupTable::new(dedup_cap),
            control_reorder: HashMap::new(),
            video: VideoReassembly::default(),
            max_reorder,
        }
    }

    pub fn handle_datagram(
        &mut self,
        header: LtpHeader,
        payload: Vec<u8>,
        now_ns: u64,
        duplicate_mode: bool,
    ) -> ReceivedEvent {
        if duplicate_mode {
            if !self
                .dedup
                .first_arrival(header.stream_id, header.seq)
            {
                return ReceivedEvent::Duplicate;
            }
        }

        match (header.priority, header.payload_type) {
            (PriorityClass::Control | PriorityClass::Haptic, PayloadType::Control | PayloadType::Haptic) => {
                let ro = self
                    .control_reorder
                    .entry(header.stream_id)
                    .or_insert_with(|| StreamReorder::new(self.max_reorder));
                let ordered = ro.push(header.seq, payload);
                ReceivedEvent::ControlOrdered(ordered)
            }
            (_, PayloadType::VideoSlice) => {
                let frame = self.video.ingest(&header, &payload, now_ns);
                ReceivedEvent::VideoProgress { frame }
            }
            _ => ReceivedEvent::Telemetry(payload),
        }
    }
}

pub enum ReceivedEvent {
    Duplicate,
    ControlOrdered(Vec<Vec<u8>>),
    VideoProgress { frame: Option<Vec<u8>> },
    Telemetry(Vec<u8>),
}
