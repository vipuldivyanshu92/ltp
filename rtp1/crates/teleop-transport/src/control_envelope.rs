//! Control payload envelope: `control_schema_id` + opaque bytes after LTP header.

use std::io::{Read, Write};

/// Well-known schema IDs (integrators MAY use experimental range 0xE000..).
pub const SCHEMA_TWIST: u16 = 1;
pub const SCHEMA_JOINT_DELTA: u16 = 2;
pub const SCHEMA_GAMEPAD: u16 = 3;
pub const SCHEMA_EXPERIMENTAL_BASE: u16 = 0xE000;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ControlEnvelope {
    pub schema_id: u16,
    pub payload: Vec<u8>,
}

impl ControlEnvelope {
    pub const PREFIX_LEN: usize = 2 + 2; // schema_id BE + inner_len BE

    pub fn encode(&self) -> Vec<u8> {
        let mut v = Vec::with_capacity(Self::PREFIX_LEN + self.payload.len());
        v.extend_from_slice(&self.schema_id.to_be_bytes());
        v.extend_from_slice(&(self.payload.len() as u16).to_be_bytes());
        v.extend_from_slice(&self.payload);
        v
    }

    pub fn decode(mut bytes: &[u8]) -> Result<Self, &'static str> {
        if bytes.len() < Self::PREFIX_LEN {
            return Err("control envelope too short");
        }
        let schema_id = u16::from_be_bytes([bytes[0], bytes[1]]);
        let inner_len = u16::from_be_bytes([bytes[2], bytes[3]]) as usize;
        bytes = &bytes[4..];
        if bytes.len() < inner_len {
            return Err("control inner_len exceeds buffer");
        }
        Ok(Self {
            schema_id,
            payload: bytes[..inner_len].to_vec(),
        })
    }

    pub fn write_into<W: Write>(&self, w: &mut W) -> std::io::Result<()> {
        w.write_all(&self.schema_id.to_be_bytes())?;
        w.write_all(&(self.payload.len() as u16).to_be_bytes())?;
        w.write_all(&self.payload)?;
        Ok(())
    }

    pub fn read_from<R: Read>(r: &mut R, max_inner: usize) -> std::io::Result<Self> {
        let mut hdr = [0u8; 4];
        r.read_exact(&mut hdr)?;
        let schema_id = u16::from_be_bytes([hdr[0], hdr[1]]);
        let inner_len = u16::from_be_bytes([hdr[2], hdr[3]]) as usize;
        if inner_len > max_inner {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "inner_len too large",
            ));
        }
        let mut payload = vec![0u8; inner_len];
        r.read_exact(&mut payload)?;
        Ok(Self { schema_id, payload })
    }
}

pub fn schema_name(id: u16) -> &'static str {
    match id {
        SCHEMA_TWIST => "twist",
        SCHEMA_JOINT_DELTA => "joint_delta",
        SCHEMA_GAMEPAD => "gamepad",
        _ if id >= SCHEMA_EXPERIMENTAL_BASE => "experimental",
        _ => "unknown",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip() {
        let e = ControlEnvelope {
            schema_id: SCHEMA_TWIST,
            payload: vec![9, 8, 7],
        };
        let b = e.encode();
        let d = ControlEnvelope::decode(&b).unwrap();
        assert_eq!(d, e);
    }
}
