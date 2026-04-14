//! Operator-side proxy: exposes a local WebSocket server on :9192 (mimicking
//! anvil_streamer) and a TCP server on :8082 (accepting TelemetryPacket from
//! the Quest).
//!
//! **Video** has two modes:
//!   1. `--local-ws ws://127.0.0.1:9191`  (recommended for same-machine)
//!      Subscribes directly to anvil_streamer's WebSocket — zero relay latency.
//!   2. `--cloud-host <VPS_IP>` (remote operation)
//!      Receives video from cloud-relay via QUIC uni-streams.
//!
//! **Control** always flows through the cloud relay:
//!   Quest TCP :8082 → QUIC bidi-stream → cloud-relay → robot-relay → ROS
//!
//! Usage:
//!   # Same-machine (best latency):
//!   quest-proxy --local-ws ws://127.0.0.1:9191 --cloud-host <VPS_IP>
//!
//!   # Remote only:
//!   quest-proxy --cloud-host <VPS_IP>

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use clap::Parser;
use futures_util::{SinkExt, StreamExt};
use quinn::crypto::rustls::QuicClientConfig;
use tokio::io::AsyncReadExt;
use tokio::net::TcpListener;
use tokio::sync::{broadcast, RwLock};
use tracing::{error, info, warn};

const VIDEO_HDR_LEN: usize = 8;
const TELEOP_PKT_LEN: usize = 112;

// ── CLI ─────────────────────────────────────────────────────────────────

#[derive(Parser, Debug)]
#[command(name = "quest-proxy")]
struct Args {
    /// Cloud relay hostname (required for control; optional for video if --local-ws is set)
    #[arg(long)]
    cloud_host: Option<String>,

    /// Cloud relay QUIC port
    #[arg(long, default_value_t = 4433)]
    cloud_port: u16,

    /// Local WebSocket port exposed to the Quest (mimics anvil_streamer)
    #[arg(long, default_value_t = 9192)]
    ws_port: u16,

    /// Local TCP port for Quest teleop data
    #[arg(long, default_value_t = 8082)]
    teleop_port: u16,

    /// Direct WebSocket URL to anvil_streamer (e.g. ws://127.0.0.1:9191).
    /// When set, video is fetched directly from this WS, bypassing the cloud relay.
    #[arg(long)]
    local_ws: Option<String>,
}

// ── QUIC client setup ───────────────────────────────────────────────────

fn build_quic_client_config() -> Result<quinn::ClientConfig> {
    let mut crypto = rustls::ClientConfig::builder()
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(SkipServerVerification))
        .with_no_client_auth();

    crypto.alpn_protocols = vec![b"teleop-quest".to_vec()];

    let mut client_config = quinn::ClientConfig::new(Arc::new(
        QuicClientConfig::try_from(crypto).context("QuicClientConfig")?,
    ));

    let mut transport = quinn::TransportConfig::default();
    transport.max_concurrent_uni_streams(256u32.into());
    transport.max_concurrent_bidi_streams(4u32.into());
    transport.receive_window(256_000_000u32.into());
    transport.send_window(64_000_000);
    transport.stream_receive_window(16_000_000u32.into());
    transport.keep_alive_interval(Some(Duration::from_secs(5)));
    client_config.transport_config(Arc::new(transport));
    Ok(client_config)
}

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

// ── Topic broadcast ─────────────────────────────────────────────────────

type TopicMap = Arc<RwLock<HashMap<String, broadcast::Sender<Vec<u8>>>>>;

/// Camera index → topic mapping.
const CAMERA_TOPICS: [&str; 1] = [
    "/cam_chest/image_raw/compressed",
];

// ── JPEG assembly (for local WS mode) ──────────────────────────────────

struct JpegAssembler {
    buf: Vec<u8>,
}

impl JpegAssembler {
    fn new() -> Self {
        Self { buf: Vec::with_capacity(256 * 1024) }
    }

    fn push_chunk(&mut self, chunk: &[u8]) -> Vec<Vec<u8>> {
        let mut frames = Vec::new();
        self.buf.extend_from_slice(chunk);

        loop {
            // Find SOI (0xFF 0xD8)
            let soi = match self.buf.windows(2).position(|w| w == [0xFF, 0xD8]) {
                Some(p) => p,
                None => {
                    self.buf.clear();
                    break;
                }
            };
            if soi > 0 {
                self.buf.drain(..soi);
            }

            // Find EOI (0xFF 0xD9) after SOI
            let eoi = match self.buf[2..].windows(2).position(|w| w == [0xFF, 0xD9]) {
                Some(p) => p + 2 + 2, // offset from start + marker size
                None => break,         // incomplete frame
            };

            let jpeg = self.buf[..eoi].to_vec();
            self.buf.drain(..eoi);
            frames.push(jpeg);
        }
        frames
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

    let use_local_ws = args.local_ws.is_some();
    info!(
        "quest-proxy: {} | WS :{} | teleop TCP :{}",
        if let Some(ref ws) = args.local_ws {
            format!("local-ws {}", ws)
        } else if let Some(ref host) = args.cloud_host {
            format!("cloud {}:{}", host, args.cloud_port)
        } else {
            "NO VIDEO SOURCE (need --local-ws or --cloud-host)".to_string()
        },
        args.ws_port,
        args.teleop_port,
    );

    // ── Topic broadcast channels ────────────────────────────────────

    let topics: TopicMap = Arc::new(RwLock::new(HashMap::new()));
    {
        let mut map = topics.write().await;
        for topic in &CAMERA_TOPICS {
            let (tx, _) = broadcast::channel::<Vec<u8>>(4); // small buffer; drop old frames
            map.insert(topic.to_string(), tx);
        }
    }

    // ── Stats ───────────────────────────────────────────────────────

    let video_frames_recv = Arc::new(AtomicU64::new(0));
    let ctrl_sent = Arc::new(AtomicU64::new(0));
    {
        let vfr = video_frames_recv.clone();
        let cs = ctrl_sent.clone();
        let last_vfr = Arc::new(AtomicU64::new(0));
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_secs(5));
            loop {
                interval.tick().await;
                let cur = vfr.load(Ordering::Relaxed);
                let prev = last_vfr.swap(cur, Ordering::Relaxed);
                let delta = cur - prev;
                let fps = delta as f64 / 5.0;
                info!(
                    "stats: video_frames={cur} (+{delta} = {fps:.1} fps) ctrl_sent={}",
                    cs.load(Ordering::Relaxed)
                );
            }
        });
    }

    // ── Video source (local WS or QUIC) ─────────────────────────────

    if let Some(ref local_ws_url) = args.local_ws {
        // LOCAL WS MODE: subscribe directly to anvil_streamer
        let url = local_ws_url.clone();
        let topics_video = topics.clone();
        let vfr = video_frames_recv.clone();

        for (cam_index, topic) in CAMERA_TOPICS.iter().enumerate() {
            let url = url.clone();
            let topic = topic.to_string();
            let topics = topics_video.clone();
            let vfr = vfr.clone();

            tokio::spawn(async move {
                let mut backoff = Duration::from_millis(200);
                loop {
                    info!("local-ws cam{cam_index}: connecting to {url} topic={topic}");
                    let ws_stream = match tokio_tungstenite::connect_async(&url).await {
                        Ok((ws, _)) => ws,
                        Err(e) => {
                            warn!("local-ws cam{cam_index} connect failed: {e}");
                            tokio::time::sleep(backoff).await;
                            backoff = (backoff * 2).min(Duration::from_secs(5));
                            continue;
                        }
                    };
                    backoff = Duration::from_millis(200);

                    let (mut ws_write, mut ws_read) = ws_stream.split();

                    // Subscribe to topic
                    if let Err(e) = ws_write
                        .send(tokio_tungstenite::tungstenite::Message::Text(topic.clone().into()))
                        .await
                    {
                        error!("local-ws cam{cam_index} subscribe: {e}");
                        continue;
                    }
                    info!("local-ws cam{cam_index}: subscribed to {topic}");

                    let mut asm = JpegAssembler::new();
                    loop {
                        match ws_read.next().await {
                            Some(Ok(tokio_tungstenite::tungstenite::Message::Binary(data))) => {
                                for jpeg in asm.push_chunk(&data) {
                                    vfr.fetch_add(1, Ordering::Relaxed);
                                    let map = topics.read().await;
                                    if let Some(tx) = map.get(&topic) {
                                        let _ = tx.send(jpeg);
                                    }
                                }
                            }
                            Some(Ok(tokio_tungstenite::tungstenite::Message::Ping(p))) => {
                                let _ = ws_write
                                    .send(tokio_tungstenite::tungstenite::Message::Pong(p))
                                    .await;
                            }
                            Some(Ok(_)) => {}
                            Some(Err(e)) => {
                                warn!("local-ws cam{cam_index} read error: {e}");
                                break;
                            }
                            None => {
                                info!("local-ws cam{cam_index}: stream ended");
                                break;
                            }
                        }
                    }

                    info!("local-ws cam{cam_index}: disconnected, retrying...");
                    tokio::time::sleep(Duration::from_millis(500)).await;
                }
            });
        }
    } else if let Some(ref cloud_host) = args.cloud_host {
        // QUIC MODE: receive video from cloud relay
        let client_config = build_quic_client_config()?;
        let mut endpoint = quinn::Endpoint::client("0.0.0.0:0".parse()?)?;
        endpoint.set_default_client_config(client_config);

        let cloud_addr: std::net::SocketAddr = tokio::net::lookup_host(format!(
            "{}:{}",
            cloud_host, args.cloud_port
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

        // Video: QUIC uni-streams → topic broadcasts
        let conn_video = conn.clone();
        let topics_video = topics.clone();
        let vfr = video_frames_recv.clone();

        tokio::spawn(async move {
            loop {
                let mut recv_stream = match conn_video.accept_uni().await {
                    Ok(s) => s,
                    Err(quinn::ConnectionError::ApplicationClosed(_)) => break,
                    Err(e) => {
                        warn!("video accept_uni error: {e}");
                        break;
                    }
                };

                let topics = topics_video.clone();
                let vfr = vfr.clone();

                tokio::spawn(async move {
                    let data = match recv_stream.read_to_end(8 * 1024 * 1024).await {
                        Ok(d) => d,
                        Err(e) => {
                            warn!("video stream read: {e}");
                            return;
                        }
                    };

                    if data.len() < VIDEO_HDR_LEN {
                        return;
                    }

                    let cam_index = data[0] as usize;
                    if cam_index >= CAMERA_TOPICS.len() {
                        return;
                    }

                    let jpeg = data[VIDEO_HDR_LEN..].to_vec();
                    vfr.fetch_add(1, Ordering::Relaxed);

                    let topic = CAMERA_TOPICS[cam_index];
                    let map = topics.read().await;
                    if let Some(tx) = map.get(topic) {
                        let _ = tx.send(jpeg);
                    }
                });
            }
        });

        // Control: TCP → QUIC bidi-stream
        let conn_ctrl = conn.clone();
        let cs = ctrl_sent.clone();
        let teleop_port = args.teleop_port;

        tokio::spawn(async move {
            let listener = match TcpListener::bind(format!("0.0.0.0:{teleop_port}")).await {
                Ok(l) => l,
                Err(e) => {
                    error!("failed to bind teleop TCP :{teleop_port}: {e}");
                    return;
                }
            };
            info!("teleop TCP server listening on :{teleop_port}");

            loop {
                let (mut tcp_stream, peer) = match listener.accept().await {
                    Ok(s) => s,
                    Err(e) => {
                        warn!("teleop TCP accept: {e}");
                        continue;
                    }
                };
                info!("teleop TCP client connected from {peer}");

                let conn = conn_ctrl.clone();
                let cs = cs.clone();

                tokio::spawn(async move {
                    let (mut quic_send, _quic_recv) = match conn.open_bi().await {
                        Ok(s) => s,
                        Err(e) => {
                            error!("control open_bi: {e}");
                            return;
                        }
                    };

                    let mut buf = vec![0u8; TELEOP_PKT_LEN];
                    loop {
                        match tcp_stream.read_exact(&mut buf).await {
                            Ok(_) => {}
                            Err(e) => {
                                info!("teleop TCP read ended: {e}");
                                break;
                            }
                        }
                        if let Err(e) = quic_send.write_all(&buf).await {
                            warn!("control QUIC write: {e}");
                            break;
                        }
                        cs.fetch_add(1, Ordering::Relaxed);
                    }
                    let _ = quic_send.finish();
                });
            }
        });
    } else {
        warn!("No video source configured. Use --local-ws or --cloud-host.");
    }

    // ── Control TCP (local-ws mode needs its own control handler) ────

    if use_local_ws {
        if let Some(ref cloud_host) = args.cloud_host {
            // Connect to cloud relay for control only
            let client_config = build_quic_client_config()?;
            let mut endpoint = quinn::Endpoint::client("0.0.0.0:0".parse()?)?;
            endpoint.set_default_client_config(client_config);

            let cloud_addr: std::net::SocketAddr = tokio::net::lookup_host(format!(
                "{}:{}",
                cloud_host, args.cloud_port
            ))
            .await
            .context("DNS lookup")?
            .next()
            .context("no address resolved")?;

            info!("connecting to cloud relay for control at {cloud_addr}...");
            let conn = endpoint
                .connect(cloud_addr, "teleop-relay")?
                .await
                .context("QUIC connect to cloud for control")?;
            info!("connected to cloud relay for control");

            let cs = ctrl_sent.clone();
            let teleop_port = args.teleop_port;

            tokio::spawn(async move {
                let listener = match TcpListener::bind(format!("0.0.0.0:{teleop_port}")).await {
                    Ok(l) => l,
                    Err(e) => {
                        error!("failed to bind teleop TCP :{teleop_port}: {e}");
                        return;
                    }
                };
                info!("teleop TCP server listening on :{teleop_port}");

                loop {
                    let (mut tcp_stream, peer) = match listener.accept().await {
                        Ok(s) => s,
                        Err(e) => {
                            warn!("teleop TCP accept: {e}");
                            continue;
                        }
                    };
                    info!("teleop TCP client connected from {peer}");

                    let conn = conn.clone();
                    let cs = cs.clone();

                    tokio::spawn(async move {
                        let (mut quic_send, _quic_recv) = match conn.open_bi().await {
                            Ok(s) => s,
                            Err(e) => {
                                error!("control open_bi: {e}");
                                return;
                            }
                        };

                        let mut buf = vec![0u8; TELEOP_PKT_LEN];
                        loop {
                            match tcp_stream.read_exact(&mut buf).await {
                                Ok(_) => {}
                                Err(e) => {
                                    info!("teleop TCP read ended: {e}");
                                    break;
                                }
                            }
                            if let Err(e) = quic_send.write_all(&buf).await {
                                warn!("control QUIC write: {e}");
                                break;
                            }
                            cs.fetch_add(1, Ordering::Relaxed);
                        }
                        let _ = quic_send.finish();
                    });
                }
            });
        } else {
            warn!("No --cloud-host specified; teleop control disabled. Quest controllers won't move the robot.");
        }
    }

    // ── WS server (exposed to Quest via adb reverse) ────────────────

    let ws_listener =
        TcpListener::bind(format!("0.0.0.0:{}", args.ws_port)).await?;
    info!("WS server listening on :{}", args.ws_port);

    loop {
        let (tcp_stream, peer) = ws_listener.accept().await?;
        let topics = topics.clone();
        tokio::spawn(async move {
            if let Err(e) = handle_ws_client(tcp_stream, peer, topics).await {
                warn!("WS client {peer}: {e}");
            }
        });
    }
}

/// Handles one WebSocket client: expects a text message with the topic name,
/// then streams binary JPEG frames for that topic.
async fn handle_ws_client(
    tcp_stream: tokio::net::TcpStream,
    peer: std::net::SocketAddr,
    topics: TopicMap,
) -> Result<()> {
    let ws_stream = tokio_tungstenite::accept_async(tcp_stream)
        .await
        .context("WS accept")?;
    let (mut ws_write, mut ws_read) = ws_stream.split();

    // First message should be the topic subscription.
    let topic = loop {
        match ws_read.next().await {
            Some(Ok(tokio_tungstenite::tungstenite::Message::Text(t))) => break t,
            Some(Ok(_)) => continue,
            Some(Err(e)) => anyhow::bail!("WS read error: {e}"),
            None => anyhow::bail!("WS closed before topic"),
        }
    };

    info!("WS client {peer} subscribed to {topic}");

    // Get broadcast receiver for this topic.
    let mut rx = {
        let map = topics.read().await;
        match map.get(topic.as_str()) {
            Some(tx) => tx.subscribe(),
            None => {
                warn!("WS client {peer}: unknown topic {topic}");
                anyhow::bail!("unknown topic");
            }
        }
    };

    // Stream JPEGs to the WebSocket client.
    loop {
        match rx.recv().await {
            Ok(jpeg) => {
                if let Err(e) = ws_write
                    .send(tokio_tungstenite::tungstenite::Message::Binary(jpeg.into()))
                    .await
                {
                    warn!("WS send to {peer}: {e}");
                    break;
                }
            }
            Err(broadcast::error::RecvError::Lagged(n)) => {
                warn!("WS client {peer} lagged {n} frames");
            }
            Err(broadcast::error::RecvError::Closed) => {
                info!("topic channel closed for {peer}");
                break;
            }
        }
    }

    Ok(())
}
