//! XOR-based FEC micro-batches (no NACK). Video groups keyed by `fec_group_id` / `fec_index`.

use std::collections::HashMap;

fn xor_bytes(a: &[u8], b: &[u8]) -> Vec<u8> {
    a.iter().zip(b.iter()).map(|(x, y)| x ^ y).collect()
}

fn xor_many(chunks: &[Vec<u8>]) -> Option<Vec<u8>> {
    if chunks.is_empty() {
        return None;
    }
    let len = chunks[0].len();
    if !chunks.iter().all(|c| c.len() == len) {
        return None;
    }
    let mut acc = vec![0u8; len];
    for c in chunks {
        for (i, &b) in c.iter().enumerate() {
            acc[i] ^= b;
        }
    }
    Some(acc)
}

/// Build one parity packet as XOR of all data packets (same length).
pub fn xor_parity(data: &[Vec<u8>]) -> Option<Vec<u8>> {
    xor_many(data)
}

/// Recover single missing row if parity is XOR of all k data blocks.
pub fn xor_recover_one_missing(mut blocks: Vec<Option<Vec<u8>>>, parity: &[u8]) -> Option<Vec<Vec<u8>>> {
    let k = blocks.len();
    if k == 0 {
        return None;
    }
    let missing_count = blocks.iter().filter(|b| b.is_none()).count();
    if missing_count != 1 {
        return None;
    }
    let len = parity.len();
    let mut known_xor = vec![0u8; len];
    for b in &blocks {
        if let Some(v) = b {
            if v.len() != len {
                return None;
            }
            for i in 0..len {
                known_xor[i] ^= v[i];
            }
        }
    }
    let recovered: Vec<u8> = known_xor.iter().zip(parity).map(|(a, p)| a ^ p).collect();
    for slot in &mut blocks {
        if slot.is_none() {
            *slot = Some(recovered.clone());
            break;
        }
    }
    Some(blocks.into_iter().map(|b| b.unwrap()).collect())
}

/// Collect video FEC shards for one `fec_group_id`; when `k` data shards present, concatenate in index order.
#[derive(Default)]
pub struct VideoFecGroup {
    pub k: u8,
    pub shards: HashMap<u8, Vec<u8>>,
}

impl VideoFecGroup {
    pub fn ingest(&mut self, fec_index: u8, payload: Vec<u8>, k: u8) -> Option<Vec<u8>> {
        if self.k == 0 {
            self.k = k;
        }
        self.shards.insert(fec_index, payload);
        if (0..self.k).all(|i| self.shards.contains_key(&i)) {
            let mut out = Vec::new();
            for i in 0..self.k {
                out.extend_from_slice(self.shards.get(&i)?);
            }
            Some(out)
        } else {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn xor_recover() {
        let a = vec![1u8, 2, 3];
        let b = vec![4u8, 5, 6];
        let p = xor_parity(&[a.clone(), b.clone()]).unwrap();
        let out = xor_recover_one_missing(vec![Some(a.clone()), None], &p).unwrap();
        assert_eq!(out[1], b);
    }
}
