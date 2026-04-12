//! LTP1 teleoperation transport: UDP framing, multi-path hooks, scheduling, FEC helpers, C ABI.

pub mod bonding;
pub mod control_envelope;
pub mod fec;
pub mod framer;
pub mod header;
pub mod ingress;
pub mod metrics;
pub mod path;
pub mod receive;
pub mod scheduler;
pub mod session;
pub mod shmem;
pub mod video_slice;

mod ffi;

pub use control_envelope::SCHEMA_QUEST_TELEOP112;
pub use header::{LtpHeader, PayloadType, PriorityClass, LTP_MAGIC, LTP_VERSION};
pub use session::{Session, SessionConfig};

pub use ffi::{
    ltp_abi_version, ltp_last_error, ltp_poll_recv, ltp_recv_pop, ltp_recv_video_pending_bytes,
    ltp_send_control, ltp_send_twist_stub, ltp_session_create, ltp_session_destroy, ltp_config,
    LtpSessionHandle, LTP_ABI_VERSION,
};

#[cfg(test)]
mod tests_lib {
    use super::header::{LtpHeader, LTP_MAGIC};

    #[test]
    fn reject_bad_magic() {
        let mut b = vec![0u8; 32];
        b[0..4].copy_from_slice(&(LTP_MAGIC ^ 1).to_be_bytes());
        assert!(LtpHeader::parse(&b).is_err());
    }

    #[test]
    fn reject_bad_version() {
        let mut h = LtpHeader::default();
        h.version = 9;
        let mut w = Vec::new();
        h.write_into(&mut w);
        assert!(LtpHeader::parse(&w).is_err());
    }

    #[test]
    fn reject_payload_len_overflow() {
        let mut h = LtpHeader::default();
        h.payload_len = 1000;
        let mut w = Vec::new();
        h.write_into(&mut w);
        w.extend_from_slice(&[0u8; 10]);
        let (hdr, _) = LtpHeader::parse(&w).unwrap();
        assert!(hdr.validate_total_len(w.len()).is_err());
    }
}
