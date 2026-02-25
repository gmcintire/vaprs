// Integration test: verifies packets flow through the router from source to consumer.
//
// This is a unit-level integration test (not full end-to-end with real serial ports).
// It creates an in-memory config, sets up a router with a test consumer, feeds
// a test packet, and verifies the packet arrives at the consumer.

use std::sync::Arc;
use std::time::Duration;

use tokio::sync::mpsc;

use vaprs::packet::{Packet, SharedPacket};
use vaprs::router::Router;

#[tokio::test]
async fn test_packet_flows_through_router() {
    let (source_tx, source_rx) = mpsc::channel::<SharedPacket>(32);
    let mut router = Router::new(source_rx);
    let mut consumer_rx = router.add_consumer("test", 32);

    tokio::spawn(async move { router.run().await });

    let pkt = Arc::new(Packet::new(
        "TEST>APRS:!4903.50N/07201.75W-",
        "radio0",
        true,
    ));
    source_tx.send(pkt).await.unwrap();
    drop(source_tx); // close to let router stop

    let received = tokio::time::timeout(Duration::from_secs(2), consumer_rx.recv())
        .await
        .expect("timed out waiting for packet")
        .expect("channel closed");

    assert_eq!(received.source_call(), "TEST");
    assert_eq!(received.dest_call(), "APRS");
    assert_eq!(received.payload(), "!4903.50N/07201.75W-");
    assert_eq!(received.source_interface, "radio0");
    assert!(received.is_aprs);
}

#[tokio::test]
async fn test_multiple_sources_fan_into_router() {
    let (source_tx, source_rx) = mpsc::channel::<SharedPacket>(32);
    let mut router = Router::new(source_rx);
    let mut consumer_rx = router.add_consumer("collector", 32);

    tokio::spawn(async move { router.run().await });

    // Simulate packets from different interfaces
    let source_a = source_tx.clone();
    let source_b = source_tx.clone();
    drop(source_tx);

    let pkt_a = Arc::new(Packet::new("ALPHA>APRS:!position_a", "serial0", true));
    let pkt_b = Arc::new(Packet::new("BRAVO>APRS:!position_b", "tcp0", true));

    source_a.send(pkt_a).await.unwrap();
    source_b.send(pkt_b).await.unwrap();
    drop(source_a);
    drop(source_b);

    let first = tokio::time::timeout(Duration::from_secs(2), consumer_rx.recv())
        .await
        .expect("timed out")
        .expect("closed");
    assert_eq!(first.source_call(), "ALPHA");
    assert_eq!(first.source_interface, "serial0");

    let second = tokio::time::timeout(Duration::from_secs(2), consumer_rx.recv())
        .await
        .expect("timed out")
        .expect("closed");
    assert_eq!(second.source_call(), "BRAVO");
    assert_eq!(second.source_interface, "tcp0");
}

#[tokio::test]
async fn test_router_fan_out_to_multiple_consumers() {
    let (source_tx, source_rx) = mpsc::channel::<SharedPacket>(32);
    let mut router = Router::new(source_rx);
    let mut igate_rx = router.add_consumer("igate", 32);
    let mut logger_rx = router.add_consumer("logger", 32);
    let mut digipeater_rx = router.add_consumer("digipeater", 32);

    tokio::spawn(async move { router.run().await });

    let pkt = Arc::new(Packet::new(
        "OH2MQK-1>APRS,WIDE1-1:!6029.50N/02505.43E>",
        "serial0",
        true,
    ));
    source_tx.send(pkt).await.unwrap();
    drop(source_tx);

    // All three consumers should receive the same packet
    let igate_pkt = tokio::time::timeout(Duration::from_secs(2), igate_rx.recv())
        .await
        .expect("timed out")
        .expect("closed");
    let logger_pkt = tokio::time::timeout(Duration::from_secs(2), logger_rx.recv())
        .await
        .expect("timed out")
        .expect("closed");
    let digi_pkt = tokio::time::timeout(Duration::from_secs(2), digipeater_rx.recv())
        .await
        .expect("timed out")
        .expect("closed");

    assert_eq!(igate_pkt.source_call(), "OH2MQK-1");
    assert_eq!(logger_pkt.source_call(), "OH2MQK-1");
    assert_eq!(digi_pkt.source_call(), "OH2MQK-1");

    // All point to the same underlying data (Arc)
    assert_eq!(igate_pkt.tnc2, logger_pkt.tnc2);
    assert_eq!(logger_pkt.tnc2, digi_pkt.tnc2);
}

#[tokio::test]
async fn test_igate_filtering_through_router() {
    // End-to-end test: packet -> router -> igate consumer -> gate_to_aprsis
    let (source_tx, source_rx) = mpsc::channel::<SharedPacket>(32);
    let mut router = Router::new(source_rx);
    let mut igate_rx = router.add_consumer("igate", 32);

    tokio::spawn(async move { router.run().await });

    // Send a valid APRS packet
    let pkt = Arc::new(Packet::new(
        "OH2MQK-1>APRS,WIDE1-1*:!6029.50N/02505.43E>Rx-only iGate",
        "serial0",
        true,
    ));
    source_tx.send(pkt).await.unwrap();
    drop(source_tx);

    let received = tokio::time::timeout(Duration::from_secs(2), igate_rx.recv())
        .await
        .expect("timed out")
        .expect("closed");

    // Apply iGate filtering
    let gated = vaprs::igate::gate_to_aprsis(&received, "MYGATE-10");
    match gated {
        vaprs::igate::GateResult::Gated(line) => {
            assert!(line.contains("qAR,MYGATE-10"));
            assert!(line.contains("OH2MQK-1>APRS"));
            assert!(line.contains("!6029.50N/02505.43E>Rx-only iGate"));
        }
        other => panic!("expected Gated, got {:?}", other),
    }
}

#[tokio::test]
async fn test_forbidden_packet_filtered_by_igate() {
    let (source_tx, source_rx) = mpsc::channel::<SharedPacket>(32);
    let mut router = Router::new(source_rx);
    let mut igate_rx = router.add_consumer("igate", 32);

    tokio::spawn(async move { router.run().await });

    // Send a packet with RFONLY in the path (should be filtered)
    let pkt = Arc::new(Packet::new("TEST>APRS,RFONLY:!position", "serial0", true));
    source_tx.send(pkt).await.unwrap();
    drop(source_tx);

    let received = tokio::time::timeout(Duration::from_secs(2), igate_rx.recv())
        .await
        .expect("timed out")
        .expect("closed");

    // iGate should filter this out
    let gated = vaprs::igate::gate_to_aprsis(&received, "MYGATE");
    assert!(
        !matches!(gated, vaprs::igate::GateResult::Gated(_)),
        "RFONLY packet should not be gated"
    );
}
