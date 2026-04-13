//! Operator-side proxy: connects to the cloud QUIC relay, exposes a local
//! WebSocket server on :9192 (mimicking anvil_streamer) and a TCP server on
//! :8082 (accepting TelemetryPacket from the Quest).
//!
//! Video path:  cloud-relay QUIC uni-streams → parse 8B hdr → WS binary to Quest
//! Control path: Quest TCP :8082 → 112-byte packets → QUIC bidi-stream → cloud-relay → robot
//!
//! Usage:
//!   quest-proxy --cloud-host <VPS_IP>
//!   quest-proxy --cloud-host <VPS_IP> --ws-port 9192 --teleop-port 8082

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
    /// Cloud relay hostname
    #[arg(long)]
    cloud_host: String,

    /// Cloud relay QUIC port
    #[arg(long, default_value_t = 4433)]
    cloud_port: u16,

    /// Local WebSocket port (mimics anvil_streamer)
    #[arg(long, default_value_t = 9192)]
    ws_port: u16,

    /// Local TCP port for Quest teleop data
    #[arg(long, default_value_t = 8082)]
    teleop_port: u16,
}

// ── QUIC client setup ───────────────────────────────────────────────────

fn build_quic_client_config() -> Result<quinn::ClientConfig> {
    let mut crypto = rustls::ClientConfig::builder()
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(SkipServerVerification))
        .with_no_client_auth();

    // Must match one of the server's advertised ALPNs for the handshake to succeed.
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

// ── Per-camera broadcast channel ────────────────────────────────────────

/// Maps camera topic string → broadcast sender of JPEG bytes.
type TopicMap = Arc<RwLock<HashMap<String, broadcast::Sender<Vec<u8>>>>>;

/// Camera index → topic mapping (same as robot-relay default).
const CAMERA_TOPICS: [&str; 4] = [
    "/cam_chest/image_raw/compressed",
    "/cam_waist/image_raw/compressed",
    "/cam_wrist_l/image_raw/compressed",
    "/cam_wrist_r/image_raw/compressed",
];

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
        "quest-proxy: cloud {}:{} | WS :{} | teleop TCP :{}",
        args.cloud_host, args.cloud_port, args.ws_port, args.teleop_port
    );

    // ── Topic broadcast channels ────────────────────────────────────

    let topics: TopicMap = Arc::new(RwLock::new(HashMap::new()));
    {
        let mut map = topics.write().await;
        for topic in &CAMERA_TOPICS {
            let (tx, _) = broadcast::channel::<Vec<u8>>(8); // small buffer; drop old frames
            map.insert(topic.to_string(), tx);
        }
    }

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

    let video_frames_recv = Arc::new(AtomicU64::new(0));
    let ctrl_sent = Arc::new(AtomicU64::new(0));
    {
        let vfr = video_frames_recv.clone();
        let cs = ctrl_sent.clone();
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_secs(5));
            loop {
                interval.tick().await;
                info!(
                    "stats: video_frames_recv={} ctrl_sent={}",
                    vfr.load(Ordering::Relaxed),
                    cs.load(Ordering::Relaxed)
                );
            }
        });
    }

    // ── Task 1: receive QUIC uni-streams (video) → topic broadcasts ─

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
                // Read full stream (8B hdr + JPEG).
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
                if cam_index >= 4 {
                    return;
                }

                let jpeg = data[VIDEO_HDR_LEN..].to_vec();
                vfr.fetch_add(1, Ordering::Relaxed);

                // Broadcast to subscribers of this topic.
                let topic = CAMERA_TOPICS[cam_index];
                let map = topics.read().await;
                if let Some(tx) = map.get(topic) {
                    // If no subscribers, this is a no-op (just drops).
                    let _ = tx.send(jpeg);
                }
            });
        }
    });

    // ── Task 2: control — TCP :8081 → QUIC bidi-stream ─────────────

    let conn_ctrl = conn.clone();
    let cs = ctrl_sent.clone();
    let teleop_port = args.teleop_port;

    tokio::spawn(async move {
        let listener = match TcpListener::bind(format!("0.0.0.0:{teleop_port}")).await {
            Ok(l) => l,
            Err(e) => {
                error!("failed to bind TCP :{teleop_port}: {e}");
                return;
            }
        };
        info!("teleop TCP server listening on :{teleop_port}");

        loop {
            let (mut tcp_stream, peer) = match listener.accept().await {
                Ok(s) => s,
                Err(e) => {
                    warn!("TCP accept error: {e}");
                    continue;
                }
            };
            info!("teleop TCP client connected from {peer}");
            tcp_stream.set_nodelay(true).ok();

            // Open a bidi stream for this quest connection.
            let (mut quic_send, _quic_recv) = match conn_ctrl.open_bi().await {
                Ok(s) => s,
                Err(e) => {
                    error!("failed to open control bidi: {e}");
                    continue;
                }
            };

            let cs = cs.clone();

            tokio::spawn(async move {
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

    // ── Task 3: WS server :9191 (mimics anvil_streamer) ─────────────

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
            Some(Ok(_)) => continue, // skip non-text
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
                // Continue — we'll catch up with the latest frame.
            }
            Err(broadcast::error::RecvError::Closed) => {
                info!("topic channel closed for {peer}");
                break;
            }
        }
    }

    Ok(())
}
