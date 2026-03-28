//! Stable C ABI (`include/ltp.h`). All framing goes through Rust session (no duplicate packet builders).

use std::ffi::CStr;
use std::net::SocketAddr;
use std::os::raw::{c_char, c_int};
use std::ptr;
use std::slice;
use std::sync::atomic::{AtomicU32, Ordering};

use crate::control_envelope::{ControlEnvelope, SCHEMA_TWIST};
use crate::header::{LtpHeader, PriorityClass, PayloadType};
use crate::path::PathConfig;
use crate::scheduler::ScheduledPacket;
use crate::session::{build_control_datagram, Session, SessionConfig};

pub const LTP_ABI_VERSION: u32 = 1;

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
    Box::into_raw(Box::new(LtpSessionHandle { inner: session }))
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
    sess.inner.drain_ingress(true, now_ns);
    set_err(LtpErr::Ok)
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
