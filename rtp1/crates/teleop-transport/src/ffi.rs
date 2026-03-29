//! Stable C ABI (`include/ltp.h`). All framing goes through Rust session (no duplicate packet builders).

use std::collections::VecDeque;
use std::ffi::CStr;
use std::net::SocketAddr;
use std::os::raw::{c_char, c_int};
use std::ptr;
use std::slice;
use std::sync::atomic::{AtomicU32, Ordering};

use crate::control_envelope::{ControlEnvelope, SCHEMA_TWIST};
use crate::header::{LtpHeader, PriorityClass, PayloadType};
use crate::path::PathConfig;
use crate::receive::ReceivedEvent;
use crate::scheduler::ScheduledPacket;
use crate::session::{build_control_datagram, Session, SessionConfig};

pub const LTP_ABI_VERSION: u32 = 2;

const RECV_QUEUE_MAX: usize = 64;

enum RecvItem {
    Control { schema_id: u16, payload: Vec<u8> },
    VideoJpeg(Vec<u8>),
}

fn push_recv(h: &mut LtpSessionHandle, item: RecvItem) {
    while h.recv_queue.len() >= RECV_QUEUE_MAX {
        h.recv_queue.pop_front();
    }
    h.recv_queue.push_back(item);
}

fn enqueue_events(h: &mut LtpSessionHandle, events: Vec<ReceivedEvent>) {
    for ev in events {
        match ev {
            ReceivedEvent::Duplicate => {}
            ReceivedEvent::ControlOrdered(chunks) => {
                for chunk in chunks {
                    if let Ok(env) = ControlEnvelope::decode(&chunk) {
                        push_recv(
                            h,
                            RecvItem::Control {
                                schema_id: env.schema_id,
                                payload: env.payload,
                            },
                        );
                    }
                }
            }
            ReceivedEvent::VideoProgress { frame } => {
                if let Some(jpeg) = frame {
                    push_recv(h, RecvItem::VideoJpeg(jpeg));
                }
            }
            ReceivedEvent::Telemetry(_) => {}
        }
    }
}

static LAST_ERROR: AtomicU32 = AtomicU32::new(0);

#[repr(i32)]
pub enum LtpErr {
    Ok = 0,
    Inval = 1,
    Io = 2,
    Abi = 3,
    Again = 4,
}

fn set_err(e: LtpErr) -> c_int {
    let code = e as u32;
    LAST_ERROR.store(code, Ordering::Relaxed);
    code as c_int
}

#[no_mangle]
pub extern "C" fn ltp_last_error() -> u32 {
    LAST_ERROR.load(Ordering::Relaxed)
}

#[no_mangle]
pub extern "C" fn ltp_abi_version() -> u32 {
    LTP_ABI_VERSION
}

#[repr(C)]
pub struct ltp_config {
    pub bind_port: u16,
    pub peer_port: u16,
    pub bind_host: *const c_char,
    pub peer_host: *const c_char,
}

pub struct LtpSessionHandle {
    inner: Session,
    recv_queue: VecDeque<RecvItem>,
}

fn parse_addr(host: *const c_char, port: u16) -> Result<SocketAddr, LtpErr> {
    if host.is_null() {
        return Err(LtpErr::Inval);
    }
    let s = unsafe { CStr::from_ptr(host) }.to_str().map_err(|_| LtpErr::Inval)?;
    let ip: std::net::IpAddr = s.parse().map_err(|_| LtpErr::Inval)?;
    Ok(SocketAddr::new(ip, port))
}

#[no_mangle]
pub extern "C" fn ltp_session_create(cfg: *const ltp_config) -> *mut LtpSessionHandle {
    if cfg.is_null() {
        set_err(LtpErr::Inval);
        return ptr::null_mut();
    }
    let cfg = unsafe { &*cfg };
    let bind = match parse_addr(cfg.bind_host, cfg.bind_port) {
        Ok(a) => a,
        Err(e) => {
            set_err(e);
            return ptr::null_mut();
        }
    };
    let peer = match parse_addr(cfg.peer_host, cfg.peer_port) {
        Ok(a) => a,
        Err(e) => {
            set_err(e);
            return ptr::null_mut();
        }
    };
    let path = PathConfig {
        path_index: 0,
        path_tag: 1,
        bind_addr: bind,
    };
    let mut sc = SessionConfig::default();
    sc.peer = peer;
    let session = match Session::new(sc, vec![path]) {
        Ok(s) => s,
        Err(_) => {
            set_err(LtpErr::Io);
            return ptr::null_mut();
        }
    };
    set_err(LtpErr::Ok);
    Box::into_raw(Box::new(LtpSessionHandle {
        inner: session,
        recv_queue: VecDeque::new(),
    }))
}

#[no_mangle]
pub extern "C" fn ltp_session_destroy(p: *mut LtpSessionHandle) {
    if p.is_null() {
        return;
    }
    unsafe {
        drop(Box::from_raw(p));
    }
}

#[no_mangle]
pub extern "C" fn ltp_send_control(
    p: *mut LtpSessionHandle,
    stream_id: u16,
    schema_id: u16,
    data: *const u8,
    len: usize,
    seq: u32,
    timestamp_ns: u64,
) -> c_int {
    if p.is_null() || (data.is_null() && len > 0) {
        return set_err(LtpErr::Inval);
    }
    let sess = unsafe { &mut *p };
    let payload = unsafe { slice::from_raw_parts(data, len) };
    let env = ControlEnvelope {
        schema_id,
        payload: payload.to_vec(),
    };
    let body = env.encode();
    let mut hdr = LtpHeader::default();
    hdr.stream_id = stream_id;
    hdr.seq = seq;
    hdr.timestamp_ns = timestamp_ns;
    hdr.payload_type = PayloadType::Control;
    hdr.priority = PriorityClass::Control;
    let dg = match build_control_datagram(&mut hdr, &body) {
        Ok(d) => d,
        Err(_) => return set_err(LtpErr::Inval),
    };
    sess.inner.enqueue_raw_datagram(ScheduledPacket {
        priority: PriorityClass::Control,
        payload_type: PayloadType::Control,
        datagram: dg,
        deadline_key: timestamp_ns,
    });
    match sess.inner.flush_send() {
        Ok(()) => set_err(LtpErr::Ok),
        Err(_) => set_err(LtpErr::Io),
    }
}

#[no_mangle]
pub extern "C" fn ltp_poll_recv(p: *mut LtpSessionHandle, now_ns: u64) -> c_int {
    if p.is_null() {
        return set_err(LtpErr::Inval);
    }
    let sess = unsafe { &mut *p };
    if let Err(_) = sess.inner.poll_ingress() {
        return set_err(LtpErr::Io);
    }
    let events = sess.inner.drain_ingress(true, now_ns);
    enqueue_events(sess, events);
    set_err(LtpErr::Ok)
}

/// Pop one received payload queued by `ltp_poll_recv`.
/// Returns: `1` ok, `0` empty, `-1` invalid args, `-2` buffer too small (`*out_len` = required bytes; item left queued).
#[no_mangle]
pub extern "C" fn ltp_recv_pop(
    p: *mut LtpSessionHandle,
    out_kind: *mut c_int,
    out_schema_id: *mut u16,
    buf: *mut u8,
    cap: usize,
    out_len: *mut usize,
) -> c_int {
    if p.is_null() || out_kind.is_null() || out_schema_id.is_null() || out_len.is_null() {
        LAST_ERROR.store(LtpErr::Inval as u32, Ordering::Relaxed);
        return -1;
    }
    if buf.is_null() && cap > 0 {
        LAST_ERROR.store(LtpErr::Inval as u32, Ordering::Relaxed);
        return -1;
    }
    let h = unsafe { &mut *p };
    if h.recv_queue.is_empty() {
        return 0;
    }
    let need = match h.recv_queue.front().unwrap() {
        RecvItem::Control { payload, .. } => payload.len(),
        RecvItem::VideoJpeg(p) => p.len(),
    };
    if need > cap {
        unsafe {
            *out_len = need;
        }
        return -2;
    }
    let item = h.recv_queue.pop_front().unwrap();
    let (kind, schema, src) = match item {
        RecvItem::Control { schema_id, payload } => (0i32, schema_id, payload),
        RecvItem::VideoJpeg(payload) => (1i32, 0u16, payload),
    };
    unsafe {
        if !buf.is_null() && !src.is_empty() {
            ptr::copy_nonoverlapping(src.as_ptr(), buf, src.len());
        }
        *out_kind = kind;
        *out_schema_id = schema;
        *out_len = src.len();
    }
    1
}

#[no_mangle]
pub extern "C" fn ltp_send_twist_stub(p: *mut LtpSessionHandle, stream_id: u16, seq: u32) -> c_int {
    let stub = [0f32; 6];
    let bytes: Vec<u8> = stub.iter().flat_map(|f| f.to_le_bytes()).collect();
    ltp_send_control(
        p,
        stream_id,
        SCHEMA_TWIST,
        bytes.as_ptr(),
        bytes.len(),
        seq,
        0,
    )
}
