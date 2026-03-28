//! Build on-wire datagrams from `LtpHeader` + payload.

use crate::header::LtpHeader;

pub fn frame_datagram(header: &LtpHeader, payload: &[u8]) -> Result<Vec<u8>, &'static str> {
    if payload.len() != header.payload_len as usize {
        return Err("payload_len mismatch");
    }
    let mut out = Vec::new();
    header.write_into(&mut out);
    out.extend_from_slice(payload);
    Ok(out)
}

pub fn split_header_payload(datagram: &[u8]) -> Result<(LtpHeader, &[u8]), &'static str> {
    let (h, hdr_len) = LtpHeader::parse(datagram)?;
    h.validate_total_len(datagram.len())?;
    let end = hdr_len + h.payload_len as usize;
    Ok((h, &datagram[hdr_len..end]))
}
