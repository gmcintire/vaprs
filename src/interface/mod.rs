// Interface trait, command types, and registry for managing APRS interfaces
//
// Each interface (serial, TCP, AX.25, AGWPE) runs as an async task communicating
// via mpsc channels. The registry holds handles for commanding each interface.

pub mod agwpe;
pub mod ax25_kernel;
pub mod serial;
pub mod tcp;

use std::future::Future;
use std::pin::Pin;

use tokio::sync::mpsc;

use crate::config::InterfaceType;
use crate::packet::SharedPacket;

/// Commands sent to a running interface task.
#[derive(Debug)]
pub enum InterfaceCommand {
    /// Transmit a packet out this interface.
    Transmit(SharedPacket),
    /// Gracefully shut down this interface.
    Shutdown,
}

/// Metadata describing an interface, used by the registry and trait implementors.
#[derive(Debug, Clone)]
pub struct InterfaceMetadata {
    /// Callsign associated with this interface (e.g., "OH2MQK-1").
    pub callsign: String,
    /// Type of interface (Serial, Tcp, Ax25, Agwpe, Null).
    pub iface_type: InterfaceType,
    /// Whether transmitting is allowed on this interface.
    pub tx_ok: bool,
    /// iGate group number for routing decisions.
    pub igate_group: u8,
}

/// Trait that all interface implementations must satisfy.
///
/// Each interface runs as an async task, receiving commands on `cmd_rx`
/// and sending decoded packets on `packet_tx`. The `run` method consumes
/// the interface and drives it until shutdown or error.
pub trait Interface: Send {
    /// Returns metadata describing this interface.
    fn metadata(&self) -> &InterfaceMetadata;

    /// Run the interface task to completion.
    ///
    /// The implementation should:
    /// - Read from its underlying transport (serial port, TCP socket, etc.)
    /// - Decode packets and send them on `packet_tx`
    /// - Listen for commands on `cmd_rx` (Transmit, Shutdown)
    /// - Return when shut down or on fatal error
    fn run(
        self: Box<Self>,
        packet_tx: mpsc::Sender<SharedPacket>,
        cmd_rx: mpsc::Receiver<InterfaceCommand>,
    ) -> Pin<Box<dyn Future<Output = ()> + Send>>;
}

/// Handle to a running interface task, held by the registry.
///
/// Contains the interface metadata and a channel sender for commanding
/// the interface task.
#[derive(Debug)]
pub struct InterfaceHandle {
    /// Metadata describing this interface.
    pub metadata: InterfaceMetadata,
    /// Channel for sending commands to the interface task.
    pub cmd_tx: mpsc::Sender<InterfaceCommand>,
}

impl InterfaceHandle {
    /// Create a new handle from metadata and a command sender.
    pub fn new(metadata: InterfaceMetadata, cmd_tx: mpsc::Sender<InterfaceCommand>) -> Self {
        Self { metadata, cmd_tx }
    }

    /// Send a transmit command to this interface.
    ///
    /// Returns `Ok(())` if the command was queued, or `Err` if the
    /// interface task has shut down.
    pub async fn transmit(
        &self,
        packet: SharedPacket,
    ) -> Result<(), mpsc::error::SendError<InterfaceCommand>> {
        self.cmd_tx.send(InterfaceCommand::Transmit(packet)).await
    }

    /// Send a shutdown command to this interface.
    ///
    /// Returns `Ok(())` if the command was queued, or `Err` if the
    /// interface task has already shut down.
    pub async fn shutdown(&self) -> Result<(), mpsc::error::SendError<InterfaceCommand>> {
        self.cmd_tx.send(InterfaceCommand::Shutdown).await
    }
}

/// Registry that manages all active interface handles.
///
/// Provides lookup by callsign and bulk operations like shutdown-all.
#[derive(Debug, Default)]
pub struct InterfaceRegistry {
    handles: Vec<InterfaceHandle>,
}

impl InterfaceRegistry {
    /// Create an empty registry.
    pub fn new() -> Self {
        Self {
            handles: Vec::new(),
        }
    }

    /// Register a new interface handle.
    pub fn register(&mut self, handle: InterfaceHandle) {
        self.handles.push(handle);
    }

    /// Find the first interface handle matching the given callsign.
    pub fn find_by_callsign(&self, callsign: &str) -> Option<&InterfaceHandle> {
        self.handles
            .iter()
            .find(|h| h.metadata.callsign == callsign)
    }

    /// Find all interface handles that can transmit.
    pub fn tx_capable(&self) -> Vec<&InterfaceHandle> {
        self.handles.iter().filter(|h| h.metadata.tx_ok).collect()
    }

    /// Find all interface handles in a given igate group.
    pub fn by_igate_group(&self, group: u8) -> Vec<&InterfaceHandle> {
        self.handles
            .iter()
            .filter(|h| h.metadata.igate_group == group)
            .collect()
    }

    /// Return how many interfaces are registered.
    pub fn len(&self) -> usize {
        self.handles.len()
    }

    /// Return whether the registry is empty.
    pub fn is_empty(&self) -> bool {
        self.handles.is_empty()
    }

    /// Iterate over all handles.
    pub fn iter(&self) -> std::slice::Iter<'_, InterfaceHandle> {
        self.handles.iter()
    }

    /// Send shutdown commands to all interfaces.
    ///
    /// Errors are logged but do not stop the shutdown of remaining interfaces.
    pub async fn shutdown_all(&self) {
        for handle in &self.handles {
            let _ = handle.shutdown().await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::packet::Packet;
    use std::sync::Arc;

    /// Helper: create metadata with the given callsign and defaults.
    fn test_metadata(callsign: &str) -> InterfaceMetadata {
        InterfaceMetadata {
            callsign: callsign.to_string(),
            iface_type: InterfaceType::Null,
            tx_ok: false,
            igate_group: 1,
        }
    }

    /// Helper: create metadata with tx_ok set.
    fn test_metadata_tx(callsign: &str, tx_ok: bool) -> InterfaceMetadata {
        InterfaceMetadata {
            callsign: callsign.to_string(),
            iface_type: InterfaceType::Serial,
            tx_ok,
            igate_group: 1,
        }
    }

    /// Helper: create metadata with a specific igate group.
    fn test_metadata_group(callsign: &str, group: u8) -> InterfaceMetadata {
        InterfaceMetadata {
            callsign: callsign.to_string(),
            iface_type: InterfaceType::Null,
            tx_ok: false,
            igate_group: group,
        }
    }

    /// Helper: create a handle with a fresh channel.
    fn test_handle(
        metadata: InterfaceMetadata,
    ) -> (InterfaceHandle, mpsc::Receiver<InterfaceCommand>) {
        let (cmd_tx, cmd_rx) = mpsc::channel(16);
        (InterfaceHandle::new(metadata, cmd_tx), cmd_rx)
    }

    // --- InterfaceCommand tests ---

    #[test]
    fn interface_command_transmit_holds_packet() {
        let pkt = Arc::new(Packet::new("TEST>APRS:hello", "port0", true));
        let cmd = InterfaceCommand::Transmit(pkt.clone());
        match &cmd {
            InterfaceCommand::Transmit(p) => assert_eq!(p.tnc2, "TEST>APRS:hello"),
            _ => panic!("expected Transmit variant"),
        }
    }

    #[test]
    fn interface_command_shutdown_variant() {
        let cmd = InterfaceCommand::Shutdown;
        assert!(matches!(cmd, InterfaceCommand::Shutdown));
    }

    #[test]
    fn interface_command_debug_format() {
        let cmd = InterfaceCommand::Shutdown;
        let debug = format!("{:?}", cmd);
        assert!(debug.contains("Shutdown"));
    }

    // --- InterfaceMetadata tests ---

    #[test]
    fn metadata_stores_all_fields() {
        let meta = InterfaceMetadata {
            callsign: "OH2MQK-1".to_string(),
            iface_type: InterfaceType::Serial,
            tx_ok: true,
            igate_group: 2,
        };
        assert_eq!(meta.callsign, "OH2MQK-1");
        assert_eq!(meta.iface_type, InterfaceType::Serial);
        assert!(meta.tx_ok);
        assert_eq!(meta.igate_group, 2);
    }

    #[test]
    fn metadata_clone() {
        let meta = test_metadata("TEST-1");
        let cloned = meta.clone();
        assert_eq!(meta.callsign, cloned.callsign);
        assert_eq!(meta.igate_group, cloned.igate_group);
    }

    #[test]
    fn metadata_debug_format() {
        let meta = test_metadata("TEST-1");
        let debug = format!("{:?}", meta);
        assert!(debug.contains("TEST-1"));
    }

    // --- InterfaceHandle tests ---

    #[tokio::test]
    async fn handle_transmit_sends_command() {
        let (handle, mut cmd_rx) = test_handle(test_metadata("TEST-1"));
        let pkt = Arc::new(Packet::new("SRC>DST:data", "test", true));

        handle.transmit(pkt).await.unwrap();

        let cmd = cmd_rx.recv().await.unwrap();
        assert!(matches!(cmd, InterfaceCommand::Transmit(_)));
    }

    #[tokio::test]
    async fn handle_shutdown_sends_command() {
        let (handle, mut cmd_rx) = test_handle(test_metadata("TEST-1"));

        handle.shutdown().await.unwrap();

        let cmd = cmd_rx.recv().await.unwrap();
        assert!(matches!(cmd, InterfaceCommand::Shutdown));
    }

    #[tokio::test]
    async fn handle_transmit_fails_when_receiver_dropped() {
        let (handle, cmd_rx) = test_handle(test_metadata("TEST-1"));
        drop(cmd_rx);

        let pkt = Arc::new(Packet::new("SRC>DST:data", "test", true));
        let result = handle.transmit(pkt).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn handle_shutdown_fails_when_receiver_dropped() {
        let (handle, cmd_rx) = test_handle(test_metadata("TEST-1"));
        drop(cmd_rx);

        let result = handle.shutdown().await;
        assert!(result.is_err());
    }

    // --- InterfaceRegistry tests ---

    #[test]
    fn registry_new_is_empty() {
        let reg = InterfaceRegistry::new();
        assert!(reg.is_empty());
        assert_eq!(reg.len(), 0);
    }

    #[test]
    fn registry_default_is_empty() {
        let reg = InterfaceRegistry::default();
        assert!(reg.is_empty());
    }

    #[test]
    fn registry_register_increases_len() {
        let mut reg = InterfaceRegistry::new();
        let (handle, _rx) = test_handle(test_metadata("TEST-1"));
        reg.register(handle);
        assert_eq!(reg.len(), 1);
        assert!(!reg.is_empty());
    }

    #[test]
    fn registry_find_by_callsign_found() {
        let mut reg = InterfaceRegistry::new();
        let (handle, _rx) = test_handle(test_metadata("OH2MQK-1"));
        reg.register(handle);

        let found = reg.find_by_callsign("OH2MQK-1");
        assert!(found.is_some());
        assert_eq!(found.unwrap().metadata.callsign, "OH2MQK-1");
    }

    #[test]
    fn registry_find_by_callsign_not_found() {
        let mut reg = InterfaceRegistry::new();
        let (handle, _rx) = test_handle(test_metadata("OH2MQK-1"));
        reg.register(handle);

        assert!(reg.find_by_callsign("NONEXIST").is_none());
    }

    #[test]
    fn registry_find_by_callsign_returns_first_match() {
        let mut reg = InterfaceRegistry::new();

        let (h1, _rx1) = test_handle(test_metadata_tx("DUPE-1", true));
        let (h2, _rx2) = test_handle(test_metadata_tx("DUPE-1", false));
        reg.register(h1);
        reg.register(h2);

        let found = reg.find_by_callsign("DUPE-1").unwrap();
        // First registered should be returned
        assert!(found.metadata.tx_ok);
    }

    #[test]
    fn registry_tx_capable_filters_correctly() {
        let mut reg = InterfaceRegistry::new();

        let (h1, _rx1) = test_handle(test_metadata_tx("TX-1", true));
        let (h2, _rx2) = test_handle(test_metadata_tx("RX-1", false));
        let (h3, _rx3) = test_handle(test_metadata_tx("TX-2", true));
        reg.register(h1);
        reg.register(h2);
        reg.register(h3);

        let tx = reg.tx_capable();
        assert_eq!(tx.len(), 2);
        assert!(tx.iter().all(|h| h.metadata.tx_ok));
    }

    #[test]
    fn registry_tx_capable_empty_when_none() {
        let mut reg = InterfaceRegistry::new();
        let (h1, _rx1) = test_handle(test_metadata_tx("RX-1", false));
        reg.register(h1);

        assert!(reg.tx_capable().is_empty());
    }

    #[test]
    fn registry_by_igate_group() {
        let mut reg = InterfaceRegistry::new();

        let (h1, _rx1) = test_handle(test_metadata_group("A-1", 1));
        let (h2, _rx2) = test_handle(test_metadata_group("B-1", 2));
        let (h3, _rx3) = test_handle(test_metadata_group("C-1", 1));
        reg.register(h1);
        reg.register(h2);
        reg.register(h3);

        let group1 = reg.by_igate_group(1);
        assert_eq!(group1.len(), 2);

        let group2 = reg.by_igate_group(2);
        assert_eq!(group2.len(), 1);
        assert_eq!(group2[0].metadata.callsign, "B-1");

        let group3 = reg.by_igate_group(3);
        assert!(group3.is_empty());
    }

    #[test]
    fn registry_iter() {
        let mut reg = InterfaceRegistry::new();
        let (h1, _rx1) = test_handle(test_metadata("A-1"));
        let (h2, _rx2) = test_handle(test_metadata("B-1"));
        reg.register(h1);
        reg.register(h2);

        let callsigns: Vec<&str> = reg.iter().map(|h| h.metadata.callsign.as_str()).collect();
        assert_eq!(callsigns, vec!["A-1", "B-1"]);
    }

    #[tokio::test]
    async fn registry_shutdown_all_sends_to_all() {
        let mut reg = InterfaceRegistry::new();

        let (h1, mut rx1) = test_handle(test_metadata("A-1"));
        let (h2, mut rx2) = test_handle(test_metadata("B-1"));
        reg.register(h1);
        reg.register(h2);

        reg.shutdown_all().await;

        let cmd1 = rx1.recv().await.unwrap();
        assert!(matches!(cmd1, InterfaceCommand::Shutdown));

        let cmd2 = rx2.recv().await.unwrap();
        assert!(matches!(cmd2, InterfaceCommand::Shutdown));
    }

    #[tokio::test]
    async fn registry_shutdown_all_tolerates_closed_channels() {
        let mut reg = InterfaceRegistry::new();

        let (h1, rx1) = test_handle(test_metadata("A-1"));
        let (h2, mut rx2) = test_handle(test_metadata("B-1"));
        drop(rx1); // simulate already-dead interface
        reg.register(h1);
        reg.register(h2);

        // Should not panic even though rx1 is dropped
        reg.shutdown_all().await;

        // Second interface should still get the shutdown
        let cmd = rx2.recv().await.unwrap();
        assert!(matches!(cmd, InterfaceCommand::Shutdown));
    }

    // --- Interface trait object test ---

    /// A minimal null interface for testing the trait.
    struct NullInterface {
        metadata: InterfaceMetadata,
    }

    impl Interface for NullInterface {
        fn metadata(&self) -> &InterfaceMetadata {
            &self.metadata
        }

        fn run(
            self: Box<Self>,
            _packet_tx: mpsc::Sender<SharedPacket>,
            mut cmd_rx: mpsc::Receiver<InterfaceCommand>,
        ) -> Pin<Box<dyn Future<Output = ()> + Send>> {
            Box::pin(async move {
                // Just wait for shutdown
                while let Some(cmd) = cmd_rx.recv().await {
                    if matches!(cmd, InterfaceCommand::Shutdown) {
                        break;
                    }
                }
            })
        }
    }

    #[test]
    fn trait_object_metadata() {
        let iface: Box<dyn Interface> = Box::new(NullInterface {
            metadata: test_metadata("NULL-1"),
        });
        assert_eq!(iface.metadata().callsign, "NULL-1");
    }

    #[tokio::test]
    async fn trait_object_run_responds_to_shutdown() {
        let iface: Box<dyn Interface> = Box::new(NullInterface {
            metadata: test_metadata("NULL-1"),
        });

        let (packet_tx, _packet_rx) = mpsc::channel(16);
        let (cmd_tx, cmd_rx) = mpsc::channel(16);

        let run_future = iface.run(packet_tx, cmd_rx);

        // Send shutdown from another task
        cmd_tx.send(InterfaceCommand::Shutdown).await.unwrap();

        // run should complete
        run_future.await;
    }

    #[tokio::test]
    async fn trait_object_run_completes_when_cmd_channel_closes() {
        let iface: Box<dyn Interface> = Box::new(NullInterface {
            metadata: test_metadata("NULL-1"),
        });

        let (packet_tx, _packet_rx) = mpsc::channel(16);
        let (cmd_tx, cmd_rx) = mpsc::channel(16);

        let run_future = iface.run(packet_tx, cmd_rx);

        // Drop the sender to close the channel
        drop(cmd_tx);

        // run should complete when channel closes
        run_future.await;
    }
}
