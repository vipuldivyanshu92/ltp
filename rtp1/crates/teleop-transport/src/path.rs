//! UDP path handle: bound socket, path id, tag, buffers.

use std::io;
use std::net::{SocketAddr, UdpSocket};

use crate::metrics::PathMetrics;

#[derive(Clone, Debug)]
pub struct PathConfig {
    pub path_index: usize,
    /// Encoded into `path_tag` high bits; low bits may carry generation.
    pub path_tag: u16,
    pub bind_addr: SocketAddr,
}

pub struct PathHandle {
    pub config: PathConfig,
    pub socket: UdpSocket,
    pub metrics: PathMetrics,
    recv_buf: Vec<u8>,
}

impl PathHandle {
    pub const DEFAULT_RECV_MTU: usize = 65535;

    pub fn open(config: PathConfig) -> io::Result<Self> {
        let socket = UdpSocket::bind(config.bind_addr)?;
        socket.set_nonblocking(true)?;
        Ok(Self {
            config,
            socket,
            metrics: PathMetrics::default(),
            recv_buf: vec![0u8; Self::DEFAULT_RECV_MTU],
        })
    }

    pub fn path_tag(&self) -> u16 {
        self.config.path_tag
    }

    pub fn try_recv(&mut self) -> io::Result<Option<(usize, SocketAddr)>> {
        match self.socket.recv_from(&mut self.recv_buf) {
            Ok((n, src)) => Ok(Some((n, src))),
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => Ok(None),
            Err(e) => Err(e),
        }
    }

    pub fn recv_slice(&self) -> &[u8] {
        &self.recv_buf
    }

    pub fn send_to(&self, addr: SocketAddr, pkt: &[u8]) -> io::Result<usize> {
        self.socket.send_to(pkt, addr)
    }
}
