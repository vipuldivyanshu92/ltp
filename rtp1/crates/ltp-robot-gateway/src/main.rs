//! Robot-site gateway: anvil_streamer (WS :9191) → LTP/UDP → cloud relay → Quest;
//! Quest control (LTP) → relay → this process → TCP **127.0.0.1:8081** (`--teleop-tcp`, `quest_teleop` listener).
//! Video/LTP egress does **not** wait for teleop TCP so the UDP relay can learn the leader while Quest is still booting.
//!
//! Cloud relay (fixed ports): `python3 python/ltp_udp_relay.py --bind 0.0.0.0 --from-leader 6001 --from-follower 5001`
//! - **Port 6001** — robot / this binary (LTP **leader**): send video to the relay; control from Quest is forwarded back here. Use `--ltp-peer <VPS_PUBLIC_IP>:6001`.
//! - **Port 5001** — Quest (LTP **follower**): receives video from the relay; sends controls to the relay. Configure the Quest LTP client with `<VPS_PUBLIC_IP>:5001` as its UDP peer (not the robot’s public IP).

use std::io::Write;
use std::net::{IpAddr, SocketAddr, TcpStream};
use std::sync::mpsc::{self, Receiver, Sender};
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

    /// Cloud relay port **6001** (LTP leader leg: robot → relay). Must match `--from-leader 6001` on `ltp_udp_relay.py`.
    #[arg(long)]
    ltp_peer: SocketAddr,

    /// quest_teleop TCP **connect** target (must be a real host, not `0.0.0.0` / `::`).
    /// Default: local listener on 8081.
    #[arg(long, default_value = "127.0.0.1:8081")]
    teleop_tcp: SocketAddr,

    /// LTP datagram MTU budget (payload slicing). Larger values reduce datagram count per JPEG
    /// (easier on WAN) until path MTU causes IP fragmentation or loss; LTP v1 allows at most
    /// 255 slices per frame. If `pending_send` in logs stays high, lower camera JPEG quality or
    /// raise this cautiously (rough max JPEG ≈ `(mtu - 40) * 255`).
    #[arg(long, default_value_t = 1380)]
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

/// JPEG with the wall-clock timestamp of when it was fully assembled in the WS
/// thread. The main loop uses this to measure channel queuing delay.
struct TimestampedJpeg {
    cam: u8,
    jpeg: Vec<u8>,
    assembled_ns: u64,
}

fn ws_camera_thread(
    ws_host: String,
    ws_port: u16,
    topic: String,
    cam_index: u8,
    tx: Sender<TimestampedJpeg>,
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
                        let msg = TimestampedJpeg {
                            cam: cam_index,
                            jpeg,
                            assembled_ns: now_ns(),
                        };
                        if tx.send(msg).is_err() {
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

/// Returns number of LTP datagrams (slices) queued on success.
fn send_jpeg_ltp(
    session: &mut Session,
    stream_id: u16,
    seq: &mut u32,
    frame_id: u16,
    jpeg: &[u8],
    capture_ns: u64,
) -> Result<usize> {
    let chunks = video_slice::slice_payload(session.cfg.mtu, VIDEO_HDR_BODY, jpeg);
    let n = chunks.len();
    if n > 255 {
        let hdr = VIDEO_HDR_BODY;
        let min_mtu = (jpeg.len() + 254) / 255 + hdr;
        bail!(
            "JPEG {} bytes needs {} slices; max 255 for LTP v1 slice_count (reduce resolution or raise --ltp-mtu to at least ~{})",
            jpeg.len(),
            n,
            min_mtu
        );
    }
    if n == 0 {
        return Ok(0);
    }
    let slice_count = n as u8;
    let deadline_ns = capture_ns.saturating_add(500_000_000);

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
    session.flush_send(capture_ns)?;
    Ok(n)
}

fn open_teleop_stream(addr: SocketAddr) -> Result<TcpStream> {
    let s = TcpStream::connect(addr).context("teleop TcpStream::connect")?;
    s.set_nodelay(true)?;
    s.set_write_timeout(Some(Duration::from_secs(2)))?;
    Ok(s)
}

fn validate_teleop_tcp(addr: SocketAddr) -> Result<()> {
    match addr.ip() {
        IpAddr::V4(v4) if v4.is_unspecified() => {
            bail!(
                "--teleop-tcp must not use 0.0.0.0 (bind wildcard, not a connect target); use 127.0.0.1:8081 for local quest_teleop"
            );
        }
        IpAddr::V6(v6) if v6.is_unspecified() => {
            bail!("--teleop-tcp must not use :: (unspecified); use [::1]:8081 for local quest_teleop");
        }
        _ => Ok(()),
    }
}

const BACKPRESSURE_THRESHOLD: usize = 512;

#[derive(Default)]
struct GatewayStats {
    jpeg_frames: [u64; 4],
    jpeg_bytes: [u64; 4],
    ltp_slices_encoded: u64,
    control_to_teleop: u64,
    control_dropped_no_tcp: u64,
    ingress_dup: u64,
    ingress_video: u64,
    ingress_telem: u64,
    jpegs_skipped_backpressure: u64,
    last_emit: Option<Instant>,
    last_pending_send: usize,
    max_pending_send: usize,
    // Latency accumulators (reset each stats interval)
    lat_channel_us_sum: u64,
    lat_encode_us_sum: u64,
    lat_poll_us_sum: u64,
    lat_flush_us_sum: u64,
    lat_drain_us_sum: u64,
    lat_sample_count: u64,
    lat_loop_count: u64,
}

impl GatewayStats {
    fn maybe_log(&mut self, now: Instant, video_dropped: u64) {
        const INTERVAL: Duration = Duration::from_secs(5);
        let frames_sum: u64 = self.jpeg_frames.iter().sum();
        if frames_sum == 0
            && self.ltp_slices_encoded == 0
            && self.ingress_dup + self.ingress_video + self.ingress_telem == 0
        {
            return;
        }
        let should_emit = match self.last_emit {
            None => true,
            Some(t) => now.duration_since(t) >= INTERVAL,
        };
        if !should_emit {
            return;
        }
        self.last_emit = Some(now);
        let n = self.lat_sample_count.max(1);
        let loops = self.lat_loop_count.max(1);
        info!(
            "gateway: JPEG/cam {:?} | pending {}/{} dropped {} bp_skip {} | ctrl→TCP {} | dup {} vid {} tel {}",
            self.jpeg_frames,
            self.last_pending_send,
            self.max_pending_send,
            video_dropped,
            self.jpegs_skipped_backpressure,
            self.control_to_teleop,
            self.ingress_dup,
            self.ingress_video,
            self.ingress_telem,
        );
        info!(
            "latency(avg µs): chan_queue={} encode={} | loop_phases: poll={} flush={} drain={} ({} loops, {} jpegs)",
            self.lat_channel_us_sum / n,
            self.lat_encode_us_sum / n,
            self.lat_poll_us_sum / loops,
            self.lat_flush_us_sum / loops,
            self.lat_drain_us_sum / loops,
            loops,
            n,
        );
        self.lat_channel_us_sum = 0;
        self.lat_encode_us_sum = 0;
        self.lat_poll_us_sum = 0;
        self.lat_flush_us_sum = 0;
        self.lat_drain_us_sum = 0;
        self.lat_sample_count = 0;
        self.lat_loop_count = 0;
    }
}

fn ltp_main_loop(
    mut session: Session,
    jpeg_rx: Receiver<TimestampedJpeg>,
    teleop_addr: SocketAddr,
) -> Result<()> {
    let mut teleop: Option<TcpStream> = None;
    let mut teleop_backoff = Duration::from_millis(200);
    let mut next_teleop_try = Instant::now();
    let mut next_teleop_warn = Instant::now();

    let mut stats = GatewayStats::default();
    let mut global_frame_id: u16 = 0;
    let mut seq: u32 = 0;

    loop {
        let t0 = Instant::now();

        if teleop.is_none() && t0 >= next_teleop_try {
            match open_teleop_stream(teleop_addr) {
                Ok(s) => {
                    info!("teleop TCP connected to {teleop_addr}");
                    teleop = Some(s);
                    teleop_backoff = Duration::from_millis(200);
                }
                Err(e) => {
                    if t0 >= next_teleop_warn {
                        warn!(
                            "teleop TCP {teleop_addr}: {e:#} — LTP/UDP video still runs without it; start quest_teleop when ready (this message every 15s)"
                        );
                        next_teleop_warn = t0 + Duration::from_secs(15);
                    }
                    next_teleop_try = t0 + teleop_backoff;
                    teleop_backoff = (teleop_backoff * 2).min(Duration::from_secs(2));
                }
            }
        }

        // Phase A: poll ingress
        let tp_poll = Instant::now();
        session.poll_ingress()?;
        stats.lat_poll_us_sum += tp_poll.elapsed().as_micros() as u64;

        // Phase B: flush pending datagrams
        let tp_flush = Instant::now();
        let now = now_ns();
        session.flush_send(now)?;
        stats.lat_flush_us_sum += tp_flush.elapsed().as_micros() as u64;

        // Phase C: drain ingress events
        let tp_drain = Instant::now();
        let events = session.drain_ingress(true, now);
        for ev in events {
            match ev {
                ReceivedEvent::ControlOrdered(chunks) => {
                    for chunk in chunks {
                        if let Ok(env) = ControlEnvelope::decode(&chunk) {
                            if env.schema_id == SCHEMA_QUEST_TELEOP112
                                && env.payload.len() == QUEST_TELEOP112_LEN
                            {
                                if let Some(ref mut t) = teleop {
                                    if let Err(e) = t.write_all(&env.payload) {
                                        warn!("teleop TCP write: {e}; will reconnect");
                                        teleop = None;
                                        teleop_backoff = Duration::from_millis(200);
                                        next_teleop_try = Instant::now();
                                        break;
                                    }
                                    stats.control_to_teleop += 1;
                                } else {
                                    stats.control_dropped_no_tcp += 1;
                                }
                            }
                        }
                    }
                }
                ReceivedEvent::Duplicate => stats.ingress_dup += 1,
                ReceivedEvent::VideoProgress { .. } => stats.ingress_video += 1,
                ReceivedEvent::Telemetry(_) => stats.ingress_telem += 1,
            }
        }
        stats.lat_drain_us_sum += tp_drain.elapsed().as_micros() as u64;
        stats.lat_loop_count += 1;

        // Phase D: encode + send JPEGs
        let congested = session.video_queue_len() > BACKPRESSURE_THRESHOLD;
        let mut got_jpeg = false;
        while let Ok(msg) = jpeg_rx.try_recv() {
            got_jpeg = true;
            if congested {
                stats.jpegs_skipped_backpressure += 1;
                continue;
            }
            let cam = msg.cam as usize;
            if cam >= 4 {
                continue;
            }
            let dequeue_ns = now_ns();
            let channel_delay_us = dequeue_ns.saturating_sub(msg.assembled_ns) / 1000;
            stats.lat_channel_us_sum += channel_delay_us;

            global_frame_id = global_frame_id.wrapping_add(1);
            let sid = (cam as u16) + 1;
            let fid = global_frame_id;
            let tp_enc = Instant::now();
            match send_jpeg_ltp(&mut session, sid, &mut seq, fid, &msg.jpeg, dequeue_ns) {
                Ok(n) => {
                    let enc_us = tp_enc.elapsed().as_micros() as u64;
                    stats.lat_encode_us_sum += enc_us;
                    stats.lat_sample_count += 1;
                    stats.jpeg_frames[cam] += 1;
                    stats.jpeg_bytes[cam] += msg.jpeg.len() as u64;
                    stats.ltp_slices_encoded += n as u64;
                }
                Err(e) => warn!("send_jpeg_ltp cam{cam}: {e}"),
            }
        }

        let pending = session.pending_send_datagrams();
        stats.last_pending_send = pending;
        stats.max_pending_send = stats.max_pending_send.max(pending);
        stats.maybe_log(t0, session.video_dropped());

        let elapsed = t0.elapsed();
        let target = if pending > 0 {
            Duration::from_micros(50)
        } else if got_jpeg {
            Duration::from_micros(200)
        } else {
            Duration::from_micros(500)
        };
        if elapsed < target {
            std::thread::sleep(target - elapsed);
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
    validate_teleop_tcp(args.teleop_tcp).context("invalid --teleop-tcp")?;
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

    let (tx, rx) = mpsc::channel::<TimestampedJpeg>();
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
