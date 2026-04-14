//! Cloud relay server for remote teleoperation.
//!
//! Accepts two QUIC peers identified by ALPN:
//! - `teleop-robot`: the robot-side relay (sends video, receives control)
//! - `teleop-quest`: the operator-side proxy (receives video, sends control)
//!
//! **Video** (robot → quest): robot opens unidirectional streams carrying
//! `[8-byte header | JPEG payload]`. The relay streams bytes directly to a
//! matching unidirectional stream on the quest peer (zero-copy, no buffering).
//!
//! **Control** (quest → robot): the first bidirectional stream opened by
//! either side is the control channel. 112-byte `TelemetryPacket` records
//! flow quest → relay → robot. The relay bridges the two halves.
//!
//! Usage:
//!   cloud-relay --port 4433
//!   cloud-relay --port 4433 --cert cert.pem --key key.pem

use std::net::SocketAddr;
use std::sync::Arc;

use anyhow::{Context, Result};
use clap::Parser;
use quinn::crypto::rustls::QuicServerConfig;
use tokio::sync::watch;
use tracing::{info, warn};

// (wire protocol constants are in robot-relay and quest-proxy only)

// ── CLI ──────────────────────────────────────────────────────────────────

#[derive(Parser, Debug)]
#[command(name = "cloud-relay", about = "QUIC relay for remote teleop")]
struct Args {
    /// UDP listen port
    #[arg(long, default_value_t = 4433)]
    port: u16,

    /// TLS certificate PEM (generated if absent)
    #[arg(long)]
    cert: Option<String>,

    /// TLS private key PEM (generated if absent)
    #[arg(long)]
    key: Option<String>,
}

// ── TLS / QUIC setup ────────────────────────────────────────────────────

fn generate_self_signed() -> Result<(Vec<rustls::pki_types::CertificateDer<'static>>, rustls::pki_types::PrivateKeyDer<'static>)> {
    let cert = rcgen::generate_simple_self_signed(vec!["localhost".into(), "teleop-relay".into()])
        .context("rcgen generate")?;
    let key_der = rustls::pki_types::PrivateKeyDer::Pkcs8(
        rustls::pki_types::PrivatePkcs8KeyDer::from(cert.key_pair.serialize_der()),
    );
    let cert_der = rustls::pki_types::CertificateDer::from(cert.cert.der().to_vec());
    Ok((vec![cert_der], key_der))
}

fn load_certs_from_file(cert_path: &str, key_path: &str) -> Result<(Vec<rustls::pki_types::CertificateDer<'static>>, rustls::pki_types::PrivateKeyDer<'static>)> {
    let cert_pem = std::fs::read(cert_path).context("read cert")?;
    let key_pem = std::fs::read(key_path).context("read key")?;

    let certs: Vec<rustls::pki_types::CertificateDer<'static>> =
        rustls_pemfile::certs(&mut &cert_pem[..])
            .filter_map(|c: Result<rustls::pki_types::CertificateDer<'static>, _>| c.ok())
            .collect();
    let key = rustls_pemfile::private_key(&mut &key_pem[..])
        .context("parse private key")?
        .context("no private key found")?;
    Ok((certs, key))
}

fn build_server_config(
    certs: Vec<rustls::pki_types::CertificateDer<'static>>,
    key: rustls::pki_types::PrivateKeyDer<'static>,
) -> Result<quinn::ServerConfig> {
    let mut crypto = rustls::ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(certs, key)
        .context("rustls server config")?;

    // Accept both ALPNs so we can identify role after handshake.
    crypto.alpn_protocols = vec![
        b"teleop-robot".to_vec(),
        b"teleop-quest".to_vec(),
    ];

    let mut server_config = quinn::ServerConfig::with_crypto(Arc::new(
        QuicServerConfig::try_from(crypto).context("QuicServerConfig")?,
    ));

    // Enable large datagrams, generous transport settings for video.
    let mut transport = quinn::TransportConfig::default();
    transport.max_concurrent_uni_streams(256u32.into());
    transport.max_concurrent_bidi_streams(4u32.into());
    // Allow up to 256 MB of flight data for bursty video.
    transport.receive_window(256_000_000u32.into());
    transport.send_window(64_000_000);
    transport.stream_receive_window(16_000_000u32.into());
    // Keep-alives to survive NAT.
    transport.keep_alive_interval(Some(std::time::Duration::from_secs(5)));
    server_config.transport_config(Arc::new(transport));
    Ok(server_config)
}

// ── Peer state ──────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Role {
    Robot,
    Quest,
}

/// Shared state: each role has a watch channel that broadcasts the current
/// connection handle. When a new peer connects, it replaces the old one.
struct RelayState {
    robot_tx: watch::Sender<Option<quinn::Connection>>,
    robot_rx: watch::Receiver<Option<quinn::Connection>>,
    quest_tx: watch::Sender<Option<quinn::Connection>>,
    quest_rx: watch::Receiver<Option<quinn::Connection>>,
}

impl RelayState {
    fn new() -> Self {
        let (robot_tx, robot_rx) = watch::channel(None);
        let (quest_tx, quest_rx) = watch::channel(None);
        Self {
            robot_tx,
            robot_rx,
            quest_tx,
            quest_rx,
        }
    }

    fn set_peer(&self, role: Role, conn: quinn::Connection) {
        match role {
            Role::Robot => {
                let _ = self.robot_tx.send(Some(conn));
            }
            Role::Quest => {
                let _ = self.quest_tx.send(Some(conn));
            }
        }
    }

    fn clear_peer(&self, role: Role) {
        match role {
            Role::Robot => {
                let _ = self.robot_tx.send(None);
            }
            Role::Quest => {
                let _ = self.quest_tx.send(None);
            }
        }
    }

    fn get_peer(&self, role: Role) -> Option<quinn::Connection> {
        match role {
            Role::Robot => self.robot_rx.borrow().clone(),
            Role::Quest => self.quest_rx.borrow().clone(),
        }
    }

    /// Subscribe to peer connection changes for the given role.
    fn subscribe(&self, role: Role) -> watch::Receiver<Option<quinn::Connection>> {
        match role {
            Role::Robot => self.robot_rx.clone(),
            Role::Quest => self.quest_rx.clone(),
        }
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

    let (certs, key) = match (&args.cert, &args.key) {
        (Some(c), Some(k)) => load_certs_from_file(c, k)?,
        _ => {
            info!("No cert/key provided; generating self-signed certificate");
            generate_self_signed()?
        }
    };

    let server_config = build_server_config(certs, key)?;
    let addr: SocketAddr = format!("0.0.0.0:{}", args.port).parse()?;
    let endpoint = quinn::Endpoint::server(server_config, addr)?;
    info!("Cloud relay listening on {addr}");

    let state = Arc::new(RelayState::new());

    // Stats task
    let st = state.clone();
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(std::time::Duration::from_secs(10));
        loop {
            interval.tick().await;
            let robot = st.get_peer(Role::Robot).is_some();
            let quest = st.get_peer(Role::Quest).is_some();
            info!("relay status: robot={robot} quest={quest}");
        }
    });

    // Accept loop
    while let Some(incoming) = endpoint.accept().await {
        let state = state.clone();
        tokio::spawn(async move {
            if let Err(e) = handle_connection(incoming, state).await {
                warn!("connection handler error: {e:#}");
            }
        });
    }

    Ok(())
}

async fn handle_connection(incoming: quinn::Incoming, state: Arc<RelayState>) -> Result<()> {
    let conn = incoming.await.context("accept QUIC connection")?;
    let remote = conn.remote_address();

    // Determine role from negotiated ALPN.
    let alpn = conn
        .handshake_data()
        .and_then(|h| {
            h.downcast::<quinn::crypto::rustls::HandshakeData>()
                .ok()
                .and_then(|d| d.protocol)
        });

    let role = match alpn.as_deref() {
        Some(b"teleop-robot") => Role::Robot,
        Some(b"teleop-quest") => Role::Quest,
        other => {
            let label = other.map(|b| String::from_utf8_lossy(b).to_string());
            warn!("unknown ALPN from {remote}: {label:?}; dropping");
            conn.close(1u32.into(), b"unknown role");
            return Ok(());
        }
    };

    info!("{role:?} connected from {remote}");
    state.set_peer(role, conn.clone());

    // The "other" role we forward to.
    let target_role = match role {
        Role::Robot => Role::Quest,
        Role::Quest => Role::Robot,
    };

    // Spawn tasks for each direction.
    let conn2 = conn.clone();
    let state2 = state.clone();

    // Task 1: Forward incoming unidirectional streams (video) to the other peer.
    let uni_fwd = {
        let conn = conn.clone();
        let state = state.clone();
        tokio::spawn(async move {
            loop {
                let recv_stream = match conn.accept_uni().await {
                    Ok(s) => s,
                    Err(quinn::ConnectionError::ApplicationClosed(_)) => break,
                    Err(e) => {
                        warn!("{role:?} accept_uni error: {e}");
                        break;
                    }
                };
                let state = state.clone();
                // Handle each video stream in its own task.
                tokio::spawn(async move {
                    if let Err(e) = forward_uni_stream(recv_stream, target_role, &state).await {
                        // Connection lost to target is normal when quest isn't connected yet.
                        if !matches!(e.downcast_ref::<quinn::ConnectionError>(), Some(_)) {
                            warn!("forward_uni {role:?}→{target_role:?}: {e:#}");
                        }
                    }
                });
            }
        })
    };

    // Task 2: Forward incoming bidirectional streams (control) to the other peer.
    let bidi_fwd = {
        let conn = conn2;
        let state = state2;
        tokio::spawn(async move {
            loop {
                let (send_back, recv_stream) = match conn.accept_bi().await {
                    Ok(s) => s,
                    Err(quinn::ConnectionError::ApplicationClosed(_)) => break,
                    Err(e) => {
                        warn!("{role:?} accept_bi error: {e}");
                        break;
                    }
                };
                let state = state.clone();
                tokio::spawn(async move {
                    if let Err(e) = forward_bidi_stream(recv_stream, send_back, role, target_role, &state).await {
                        warn!("forward_bidi {role:?}→{target_role:?}: {e:#}");
                    }
                });
            }
        })
    };

    // Wait for connection to close.
    let reason = conn.closed().await;
    info!("{role:?} disconnected from {remote}: {reason}");
    state.clear_peer(role);
    uni_fwd.abort();
    bidi_fwd.abort();
    Ok(())
}

/// Stream a unidirectional stream directly to the target peer (zero-copy relay).
/// Opens a matching uni stream on the target and pipes bytes as they arrive.
async fn forward_uni_stream(
    mut recv: quinn::RecvStream,
    target_role: Role,
    state: &RelayState,
) -> Result<()> {
    // Get target connection first so we can start streaming immediately.
    let target_conn = match state.get_peer(target_role) {
        Some(c) => c,
        None => {
            // No peer connected yet; drain and discard.
            let _ = recv.read_to_end(8 * 1024 * 1024).await;
            return Ok(());
        }
    };

    let mut send = target_conn.open_uni().await.context("open_uni to target")?;

    // Stream bytes directly from recv → send without buffering the entire frame.
    tokio::io::copy(&mut recv, &mut send)
        .await
        .context("streaming copy")?;

    send.finish().context("finish uni stream")?;
    Ok(())
}

/// Bridge a bidirectional stream: read from `recv` and forward to a new bidi stream
/// on the target peer's connection.  Also pipe the return direction.
async fn forward_bidi_stream(
    mut recv: quinn::RecvStream,
    mut send_back: quinn::SendStream,
    _from_role: Role,
    target_role: Role,
    state: &RelayState,
) -> Result<()> {
    // Wait for target peer to be available.
    let target_conn = loop {
        if let Some(c) = state.get_peer(target_role) {
            break c;
        }
        // Poll every 100ms.
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    };

    let (mut target_send, mut target_recv) = target_conn
        .open_bi()
        .await
        .context("open_bi to target")?;

    // Forward from_role → target
    let fwd_a = tokio::spawn(async move {
        let mut buf = vec![0u8; 8192];
        loop {
            match recv.read(&mut buf).await {
                Ok(Some(n)) => {
                    if target_send.write_all(&buf[..n]).await.is_err() {
                        break;
                    }
                }
                Ok(None) => break, // stream finished
                Err(_) => break,
            }
        }
        let _ = target_send.finish();
    });

    // Forward target → from_role (return path)
    let fwd_b = tokio::spawn(async move {
        let mut buf = vec![0u8; 8192];
        loop {
            match target_recv.read(&mut buf).await {
                Ok(Some(n)) => {
                    if send_back.write_all(&buf[..n]).await.is_err() {
                        break;
                    }
                }
                Ok(None) => break,
                Err(_) => break,
            }
        }
        let _ = send_back.finish();
    });

    let _ = tokio::join!(fwd_a, fwd_b);
    Ok(())
}
