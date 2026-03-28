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
    deadline_ns: u64,
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
        if h.deadline_ns() > 0 {
            entry.deadline_ns = h.deadline_ns();
        }
        if now_ns > entry.deadline_ns && entry.deadline_ns > 0 {
            self.frames.remove(&frame_id);
            return None;
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

    pub fn sweep_stale(&mut self, now_ns: u64) {
        let stale: Vec<u16> = self
            .frames
            .iter()
            .filter(|(_, f)| f.deadline_ns > 0 && now_ns > f.deadline_ns)
            .map(|(&k, _)| k)
            .collect();
        for k in stale {
            self.frames.remove(&k);
        }
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
