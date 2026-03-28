//! LTP1 fixed header (big-endian on wire). See `openspec/.../design.md`.

pub const LTP_MAGIC: u32 = 0x4C54_5031;
pub const LTP_VERSION: u8 = 1;
pub const FIXED_HEADER_LEN: usize = 32;
pub const MAX_HEADER_DWORDS: u8 = 15;
pub const MAX_EXTENSION_LEN: usize = (MAX_HEADER_DWORDS as usize) * 4 - FIXED_HEADER_LEN;

#[repr(u8)]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum PriorityClass {
    Control = 0,
    Haptic = 1,
    Video = 2,
    Telemetry = 3,
}

impl TryFrom<u8> for PriorityClass {
    type Error = ();

    fn try_from(v: u8) -> Result<Self, Self::Error> {
        match v {
            0 => Ok(Self::Control),
            1 => Ok(Self::Haptic),
            2 => Ok(Self::Video),
            3 => Ok(Self::Telemetry),
            _ => Err(()),
        }
    }
}

#[repr(u8)]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum PayloadType {
    Control = 0,
    Haptic = 1,
    VideoSlice = 2,
    Telemetry = 3,
    FecShard = 4,
    AckMeta = 5,
}

impl TryFrom<u8> for PayloadType {
    type Error = ();

    fn try_from(v: u8) -> Result<Self, Self::Error> {
        match v {
            0 => Ok(Self::Control),
            1 => Ok(Self::Haptic),
            2 => Ok(Self::VideoSlice),
            3 => Ok(Self::Telemetry),
            4 => Ok(Self::FecShard),
            5 => Ok(Self::AckMeta),
            _ => Err(()),
        }
    }
}

pub const FLAG_FEC: u8 = 1 << 0;
pub const FLAG_KEYFRAME: u8 = 1 << 1;
pub const FLAG_DUPLICATE_SEND: u8 = 1 << 2;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LtpHeader {
    pub version: u8,
    /// Total header size in 4-byte words (includes fixed + extension).
    pub hdr_len_dw: u8,
    pub priority: PriorityClass,
    pub payload_type: PayloadType,
    pub flags: u8,
    pub stream_id: u16,
    pub path_tag: u16,
    pub seq: u32,
    pub timestamp_ns: u64,
    pub frame_id: u16,
    pub slice_id: u8,
    pub slice_count: u8,
    pub payload_len: u16,
    pub fec_group_id: u8,
    pub fec_index: u8,
    pub extension: Vec<u8>,
}

impl Default for LtpHeader {
    fn default() -> Self {
        Self {
            version: LTP_VERSION,
            hdr_len_dw: 8,
            priority: PriorityClass::Control,
            payload_type: PayloadType::Control,
            flags: 0,
            stream_id: 0,
            path_tag: 0,
            seq: 0,
            timestamp_ns: 0,
            frame_id: 0,
            slice_id: 0,
            slice_count: 0,
            payload_len: 0,
            fec_group_id: 0,
            fec_index: 0,
            extension: Vec::new(),
        }
    }
}

impl LtpHeader {
    pub fn header_bytes_len(&self) -> usize {
        self.hdr_len_dw as usize * 4
    }

    pub fn extension_len(&self) -> usize {
        self.header_bytes_len().saturating_sub(FIXED_HEADER_LEN)
    }

    fn sync_hdr_len_dw(&mut self) {
        let ext = self.extension.len();
        let total = FIXED_HEADER_LEN + ext;
        let dw = ((total + 3) / 4).min(MAX_HEADER_DWORDS as usize);
        self.hdr_len_dw = dw as u8;
        let padded = self.hdr_len_dw as usize * 4;
        if padded > FIXED_HEADER_LEN + ext {
            self.extension.resize(padded - FIXED_HEADER_LEN, 0);
        }
    }

    pub fn write_into(&self, out: &mut Vec<u8>) {
        let mut h = self.clone();
        h.sync_hdr_len_dw();
        let hdr_len = h.header_bytes_len();
        out.clear();
        out.reserve(hdr_len + h.payload_len as usize);
        out.extend_from_slice(&h.ltp_magic_be());
        let b4 = ((h.version & 0x0F) << 4) | (h.hdr_len_dw & 0x0F);
        out.push(b4);
        out.push(h.priority as u8);
        out.push(h.payload_type as u8);
        out.push(h.flags);
        out.extend_from_slice(&h.stream_id.to_be_bytes());
        out.extend_from_slice(&h.path_tag.to_be_bytes());
        out.extend_from_slice(&h.seq.to_be_bytes());
        out.extend_from_slice(&h.timestamp_ns.to_be_bytes());
        out.extend_from_slice(&h.frame_id.to_be_bytes());
        out.push(h.slice_id);
        out.push(h.slice_count);
        out.extend_from_slice(&h.payload_len.to_be_bytes());
        out.push(h.fec_group_id);
        out.push(h.fec_index);
        debug_assert_eq!(out.len(), FIXED_HEADER_LEN);
        if h.extension_len() > 0 {
            out.extend_from_slice(&h.extension);
        }
        debug_assert_eq!(out.len(), hdr_len);
    }

    fn ltp_magic_be(&self) -> [u8; 4] {
        LTP_MAGIC.to_be_bytes()
    }

    /// Parse header from the start of `buf`. Returns `(header, total_header_len)` or error message.
    pub fn parse(buf: &[u8]) -> Result<(Self, usize), &'static str> {
        if buf.len() < FIXED_HEADER_LEN {
            return Err("buffer too small");
        }
        let magic = u32::from_be_bytes(buf[0..4].try_into().unwrap());
        if magic != LTP_MAGIC {
            return Err("bad magic");
        }
        let b4 = buf[4];
        let version = (b4 >> 4) & 0x0F;
        if version != LTP_VERSION {
            return Err("unsupported version");
        }
        let hdr_len_dw = b4 & 0x0F;
        if hdr_len_dw < 8 {
            return Err("hdr_len_dw too small");
        }
        let total_hdr = hdr_len_dw as usize * 4;
        if buf.len() < total_hdr {
            return Err("incomplete header");
        }
        let priority = PriorityClass::try_from(buf[5]).map_err(|_| "bad priority")?;
        let payload_type = PayloadType::try_from(buf[6]).map_err(|_| "bad payload_type")?;
        let flags = buf[7];
        let stream_id = u16::from_be_bytes(buf[8..10].try_into().unwrap());
        let path_tag = u16::from_be_bytes(buf[10..12].try_into().unwrap());
        let seq = u32::from_be_bytes(buf[12..16].try_into().unwrap());
        let timestamp_ns = u64::from_be_bytes(buf[16..24].try_into().unwrap());
        let frame_id = u16::from_be_bytes(buf[24..26].try_into().unwrap());
        let slice_id = buf[26];
        let slice_count = buf[27];
        let payload_len = u16::from_be_bytes(buf[28..30].try_into().unwrap());
        let fec_group_id = buf[30];
        let fec_index = buf[31];
        let ext_len = total_hdr.saturating_sub(FIXED_HEADER_LEN);
        let extension = if ext_len > 0 {
            buf[FIXED_HEADER_LEN..FIXED_HEADER_LEN + ext_len].to_vec()
        } else {
            Vec::new()
        };
        Ok((
            Self {
                version,
                hdr_len_dw,
                priority,
                payload_type,
                flags,
                stream_id,
                path_tag,
                seq,
                timestamp_ns,
                frame_id,
                slice_id,
                slice_count,
                payload_len,
                fec_group_id,
                fec_index,
                extension,
            },
            total_hdr,
        ))
    }

    pub fn validate_total_len(&self, datagram_len: usize) -> Result<(), &'static str> {
        let hdr = self.header_bytes_len();
        let need = hdr.saturating_add(self.payload_len as usize);
        if datagram_len < need {
            return Err("datagram shorter than header+payload_len");
        }
        Ok(())
    }

    /// Frame/slice deadline (ns); v1 stores in first 8 bytes of extension when present.
    pub fn deadline_ns(&self) -> u64 {
        if self.extension.len() >= 8 {
            u64::from_be_bytes(self.extension[0..8].try_into().unwrap())
        } else {
            0
        }
    }

    pub fn set_deadline_extension(&mut self, deadline_ns: u64) {
        self.extension.resize(8, 0);
        self.extension[0..8].copy_from_slice(&deadline_ns.to_be_bytes());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fixed_header_repr_size() {
        assert_eq!(FIXED_HEADER_LEN, 32);
    }

    #[test]
    fn round_trip_no_extension() {
        let mut h = LtpHeader::default();
        h.seq = 0x01020304;
        h.stream_id = 0xAABB;
        h.path_tag = 0x1122;
        h.payload_len = 4;
        h.timestamp_ns = 99;
        let mut w = Vec::new();
        h.write_into(&mut w);
        assert_eq!(w.len(), FIXED_HEADER_LEN);
        let (p, n) = LtpHeader::parse(&w).unwrap();
        assert_eq!(n, FIXED_HEADER_LEN);
        assert_eq!(p.seq, h.seq);
        assert_eq!(p.stream_id, h.stream_id);
        assert_eq!(p.path_tag, h.path_tag);
        assert_eq!(p.payload_len, 4);
    }

    #[test]
    fn round_trip_with_extension() {
        let mut h = LtpHeader::default();
        h.extension = vec![1, 2, 3, 4];
        let mut w = Vec::new();
        h.write_into(&mut w);
        let (p, n) = LtpHeader::parse(&w).unwrap();
        assert_eq!(n, w.len());
        assert_eq!(p.extension, vec![1, 2, 3, 4]);
    }
}
