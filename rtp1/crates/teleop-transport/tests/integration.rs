use std::net::UdpSocket;

use teleop_transport::control_envelope::{ControlEnvelope, SCHEMA_TWIST};
use teleop_transport::framer::split_header_payload;
use teleop_transport::header::{LtpHeader, PriorityClass, PayloadType};
use teleop_transport::path::PathConfig;
use teleop_transport::receive::ReceiveDemux;
use teleop_transport::scheduler::{PriorityScheduler, ScheduledPacket};
use teleop_transport::session::{build_control_datagram, Session, SessionConfig};

#[test]
fn duplicate_path_sends_two_dgrams_loopback() {
    let peer = UdpSocket::bind("127.0.0.1:0").unwrap();
    peer.set_read_timeout(Some(std::time::Duration::from_secs(2))).unwrap();
    let peer_addr = peer.local_addr().unwrap();

    let p1 = PathConfig {
        path_index: 0,
        path_tag: 0x100,
        bind_addr: "127.0.0.1:0".parse().unwrap(),
    };
    let p2 = PathConfig {
        path_index: 1,
        path_tag: 0x200,
        bind_addr: "127.0.0.1:0".parse().unwrap(),
    };
    let mut cfg = SessionConfig::default();
    cfg.peer = peer_addr;
    let mut s = Session::new(cfg, vec![p1, p2]).unwrap();

    let env = ControlEnvelope {
        schema_id: SCHEMA_TWIST,
        payload: vec![1, 2, 3],
    };
    let body = env.encode();
    let mut hdr = LtpHeader::default();
    hdr.stream_id = 7;
    hdr.seq = 1;
    let dg = build_control_datagram(&mut hdr, &body).unwrap();
    s.enqueue_raw_datagram(ScheduledPacket {
        priority: PriorityClass::Control,
        payload_type: PayloadType::Control,
        datagram: dg,
        deadline_key: 0,
    });
    s.flush_send(0).unwrap();

    let mut b1 = [0u8; 2048];
    let mut b2 = [0u8; 2048];
    let (n1, _) = peer.recv_from(&mut b1).unwrap();
    let (n2, _) = peer.recv_from(&mut b2).unwrap();
    let (h1, p1) = split_header_payload(&b1[..n1]).unwrap();
    let (h2, p2) = split_header_payload(&b2[..n2]).unwrap();
    assert_ne!(h1.path_tag, h2.path_tag);
    assert_eq!(h1.seq, h2.seq);
    assert_eq!(p1, p2);
}

#[test]
fn dedup_second_copy_dropped() {
    let mut d = ReceiveDemux::new(1024, 8);
    let mut h = LtpHeader::default();
    h.stream_id = 1;
    h.seq = 5;
    h.payload_type = PayloadType::Control;
    h.priority = PriorityClass::Control;
    h.payload_len = 0;
    let ev1 = d.handle_datagram(h.clone(), vec![], 0, true);
    let ev2 = d.handle_datagram(h, vec![], 0, true);
    match ev2 {
        teleop_transport::receive::ReceivedEvent::Duplicate => {}
        _ => panic!("expected duplicate"),
    }
    assert!(matches!(
        ev1,
        teleop_transport::receive::ReceivedEvent::ControlOrdered(_)
    ));
}

#[test]
fn control_precedence_under_contention() {
    let mut sch = PriorityScheduler::default();
    sch.enqueue(ScheduledPacket {
        priority: PriorityClass::Video,
        payload_type: PayloadType::VideoSlice,
        datagram: vec![1],
        deadline_key: 1,
    });
    sch.enqueue(ScheduledPacket {
        priority: PriorityClass::Control,
        payload_type: PayloadType::Control,
        datagram: vec![2],
        deadline_key: 99,
    });
    let first = sch.pop_next().unwrap();
    assert_eq!(first.datagram[0], 2);
}

#[test]
fn control_ordered_after_reorder() {
    let mut d = ReceiveDemux::new(1024, 32);
    let mut h1 = LtpHeader::default();
    h1.stream_id = 9;
    h1.seq = 2;
    h1.payload_type = PayloadType::Control;
    h1.priority = PriorityClass::Control;
    h1.payload_len = 1;
    let mut h0 = h1.clone();
    h0.seq = 1;
    h0.payload_len = 1;

    let e0 = d.handle_datagram(h0, vec![10], 0, false);
    let e1 = d.handle_datagram(h1, vec![11], 0, false);
    let mut flat: Vec<Vec<u8>> = Vec::new();
    if let teleop_transport::receive::ReceivedEvent::ControlOrdered(v) = e0 {
        flat.extend(v);
    }
    if let teleop_transport::receive::ReceivedEvent::ControlOrdered(v) = e1 {
        flat.extend(v);
    }
    assert_eq!(flat, vec![vec![10], vec![11]]);
}

#[test]
fn split_header_round_trip() {
    let mut h = LtpHeader::default();
    h.payload_len = 3;
    let mut w = Vec::new();
    h.write_into(&mut w);
    w.extend_from_slice(&[9u8, 8, 7]);
    let (h2, pl) = split_header_payload(&w).unwrap();
    assert_eq!(h2.payload_len, 3);
    assert_eq!(pl, &[9, 8, 7]);
}
