//! Lock-free-style SPSC ring layout for control + compressed frames (hot path; DDS bypass).
//!
//! ## On-disk / mmap layout (little-endian host for v0 reference; embedders SHOULD use same endian per platform doc)
//!
//! Offset 0: `magic` u32 = `0x4C545052` (`LTPR` ring)
//! Offset 4: `version` u32
//! Offset 8: `capacity` u32 (power of two, number of fixed-size slots)
//! Offset 12: `stride` u32 (max slot size, padded)
//! Offset 16: `head` u32 (producer)
//! Offset 20: `tail` u32 (consumer)
//! Offset 24: reserved
//! Offset 32: ring slots begin

pub const SHMEM_RING_MAGIC: u32 = 0x4C54_5052;

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct ShmemRingHeader {
    pub magic: u32,
    pub version: u32,
    pub capacity: u32,
    pub stride: u32,
    pub head: u32,
    pub tail: u32,
    pub _reserved: u32,
}

impl ShmemRingHeader {
    pub const LAYOUT_PREFIX: usize = 32;

    pub fn new(capacity: u32, stride: u32) -> Self {
        Self {
            magic: SHMEM_RING_MAGIC,
            version: 1,
            capacity,
            stride,
            head: 0,
            tail: 0,
            _reserved: 0,
        }
    }
}

/// In-process ring with same slot semantics as the documented mmap layout (for tests and single-process bridges).
pub struct ShmemRing {
    pub hdr: ShmemRingHeader,
    data: Vec<u8>,
}

impl ShmemRing {
    pub fn new(capacity: u32, stride: u32) -> Option<Self> {
        if !capacity.is_power_of_two() || stride == 0 {
            return None;
        }
        let cap = capacity as usize;
        let st = stride as usize;
        let data = vec![0u8; cap * st];
        Some(Self {
            hdr: ShmemRingHeader::new(capacity, stride),
            data,
        })
    }

    fn mask(&self) -> u32 {
        self.hdr.capacity - 1
    }

    /// Returns false if full.
    pub fn try_push(&mut self, slot: &[u8]) -> bool {
        if slot.len() > self.hdr.stride as usize {
            return false;
        }
        let cap = self.hdr.capacity;
        let head = self.hdr.head;
        let tail = self.hdr.tail;
        if head.wrapping_sub(tail) >= cap {
            return false;
        }
        let idx = (head & self.mask()) as usize;
        let off = idx * self.hdr.stride as usize;
        let dst = &mut self.data[off..off + self.hdr.stride as usize];
        dst.fill(0);
        dst[..slot.len()].copy_from_slice(slot);
        self.hdr.head = head.wrapping_add(1);
        true
    }

    pub fn try_pop(&mut self, out: &mut [u8]) -> Option<usize> {
        if self.hdr.head == self.hdr.tail {
            return None;
        }
        let idx = (self.hdr.tail & self.mask()) as usize;
        let off = idx * self.hdr.stride as usize;
        let src = &self.data[off..off + self.hdr.stride as usize];
        let len = src.iter().rposition(|&b| b != 0).map(|i| i + 1).unwrap_or(0);
        let n = len.min(out.len());
        out[..n].copy_from_slice(&src[..n]);
        self.hdr.tail = self.hdr.tail.wrapping_add(1);
        Some(n)
    }
}
