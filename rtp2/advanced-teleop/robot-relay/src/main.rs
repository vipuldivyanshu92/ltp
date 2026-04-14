//! Robot-side relay: bridges local anvil_streamer (WS :9191) and quest_teleop
//! (TCP :8081) to the cloud QUIC relay.
//!
//! Video path:  anvil_streamer WS → JPEG assembly → QUIC uni-stream → cloud-relay → quest
//! Control path: cloud-relay QUIC bidi-stream → 112-byte packets → TCP :8081 → ROS
//!
//! Usage:
//!   robot-relay --cloud-host <VPS_IP> --cloud-port 4433

use std::sync::atomic::{AtomicU16, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use clap::Parser;
use futures_util::{SinkExt, StreamExt};
use quinn::crypto::rustls::QuicClientConfig;
use tokio::io::AsyncWriteExt;
use tokio::net::TcpStream;
use tokio::sync::mpsc;
use tracing::{error, info, warn};

// ── Wire protocol ───────────────────────────────────────────────────────

const VIDEO_HDR_LEN: usize = 8;
const TELEOP_PKT_LEN: usize = 112;

fn encode_video_header(cam_index: u8, frame_id: u16, jpeg_len: u32) -> [u8; VIDEO_HDR_LEN] {
    let mut hdr = [0u8; VIDEO_HDR_LEN];
    hdr[0] = cam_index;
    hdr[1] = 0; // flags
    hdr[2..4].copy_from_slice(&frame_id.to_be_bytes());
    hdr[4..8].copy_from_slice(&jpeg_len.to_be_bytes());
    hdr
}

// ── CLI ─────────────────────────────────────────────────────────────────

#[derive(Parser, Debug)]
#[command(name = "robot-relay")]
struct Args {
    /// anvil_streamer WebSocket host
    #[arg(long, default_value = "127.0.0.1")]
    ws_host: String,

    /// anvil_streamer WebSocket port
    #[arg(long, default_value_t = 9191)]
    ws_port: u16,

    /// Cloud relay hostname
    #[arg(long)]
    cloud_host: String,

    /// Cloud relay QUIC port
    #[arg(long, default_value_t = 4433)]
    cloud_port: u16,

    /// quest_teleop TCP target (ROS bridge). Must be a connectable address.
    #[arg(long, default_value = "127.0.0.1:8081")]
    teleop_tcp: String,

    /// Target video FPS per camera (0 = unlimited)
    #[arg(long, default_value_t = 30)]
    video_fps: u32,

    /// Camera ROS compressed topics (1–4, same order as Quest panels)
    #[arg(
        long,
        num_args = 1..=4,
        default_values = [
            "/cam_chest/image_raw/compressed",
        ]
    )]
    topics: Vec<String>,
}

// ── JPEG assembly ───────────────────────────────────────────────────────

struct JpegFrame {
    cam: u8,
    jpeg: Vec<u8>,
}

/// Accumulates WS binary chunks until a full JPEG (FFD8..FFD9) is found.
struct JpegAssembler {
    buf: Vec<u8>,
}

impl JpegAssembler {
    fn new() -> Self {
        Self {
            buf: Vec::with_capacity(256 * 1024),
        }
    }

    fn push_chunk(&mut self, chunk: &[u8]) -> Vec<Vec<u8>> {
        const MAX: usize = 8 * 1024 * 1024;

        let starts_soi = chunk.len() >= 2 && chunk[0] == 0xff && chunk[1] == 0xd8;
        let ends_eoi =
            chunk.len() >= 2 && chunk[chunk.len() - 2] == 0xff && chunk[chunk.len() - 1] == 0xd9;

        if starts_soi && !self.buf.is_empty() {
            self.buf.clear();
        }
        if self.buf.is_empty() && !starts_soi {
            return vec![];
        }
        if self.buf.len() + chunk.len() > MAX {
            self.buf.clear();
            return vec![];
        }

        self.buf.extend_from_slice(chunk);
        let mut out = Vec::new();
        if ends_eoi
            && self.buf.len() >= 2
            && self.buf[self.buf.len() - 2] == 0xff
            && self.buf[self.buf.len() - 1] == 0xd9
        {
            out.push(std::mem::take(&mut self.buf));
        }
        out
    }
}

// ── FPS limiter ─────────────────────────────────────────────────────────

struct FpsLimiter {
    min_interval: Duration,
    last_sent: [Instant; 4],
}

impl FpsLimiter {
    fn new(target_fps: u32) -> Self {
        let min_interval = if target_fps == 0 {
            Duration::ZERO
        } else {
            Duration::from_nanos(1_000_000_000 / target_fps as u64)
        };
        let past = Instant::now() - Duration::from_secs(1);
        Self {
            min_interval,
            last_sent: [past; 4],
        }
    }

    fn allow(&mut self, cam: usize) -> bool {
        if self.min_interval.is_zero() || cam >= 4 {
            return true;
        }
        let now = Instant::now();
        if now.duration_since(self.last_sent[cam]) >= self.min_interval {
            self.last_sent[cam] = now;
            true
        } else {
            false
        }
    }
}

// ── QUIC client setup ───────────────────────────────────────────────────

fn build_quic_client_config() -> Result<quinn::ClientConfig> {
    // Accept any certificate (self-signed relay).
    let mut crypto = rustls::ClientConfig::builder()
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(SkipServerVerification))
        .with_no_client_auth();

    // Must match one of the server's advertised ALPNs for the handshake to succeed.
    crypto.alpn_protocols = vec![b"teleop-robot".to_vec()];

    let mut client_config = quinn::ClientConfig::new(Arc::new(
        QuicClientConfig::try_from(crypto).context("QuicClientConfig")?,
    ));

    let mut transport = quinn::TransportConfig::default();
    transport.max_concurrent_uni_streams(256u32.into());
    transport.max_concurrent_bidi_streams(4u32.into());
    transport.receive_window(64_000_000u32.into());
    transport.send_window(64_000_000);
    transport.stream_receive_window(16_000_000u32.into());
    transport.keep_alive_interval(Some(Duration::from_secs(5)));
    client_config.transport_config(Arc::new(transport));
    Ok(client_config)
}

/// Skips server certificate verification (for self-signed dev certs).
#[derive(Debug)]
struct SkipServerVerification;

impl rustls::client::danger::ServerCertVerifier for SkipServerVerification {
    fn verify_server_cert(
        &self,
        _: &rustls::pki_types::CertificateDer<'_>,
        _: &[rustls::pki_types::CertificateDer<'_>],
        _: &rustls::pki_types::ServerName<'_>,
        _: &[u8],
        _: rustls::pki_types::UnixTime,
    ) -> Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
        Ok(rustls::client::danger::ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        _: &[u8],
        _: &rustls::pki_types::CertificateDer<'_>,
        _: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
    }

    fn verify_tls13_signature(
        &self,
        _: &[u8],
        _: &rustls::pki_types::CertificateDer<'_>,
        _: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
    }

    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        rustls::crypto::ring::default_provider()
            .signature_verification_algorithms
            .supported_schemes()
    }
}

// ── Main ────────────────────────────────────────────────────────────────

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let args = Args::parse();
    info!(
        "robot-relay: WS {}:{} | cloud {}:{} | teleop TCP {}",
        args.ws_host, args.ws_port, args.cloud_host, args.cloud_port, args.teleop_tcp
    );

    // ── QUIC connection to cloud relay ──────────────────────────────

    let client_config = build_quic_client_config()?;
    let mut endpoint = quinn::Endpoint::client("0.0.0.0:0".parse()?)?;
    endpoint.set_default_client_config(client_config);

    let cloud_addr: std::net::SocketAddr = tokio::net::lookup_host(format!(
        "{}:{}",
        args.cloud_host, args.cloud_port
    ))
    .await
    .context("DNS lookup")?
    .next()
    .context("no address resolved")?;

    info!("connecting to cloud relay at {cloud_addr}...");
    let conn = endpoint
        .connect(cloud_addr, "teleop-relay")?
        .await
        .context("QUIC connect to cloud")?;
    info!("connected to cloud relay");

    // ── Stats ───────────────────────────────────────────────────────

    let frames_sent = Arc::new([
        AtomicU64::new(0),
        AtomicU64::new(0),
        AtomicU64::new(0),
        AtomicU64::new(0),
    ]);
    let bytes_sent = Arc::new(AtomicU64::new(0));
    let ctrl_received = Arc::new(AtomicU64::new(0));
    let frame_id_counter = Arc::new(AtomicU16::new(0));

    // Stats printer
    {
        let fs = frames_sent.clone();
        let bs = bytes_sent.clone();
        let cr = ctrl_received.clone();
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_secs(5));
            loop {
                interval.tick().await;
                let f: Vec<u64> = fs.iter().map(|a| a.load(Ordering::Relaxed)).collect();
                let b = bs.load(Ordering::Relaxed);
                let c = cr.load(Ordering::Relaxed);
                info!(
                    "stats: frames/cam={f:?} total_bytes={b} ctrl_pkts={c}"
                );
            }
        });
    }

    // ── Channel: WS tasks send JPEGs here, main loop sends to QUIC ─

    let (jpeg_tx, mut jpeg_rx) = mpsc::channel::<JpegFrame>(64);

    // ── WS subscriber tasks (one per camera) ────────────────────────

    for (i, topic) in args.topics.iter().enumerate() {
        let ws_url = format!("ws://{}:{}/", args.ws_host, args.ws_port);
        let topic = topic.clone();
        let tx = jpeg_tx.clone();
        let cam_index = i as u8;

        tokio::spawn(async move {
            let mut backoff = Duration::from_millis(300);
            loop {
                info!("WS cam{cam_index}: connecting to {ws_url} topic={topic}");
                let ws_stream = match tokio_tungstenite::connect_async(&ws_url).await {
                    Ok((ws, _)) => ws,
                    Err(e) => {
                        warn!("WS cam{cam_index} connect failed: {e}");
                        tokio::time::sleep(backoff).await;
                        backoff = (backoff * 2).min(Duration::from_secs(5));
                        continue;
                    }
                };
                backoff = Duration::from_millis(300);

                let (mut ws_write, mut ws_read) = ws_stream.split();

                // Subscribe to topic
                if let Err(e) = ws_write.send(tokio_tungstenite::tungstenite::Message::Text(topic.clone().into())).await {
                    error!("WS cam{cam_index} subscribe send: {e}");
                    continue;
                }
                info!("WS cam{cam_index}: subscribed to {topic}");

                let mut asm = JpegAssembler::new();
                loop {
                    match ws_read.next().await {
                        Some(Ok(tokio_tungstenite::tungstenite::Message::Binary(data))) => {
                            for jpeg in asm.push_chunk(&data) {
                                if tx
                                    .send(JpegFrame {
                                        cam: cam_index,
                                        jpeg,
                                    })
                                    .await
                                    .is_err()
                                {
                                    return; // channel closed
                                }
                            }
                        }
                        Some(Ok(tokio_tungstenite::tungstenite::Message::Ping(p))) => {
                            let _ = ws_write.send(tokio_tungstenite::tungstenite::Message::Pong(p)).await;
                        }
                        Some(Ok(_)) => {} // text, pong, etc
                        Some(Err(e)) => {
                            warn!("WS cam{cam_index} read error: {e}");
                            break;
                        }
                        None => {
                            info!("WS cam{cam_index}: stream ended");
                            break;
                        }
                    }
                }

                info!("WS cam{cam_index}: disconnected, retrying...");
                tokio::time::sleep(Duration::from_millis(500)).await;
            }
        });
    }
    drop(jpeg_tx); // we keep only clones in tasks

    // ── Control: cloud-relay bidi-stream → TCP :8081 ─────────────────

    let conn_ctrl = conn.clone();
    let teleop_tcp = args.teleop_tcp.clone();
    let ctrl_received2 = ctrl_received.clone();

    tokio::spawn(async move {
        // The cloud relay opens a bidi stream to us for each quest control
        // session.  We accept each one and forward 112-byte packets to the
        // ROS quest_teleop TCP server.
        loop {
            let (_quic_send, mut quic_recv) = match conn_ctrl.accept_bi().await {
                Ok(s) => s,
                Err(quinn::ConnectionError::ApplicationClosed(_)) => break,
                Err(e) => {
                    error!("control accept_bi error: {e}");
                    break;
                }
            };
            info!("accepted control bidi stream from cloud relay");

            let teleop_tcp = teleop_tcp.clone();
            let ctrl_received2 = ctrl_received2.clone();

            tokio::spawn(async move {
                // Connect to quest_teleop TCP and forward.
                let mut tcp: Option<TcpStream> = None;
                let mut tcp_backoff = Duration::from_millis(200);
                let mut buf = vec![0u8; TELEOP_PKT_LEN];

                loop {
                    // Read exactly 112 bytes from QUIC.
                    match quic_recv.read_exact(&mut buf).await {
                        Ok(()) => {}
                        Err(e) => {
                            warn!("control quic_recv: {e}");
                            break;
                        }
                    }

                    ctrl_received2.fetch_add(1, Ordering::Relaxed);

                    // Ensure TCP connection to quest_teleop.
                    loop {
                        if tcp.is_some() {
                            break;
                        }
                        match TcpStream::connect(&teleop_tcp).await {
                            Ok(s) => {
                                s.set_nodelay(true).ok();
                                info!("teleop TCP connected to {teleop_tcp}");
                                tcp = Some(s);
                                tcp_backoff = Duration::from_millis(200);
                            }
                            Err(e) => {
                                warn!("teleop TCP {teleop_tcp}: {e}; retrying in {tcp_backoff:?}");
                                tokio::time::sleep(tcp_backoff).await;
                                tcp_backoff = (tcp_backoff * 2).min(Duration::from_secs(2));
                            }
                        }
                    }

                    if let Some(ref mut stream) = tcp {
                        if let Err(e) = stream.write_all(&buf).await {
                            warn!("teleop TCP write: {e}; reconnecting");
                            tcp = None;
                        }
                    }
                }
            });
        }
    });

    // ── Video: JPEG channel → QUIC uni-streams (parallel sends) ──────

    let mut fps_limiter = FpsLimiter::new(args.video_fps);
    // Allow up to 16 frames in-flight to avoid head-of-line blocking between
    // cameras, while limiting memory usage if the network is slower than the
    // camera source.
    let send_semaphore = Arc::new(tokio::sync::Semaphore::new(16));

    while let Some(frame) = jpeg_rx.recv().await {
        let cam = frame.cam as usize;
        if cam >= 4 {
            continue;
        }
        if !fps_limiter.allow(cam) {
            continue;
        }

        let fid = frame_id_counter.fetch_add(1, Ordering::Relaxed);
        let hdr = encode_video_header(frame.cam, fid, frame.jpeg.len() as u32);

        // Acquire a permit before spawning (blocks if 16 frames already in-flight).
        let permit = match send_semaphore.clone().acquire_owned().await {
            Ok(p) => p,
            Err(_) => break, // semaphore closed
        };

        let conn = conn.clone();
        let fs = frames_sent.clone();
        let bs = bytes_sent.clone();

        tokio::spawn(async move {
            let _permit = permit; // held until send completes

            match conn.open_uni().await {
                Ok(mut send) => {
                    let jpeg_len = frame.jpeg.len();
                    let write_result = async {
                        send.write_all(&hdr).await?;
                        send.write_all(&frame.jpeg).await?;
                        send.finish()?;
                        Ok::<(), anyhow::Error>(())
                    }
                    .await;

                    if let Err(e) = write_result {
                        warn!("video uni-stream write error: {e}");
                    } else {
                        fs[cam].fetch_add(1, Ordering::Relaxed);
                        bs.fetch_add(jpeg_len as u64, Ordering::Relaxed);
                    }
                }
                Err(e) => {
                    error!("open_uni to cloud failed: {e}; connection may be lost");
                }
            }
        });
    }

    info!("robot-relay shutting down");
    conn.close(0u32.into(), b"shutdown");
    Ok(())
}
