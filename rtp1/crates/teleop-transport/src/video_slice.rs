//! MTU-aware slicing for video payloads (no IP fragmentation on hot path).

pub fn slice_payload(mtu: usize, header_len: usize, payload: &[u8]) -> Vec<Vec<u8>> {
    let max_chunk = mtu.saturating_sub(header_len);
    if max_chunk == 0 {
        return Vec::new();
    }
    payload.chunks(max_chunk).map(|c| c.to_vec()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chunks_respect_mtu() {
        let mtu = 50;
        let hdr = 32;
        // mtu 50 − hdr 32 ⇒ 18-byte chunks; 54 bytes ⇒ exactly 3 chunks.
        let p = vec![7u8; 54];
        let s = slice_payload(mtu, hdr, &p);
        assert_eq!(s.len(), 3);
        assert!(s.iter().all(|c| c.len() <= mtu - hdr));
    }
}
