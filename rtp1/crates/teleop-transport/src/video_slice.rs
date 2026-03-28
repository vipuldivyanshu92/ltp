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
        let p = vec![7u8; 100];
        let s = slice_payload(mtu, hdr, &p);
        assert_eq!(s.len(), 3);
        assert!(s.iter().all(|c| c.len() <= mtu - hdr));
    }
}
