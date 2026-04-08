//! Robot-site gateway: anvil_streamer (WS :9191) → LTP/UDP → cloud relay → Quest;
//! Quest control (LTP) → relay → this process → TCP :8081 (quest_teleop).
//!
//! Cloud relay: `python3 python/ltp_udp_relay.py --bind 0.0.0.0 --from-leader 6001 --from-follower 5001`
//! - This binary is the **leader**: `--ltp-peer <VPS>:6001` (send video here).
//! - Quest is the **follower**: sends control to `<VPS>:5001`.

use std::io::Write;
use std::net::{SocketAddr, TcpStream};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use clap::Parser;
use teleop_transport::control_envelope::{ControlEnvelope, SCHEMA_QUEST_TELEOP112};
use teleop_transport::framer::frame_datagram;
use teleop_transport::header::{LtpHeader, PayloadType, PriorityClass};
use teleop_transport::path::PathConfig;
use teleop_transport::receive::ReceivedEvent;
use teleop_transport::scheduler::ScheduledPacket;
use teleop_transport::session::Session;
use teleop_transport::session::SessionConfig;
use teleop_transport::video_slice;
use tracing::{error, info, warn};
use tungstenite::{connect, Message};
const QUEST_TELEOP112_LEN: usize = 112;
/// LTP fixed header 32 + 8-byte deadline extension (see `LtpHeader::set_deadline_extension`).
const VIDEO_HDR_BODY: usize = 40;

#[derive(Parser, Debug)]
#[command(name = "ltp-robot-gateway")]
struct Args {
    /// anvil_streamer WebSocket host
    #[arg(long, default_value = "127.0.0.1")]
    ws_host: String,

    /// anvil_streamer WebSocket port
    #[arg(long, default_value_t = 9191)]
    ws_port: u16,

    /// Local UDP bind address (port 0 = ephemeral)
    #[arg(long, default_value = "0.0.0.0:0")]
    ltp_bind: String,

    /// Cloud relay **leader** port (robot → relay). Matches `--from-leader` on `ltp_udp_relay.py`.
    #[arg(long)]
    ltp_peer: SocketAddr,

    /// quest_teleop TCP address (112-byte `TelemetryPacket` frames)
    #[arg(long, default_value = "127.0.0.1:8081")]
    teleop_tcp: SocketAddr,

    /// LTP datagram MTU budget (payload slicing)
    #[arg(long, default_value_t = 1200)]
    ltp_mtu: usize,

    /// Camera ROS compressed topics (must be 4, same order as Quest panels)
    #[arg(
        long,
        num_args = 4,
        default_values = [
            "/cam_chest/image_raw/compressed",
            "/cam_waist/image_raw/compressed",
            "/cam_wrist_l/image_raw/compressed",
            "/cam_wrist_r/image_raw/compressed",
        ]
    )]
    topics: Vec<String>,
}

fn now_ns() -> u64 {
    use std::time::SystemTime;
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0)
}

/// Per-camera JPEG assembly (matches Quest `websocket_video_receiver.cpp` SOI..EOI logic).
struct JpegAssembler {
    buf: Vec<u8>,
}

impl JpegAssembler {
    fn new() -> Self {
        Self { buf: Vec::new() }
    }

    fn push_chunk(&mut self, chunk: &[u8], out: &mut Vec<Vec<u8>>) {
        const MAX: usize = 32 * 1024 * 1024;
        let starts_soi = chunk.len() >= 2 && chunk[0] == 0xff && chunk[1] == 0xd8;
        let ends_eoi =
            chunk.len() >= 2 && chunk[chunk.len() - 2] == 0xff && chunk[chunk.len() - 1] == 0xd9;

        if starts_soi && !self.buf.is_empty() {
            if !self.buf.ends_with(&[0xff, 0xd9]) {
                tracing::trace!("dropped incomplete JPEG buffer {} bytes (new SOI)", self.buf.len());
            }
            self.buf.clear();
        }

        if self.buf.is_empty() && !starts_soi {
            tracing::trace!("binary chunk without SOI while buffer empty; drop");
            return;
        }

        if self.buf.len() + chunk.len() > MAX {
            tracing::warn!("WS assembly overflow; clearing");
            self.buf.clear();
            return;
        }

        self.buf.extend_from_slice(chunk);
        if ends_eoi && self.buf.len() >= 2 && self.buf[self.buf.len() - 2] == 0xff && self.buf[self.buf.len() - 1] == 0xd9
        {
            out.push(std::mem::take(&mut self.buf));
        }
    }
}

fn ws_camera_thread(
    ws_host: String,
    ws_port: u16,
    topic: String,
    cam_index: u8,
    tx: Sender<(u8, Vec<u8>)>,
) {
    let url = format!("ws://{ws_host}:{ws_port}/");
    let mut backoff = Duration::from_millis(300);
    loop {
        let conn = connect(&url);
        let (mut ws, _) = match conn {
            Ok(c) => c,
            Err(e) => {
                tracing::debug!("ws connect {url} failed: {e}");
                std::thread::sleep(backoff);
                backoff = (backoff * 2).min(Duration::from_secs(5));
                continue;
            }
        };
        backoff = Duration::from_millis(300);

        if let Err(e) = ws.send(Message::Text(topic.clone())) {
            error!("ws send subscribe {topic}: {e}");
            let _ = ws.close(None);
            std::thread::sleep(Duration::from_millis(500));
            continue;
        }

        info!("WS subscribed cam{cam_index} {topic}");
        let mut asm = JpegAssembler::new();
        loop {
            match ws.read() {
                Ok(Message::Binary(data)) => {
                    let mut completed = Vec::new();
                    asm.push_chunk(&data, &mut completed);
                    for jpeg in completed {
                        if tx.send((cam_index, jpeg)).is_err() {
                            return;
                        }
                    }
                }
                Ok(Message::Text(_)) => {}
                Ok(Message::Ping(p)) => {
                    let _ = ws.send(Message::Pong(p));
                }
                Ok(Message::Pong(_)) | Ok(Message::Frame(_)) => {}
                Ok(Message::Close(_)) => break,
                Err(e) => {
                    warn!("ws read cam{cam_index}: {e}");
                    break;
                }
            }
        }
        let _ = ws.close(None);
        info!("WS cam{cam_index} disconnected; retry...");
        std::thread::sleep(Duration::from_millis(500));
    }
}

fn send_jpeg_ltp(
    session: &mut Session,
    stream_id: u16,
    seq: &mut u32,
    frame_id: u16,
    jpeg: &[u8],
    capture_ns: u64,
) -> Result<()> {
    let chunks = video_slice::slice_payload(session.cfg.mtu, VIDEO_HDR_BODY, jpeg);
    let n = chunks.len();
    if n > 255 {
        bail!(
            "JPEG {} bytes needs {} slices; max 255 for LTP v1 slice_count (reduce resolution or raise MTU)",
            jpeg.len(),
            n
        );
    }
    if n == 0 {
        return Ok(());
    }
    let slice_count = n as u8;
    let deadline_ns = capture_ns.saturating_add(100_000_000);

    for (i, chunk) in chunks.iter().enumerate() {
        *seq = seq.wrapping_add(1);
        let mut hdr = LtpHeader::default();
        hdr.stream_id = stream_id;
        hdr.seq = *seq;
        hdr.timestamp_ns = capture_ns;
        hdr.frame_id = frame_id;
        hdr.slice_id = i as u8;
        hdr.slice_count = slice_count;
        hdr.payload_len = chunk.len() as u16;
        hdr.payload_type = PayloadType::VideoSlice;
        hdr.priority = PriorityClass::Video;
        hdr.set_deadline_extension(deadline_ns);
        let dg = frame_datagram(&hdr, chunk).map_err(|e| anyhow::anyhow!(e))?;
        session.enqueue_raw_datagram(ScheduledPacket {
            priority: PriorityClass::Video,
            payload_type: PayloadType::VideoSlice,
            datagram: dg,
            deadline_key: deadline_ns,
        });
    }
    session.flush_send()?;
    Ok(())
}

fn open_teleop_stream(addr: SocketAddr) -> Result<TcpStream> {
    let s = TcpStream::connect(addr).context("teleop TcpStream::connect")?;
    s.set_nodelay(true)?;
    s.set_write_timeout(Some(Duration::from_secs(2)))?;
    Ok(s)
}

fn ltp_main_loop(
    mut session: Session,
    jpeg_rx: Receiver<(u8, Vec<u8>)>,
    teleop_addr: SocketAddr,
) -> Result<()> {
    let mut teleop = open_teleop_stream(teleop_addr)?;
    // Global LTP `frame_id` for all cameras (`ReceiveDemux` video map is keyed only by `frame_id`).
    let mut global_frame_id: u16 = 0;
    let mut seq: u32 = 0;

    loop {
        let t0 = Instant::now();
        session.poll_ingress()?;
        let now = now_ns();
        let events = session.drain_ingress(true, now);
        for ev in events {
            if let ReceivedEvent::ControlOrdered(chunks) = ev {
                for chunk in chunks {
                    if let Ok(env) = ControlEnvelope::decode(&chunk) {
                        if env.schema_id == SCHEMA_QUEST_TELEOP112
                            && env.payload.len() == QUEST_TELEOP112_LEN
                        {
                            if let Err(e) = teleop.write_all(&env.payload) {
                                warn!("teleop TCP write: {e}; reconnect");
                                loop {
                                    match open_teleop_stream(teleop_addr) {
                                        Ok(s) => {
                                            teleop = s;
                                            break;
                                        }
                                        Err(e2) => {
                                            error!("teleop reconnect failed: {e2}");
                                            std::thread::sleep(Duration::from_millis(200));
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }

        while let Ok((cam, jpeg)) = jpeg_rx.try_recv() {
            let cam = cam as usize;
            if cam >= 4 {
                continue;
            }
            global_frame_id = global_frame_id.wrapping_add(1);
            let sid = (cam as u16) + 1;
            let fid = global_frame_id;
            let cap = now_ns();
            if let Err(e) = send_jpeg_ltp(&mut session, sid, &mut seq, fid, &jpeg, cap) {
                warn!("send_jpeg_ltp cam{cam}: {e}");
            }
        }

        match jpeg_rx.recv_timeout(Duration::from_millis(2)) {
            Ok((cam, jpeg)) => {
                let cam = cam as usize;
                if cam < 4 {
                    global_frame_id = global_frame_id.wrapping_add(1);
                    let sid = (cam as u16) + 1;
                    let fid = global_frame_id;
                    let cap = now_ns();
                    if let Err(e) = send_jpeg_ltp(&mut session, sid, &mut seq, fid, &jpeg, cap) {
                        warn!("send_jpeg_ltp cam{cam}: {e}");
                    }
                }
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => return Ok(()),
        }

        // Avoid hot spin when idle
        let elapsed = t0.elapsed();
        if elapsed < Duration::from_millis(2) {
            std::thread::sleep(Duration::from_millis(2) - elapsed);
        }
    }
}

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let args = Args::parse();
    let bind: SocketAddr = args
        .ltp_bind
        .parse()
        .context("parse --ltp-bind")?;

    let path = PathConfig {
        path_index: 0,
        path_tag: 1,
        bind_addr: bind,
    };

    let mut sc = SessionConfig::default();
    sc.peer = args.ltp_peer;
    sc.mtu = args.ltp_mtu;

    let mut session = Session::new(sc, vec![path]).context("Session::new")?;
    let local_udp = session.paths_mut()[0].socket.local_addr()?;
    info!(
        "LTP bound {local_udp} peer {} mtu {} | teleop TCP {}",
        args.ltp_peer,
        args.ltp_mtu,
        args.teleop_tcp
    );

    let (tx, rx) = mpsc::channel::<(u8, Vec<u8>)>();
    for (i, topic) in args.topics.iter().enumerate() {
        let ws_host = args.ws_host.clone();
        let ws_port = args.ws_port;
        let topic = topic.clone();
        let txc = tx.clone();
        std::thread::spawn(move || {
            ws_camera_thread(ws_host, ws_port, topic, i as u8, txc);
        });
    }
    drop(tx);

    ltp_main_loop(session, rx, args.teleop_tcp)?;
    Ok(())
}
