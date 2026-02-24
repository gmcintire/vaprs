// Central packet router - receives packets from all interfaces via a single
// mpsc channel and fans out to registered consumers.
//
// Each consumer gets its own mpsc channel. The router clones the Arc<Packet>
// (SharedPacket) for each consumer, which is cheap since it's just a reference
// count bump. If a consumer's channel is full, the packet is dropped for that
// consumer (using try_send) to avoid backpressure from one slow consumer
// blocking the entire system.

use tokio::sync::mpsc;
use tracing::{debug, warn};

use crate::packet::SharedPacket;

/// A consumer that receives packets from the router.
pub struct Consumer {
    pub name: String,
    pub tx: mpsc::Sender<SharedPacket>,
}

/// The central packet router.
///
/// Receives packets from all sources on a single channel and fans them out
/// to all registered consumers.
pub struct Router {
    /// Receive from all sources.
    rx: mpsc::Receiver<SharedPacket>,
    /// Fan out to all consumers.
    consumers: Vec<Consumer>,
}

impl Router {
    /// Create a new router that receives packets from the given channel.
    pub fn new(rx: mpsc::Receiver<SharedPacket>) -> Self {
        Self {
            rx,
            consumers: Vec::new(),
        }
    }

    /// Register a consumer, returns its receiver.
    ///
    /// The consumer will receive cloned copies of every packet that arrives
    /// at the router. If the consumer's channel fills up, packets are dropped
    /// for that consumer.
    pub fn add_consumer(&mut self, name: &str, buffer_size: usize) -> mpsc::Receiver<SharedPacket> {
        let (tx, rx) = mpsc::channel(buffer_size);
        self.consumers.push(Consumer {
            name: name.to_string(),
            tx,
        });
        rx
    }

    /// Run the router, receiving packets and fanning out to all consumers.
    ///
    /// The router runs until the source channel is closed (all senders dropped).
    pub async fn run(mut self) {
        debug!(consumers = self.consumers.len(), "router started");

        while let Some(packet) = self.rx.recv().await {
            for consumer in &self.consumers {
                match consumer.tx.try_send(packet.clone()) {
                    Ok(()) => {}
                    Err(mpsc::error::TrySendError::Full(_)) => {
                        warn!(
                            consumer = consumer.name,
                            "consumer channel full, dropping packet"
                        );
                    }
                    Err(mpsc::error::TrySendError::Closed(_)) => {
                        debug!(consumer = consumer.name, "consumer channel closed");
                    }
                }
            }
        }

        debug!("router stopped (source channel closed)");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::packet::Packet;
    use std::sync::Arc;
    use std::time::Duration;

    /// Helper to create a test packet.
    fn test_packet(tnc2: &str) -> SharedPacket {
        Arc::new(Packet::new(tnc2, "test", true))
    }

    #[tokio::test]
    async fn router_delivers_to_single_consumer() {
        let (source_tx, source_rx) = mpsc::channel(32);
        let mut router = Router::new(source_rx);
        let mut consumer_rx = router.add_consumer("test", 32);

        tokio::spawn(async move { router.run().await });

        let pkt = test_packet("TEST>APRS:!4903.50N/07201.75W-");
        source_tx.send(pkt).await.unwrap();
        drop(source_tx);

        let received = consumer_rx.recv().await.unwrap();
        assert_eq!(received.source_call(), "TEST");
        assert_eq!(received.payload(), "!4903.50N/07201.75W-");
    }

    #[tokio::test]
    async fn router_delivers_to_multiple_consumers() {
        let (source_tx, source_rx) = mpsc::channel(32);
        let mut router = Router::new(source_rx);
        let mut consumer_a = router.add_consumer("consumer_a", 32);
        let mut consumer_b = router.add_consumer("consumer_b", 32);
        let mut consumer_c = router.add_consumer("consumer_c", 32);

        tokio::spawn(async move { router.run().await });

        let pkt = test_packet("OH2MQK-1>APRS:!6029.50N/02505.43E>");
        source_tx.send(pkt).await.unwrap();
        drop(source_tx);

        let a = consumer_a.recv().await.unwrap();
        let b = consumer_b.recv().await.unwrap();
        let c = consumer_c.recv().await.unwrap();

        assert_eq!(a.source_call(), "OH2MQK-1");
        assert_eq!(b.source_call(), "OH2MQK-1");
        assert_eq!(c.source_call(), "OH2MQK-1");
    }

    #[tokio::test]
    async fn router_stops_when_source_channel_closes() {
        let (source_tx, source_rx) = mpsc::channel(32);
        let mut router = Router::new(source_rx);
        let mut consumer_rx = router.add_consumer("test", 32);

        let handle = tokio::spawn(async move { router.run().await });

        // Send one packet then close the source
        let pkt = test_packet("TEST>APRS:data");
        source_tx.send(pkt).await.unwrap();
        drop(source_tx);

        // Router should stop, which means consumer channel eventually closes
        tokio::time::timeout(Duration::from_secs(2), handle)
            .await
            .expect("router did not stop after source closed")
            .expect("router task panicked");

        // Drain the one packet
        let _ = consumer_rx.recv().await;
        // Next recv should return None (channel closed)
        assert!(consumer_rx.recv().await.is_none());
    }

    #[tokio::test]
    async fn consumer_channel_full_causes_packet_drop() {
        let (source_tx, source_rx) = mpsc::channel(32);
        let mut router = Router::new(source_rx);
        // Very small buffer so it fills up
        let mut consumer_rx = router.add_consumer("slow", 1);

        tokio::spawn(async move { router.run().await });

        // Send multiple packets rapidly
        for i in 0..10 {
            let pkt = test_packet(&format!("SRC{}>APRS:data{}", i, i));
            source_tx.send(pkt).await.unwrap();
        }
        drop(source_tx);

        // Consumer should receive some packets but not all 10
        let mut received = Vec::new();
        while let Some(pkt) = consumer_rx.recv().await {
            received.push(pkt);
        }

        assert!(
            !received.is_empty(),
            "should have received at least one packet"
        );
        assert!(
            received.len() < 10,
            "should have dropped some packets, but got all {}",
            received.len()
        );
    }

    #[tokio::test]
    async fn router_delivers_multiple_packets_in_order() {
        let (source_tx, source_rx) = mpsc::channel(32);
        let mut router = Router::new(source_rx);
        let mut consumer_rx = router.add_consumer("test", 32);

        tokio::spawn(async move { router.run().await });

        for i in 0..5 {
            let pkt = test_packet(&format!("SRC{}>APRS:data{}", i, i));
            source_tx.send(pkt).await.unwrap();
        }
        drop(source_tx);

        for i in 0..5 {
            let pkt = consumer_rx.recv().await.unwrap();
            let expected_source = format!("SRC{}", i);
            assert_eq!(pkt.source_call(), expected_source);
        }
    }

    #[tokio::test]
    async fn router_with_no_consumers_does_not_panic() {
        let (source_tx, source_rx) = mpsc::channel(32);
        let router = Router::new(source_rx);

        let handle = tokio::spawn(async move { router.run().await });

        let pkt = test_packet("TEST>APRS:data");
        source_tx.send(pkt).await.unwrap();
        drop(source_tx);

        tokio::time::timeout(Duration::from_secs(2), handle)
            .await
            .expect("router did not stop")
            .expect("router panicked");
    }

    #[tokio::test]
    async fn router_continues_when_one_consumer_closes() {
        let (source_tx, source_rx) = mpsc::channel(32);
        let mut router = Router::new(source_rx);
        let consumer_a = router.add_consumer("dropped", 32);
        let mut consumer_b = router.add_consumer("alive", 32);

        tokio::spawn(async move { router.run().await });

        // Drop consumer A's receiver
        drop(consumer_a);

        // Send a packet - should still reach consumer B
        let pkt = test_packet("TEST>APRS:data");
        source_tx.send(pkt).await.unwrap();
        drop(source_tx);

        let received = consumer_b.recv().await.unwrap();
        assert_eq!(received.source_call(), "TEST");
    }
}
