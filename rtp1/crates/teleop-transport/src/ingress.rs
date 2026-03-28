//! Batched non-blocking poll over multiple UDP paths.

use std::collections::VecDeque;
use std::io;
use std::net::SocketAddr;

use crate::path::PathHandle;

#[derive(Debug)]
pub struct IngressDatagram {
    pub path_index: usize,
    pub src: SocketAddr,
    pub data: Vec<u8>,
}

pub struct IngressQueue {
    pub pending: VecDeque<IngressDatagram>,
}

impl Default for IngressQueue {
    fn default() -> Self {
        Self {
            pending: VecDeque::new(),
        }
    }
}

impl IngressQueue {
    /// Non-blocking poll: drain any ready datagrams from all paths into `pending`.
    pub fn poll_paths(&mut self, paths: &mut [PathHandle]) -> io::Result<()> {
        for (path_index, p) in paths.iter_mut().enumerate() {
            loop {
                match p.try_recv()? {
                    Some((n, src)) => {
                        let slice = &p.recv_slice()[..n];
                        self.pending.push_back(IngressDatagram {
                            path_index,
                            src,
                            data: slice.to_vec(),
                        });
                    }
                    None => break,
                }
            }
        }
        Ok(())
    }
}
