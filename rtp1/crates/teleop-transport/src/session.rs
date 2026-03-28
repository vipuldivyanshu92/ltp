//! High-level session: paths, scheduler, bonding send, ingress + demux.

use std::io;
use std::net::SocketAddr;

use crate::bonding::{fast_switch_path, mode_for, pick_split_path, BondingPolicy, SendMode};
use crate::framer::{frame_datagram, split_header_payload};
use crate::header::{LtpHeader, PayloadType, PriorityClass, FLAG_DUPLICATE_SEND};
use crate::ingress::{IngressQueue, IngressDatagram};
use crate::path::{PathConfig, PathHandle};
use crate::receive::ReceiveDemux;
use crate::scheduler::{PriorityScheduler, ScheduledPacket};

pub struct SessionConfig {
    pub peer: SocketAddr,
    pub dedup_cap: usize,
    pub max_reorder: u32,
    pub bonding: BondingPolicy,
    pub mtu: usize,
}

impl Default for SessionConfig {
    fn default() -> Self {
        Self {
            peer: "127.0.0.1:5000".parse().unwrap(),
            dedup_cap: 4096,
            max_reorder: 32,
            bonding: BondingPolicy::default(),
            mtu: 1200,
        }
    }
}

pub struct Session {
    pub cfg: SessionConfig,
    paths: Vec<PathHandle>,
    scheduler: PriorityScheduler,
    ingress: IngressQueue,
    demux: ReceiveDemux,
    baseline_rtt: Vec<f64>,
}

impl Session {
    pub fn new(cfg: SessionConfig, path_cfgs: Vec<PathConfig>) -> io::Result<Self> {
        let mut paths = Vec::with_capacity(path_cfgs.len());
        for c in path_cfgs {
            paths.push(PathHandle::open(c)?);
        }
        let n = paths.len();
        let dedup_cap = cfg.dedup_cap;
        let max_reorder = cfg.max_reorder;
        Ok(Self {
            cfg,
            paths,
            scheduler: PriorityScheduler::default(),
            ingress: IngressQueue::default(),
            demux: ReceiveDemux::new(dedup_cap, max_reorder),
            baseline_rtt: vec![10.0; n],
        })
    }

    pub fn paths_mut(&mut self) -> &mut [PathHandle] {
        &mut self.paths
    }

    pub fn enqueue_raw_datagram(&mut self, p: ScheduledPacket) {
        self.scheduler.enqueue(p);
    }

    pub fn flush_send(&mut self) -> io::Result<()> {
        self.scheduler.reset_telemetry_tick();
        while let Some(pkt) = self.scheduler.pop_next() {
            self.send_datagram_on_paths(&pkt)?;
        }
        Ok(())
    }

    fn send_datagram_on_paths(&mut self, pkt: &ScheduledPacket) -> io::Result<()> {
        let (h, pl) = split_header_payload(&pkt.datagram)
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "bad datagram"))?;
        let mode = mode_for(pkt.priority, pkt.payload_type);
        let rtt: Vec<f64> = self.paths.iter().map(|p| p.metrics.rtt_ewma_ms).collect();
        let loss: Vec<f64> = self.paths.iter().map(|p| p.metrics.loss_ewma).collect();
        match mode {
            SendMode::Duplicate => {
                for p in &self.paths {
                    let mut hdr = h.clone();
                    hdr.path_tag = p.path_tag();
                    hdr.flags |= FLAG_DUPLICATE_SEND;
                    let mut buf = Vec::new();
                    hdr.write_into(&mut buf);
                    buf.extend_from_slice(pl);
                    p.send_to(self.cfg.peer, &buf)?;
                }
            }
            SendMode::Split => {
                let idx = pick_split_path(&rtt);
                let idx = fast_switch_path(idx, &loss, &rtt, &self.baseline_rtt, &self.cfg.bonding);
                let p = &self.paths[idx];
                let mut hdr = h.clone();
                hdr.path_tag = p.path_tag();
                let mut buf = Vec::new();
                hdr.write_into(&mut buf);
                buf.extend_from_slice(pl);
                p.send_to(self.cfg.peer, &buf)?;
            }
        }
        Ok(())
    }

    pub fn poll_ingress(&mut self) -> io::Result<()> {
        self.ingress.poll_paths(&mut self.paths)
    }

    pub fn drain_ingress(
        &mut self,
        duplicate_rx: bool,
        now_ns: u64,
    ) -> Vec<crate::receive::ReceivedEvent> {
        let mut out = Vec::new();
        while let Some(IngressDatagram { data, .. }) = self.ingress.pending.pop_front() {
            if let Ok((h, pl)) = split_header_payload(&data) {
                let ev = self.demux.handle_datagram(h, pl.to_vec(), now_ns, duplicate_rx);
                out.push(ev);
            }
        }
        out
    }
}

/// Helper: frame control with envelope bytes inside payload.
pub fn build_control_datagram(
    base: &mut LtpHeader,
    control_payload: &[u8],
) -> Result<Vec<u8>, &'static str> {
    base.payload_len = control_payload.len() as u16;
    base.payload_type = PayloadType::Control;
    base.priority = PriorityClass::Control;
    frame_datagram(base, control_payload)
}
