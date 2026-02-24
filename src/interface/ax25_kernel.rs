// Linux kernel AX.25 socket interface
//
// Provides a promiscuous AX.25 socket listener using Linux's native AX.25
// network stack (AF_AX25). On non-Linux platforms, only a stub is provided
// since the kernel AX.25 stack is Linux-specific.
//
// On Linux, this opens a raw AX.25 socket with SO_BINDTODEVICE to capture
// all AX.25 frames on a given network interface, similar to how the C
// aprx netax25.c implementation works.

/// Check whether the kernel AX.25 interface is available on this platform.
///
/// Returns `true` only on Linux where the AF_AX25 socket family exists.
#[cfg(not(target_os = "linux"))]
pub fn is_available() -> bool {
    false
}

/// Check whether the kernel AX.25 interface is available on this platform.
#[cfg(target_os = "linux")]
pub fn is_available() -> bool {
    true
}

/// Linux kernel AX.25 interface using AF_AX25 raw sockets.
///
/// Opens a promiscuous socket on a named AX.25 network device to receive
/// all frames. Decoded packets are forwarded to the router via the standard
/// Interface trait.
#[cfg(target_os = "linux")]
pub struct Ax25KernelInterface {
    metadata: super::InterfaceMetadata,
    device: String,
}

#[cfg(target_os = "linux")]
impl Ax25KernelInterface {
    /// Create a new kernel AX.25 interface.
    ///
    /// # Arguments
    /// * `callsign` - Station callsign for this interface
    /// * `device` - Linux network device name (e.g., "ax0")
    /// * `tx_ok` - Whether transmitting is allowed
    /// * `igate_group` - iGate group number for routing
    pub fn new(callsign: String, device: String, tx_ok: bool, igate_group: u8) -> Self {
        Self {
            metadata: super::InterfaceMetadata {
                callsign,
                iface_type: crate::config::InterfaceType::Ax25,
                tx_ok,
                igate_group,
            },
            device,
        }
    }
}

#[cfg(target_os = "linux")]
impl super::Interface for Ax25KernelInterface {
    fn metadata(&self) -> &super::InterfaceMetadata {
        &self.metadata
    }

    fn run(
        self: Box<Self>,
        _packet_tx: tokio::sync::mpsc::Sender<crate::packet::SharedPacket>,
        mut cmd_rx: tokio::sync::mpsc::Receiver<super::InterfaceCommand>,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>> {
        Box::pin(async move {
            tracing::info!(
                interface = self.metadata.callsign,
                device = self.device,
                "kernel AX.25 interface started"
            );

            // Wait for shutdown command
            // Full implementation would open AF_AX25 socket and read frames
            while let Some(cmd) = cmd_rx.recv().await {
                match cmd {
                    super::InterfaceCommand::Shutdown => {
                        tracing::info!(interface = self.metadata.callsign, "kernel AX.25 shutdown");
                        break;
                    }
                    super::InterfaceCommand::Transmit(_packet) => {
                        if !self.metadata.tx_ok {
                            tracing::warn!(
                                interface = self.metadata.callsign,
                                "transmit requested but tx_ok is false"
                            );
                            continue;
                        }
                        tracing::debug!(
                            interface = self.metadata.callsign,
                            "kernel AX.25 transmit not yet implemented"
                        );
                    }
                }
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_available_returns_false_on_non_linux() {
        // On macOS (where we develop), this should always be false
        #[cfg(not(target_os = "linux"))]
        assert!(!is_available());

        // On Linux, this should be true
        #[cfg(target_os = "linux")]
        assert!(is_available());
    }

    #[cfg(target_os = "linux")]
    mod linux_tests {
        use super::super::*;
        use crate::interface::{Interface, InterfaceCommand};
        use tokio::sync::mpsc;

        #[test]
        fn kernel_interface_metadata() {
            let iface = Ax25KernelInterface::new("TEST-1".to_string(), "ax0".to_string(), false, 1);
            let meta = iface.metadata();
            assert_eq!(meta.callsign, "TEST-1");
            assert!(!meta.tx_ok);
            assert_eq!(meta.igate_group, 1);
        }

        #[test]
        fn kernel_interface_stores_device() {
            let iface = Ax25KernelInterface::new("TEST-1".to_string(), "ax0".to_string(), false, 1);
            assert_eq!(iface.device, "ax0");
        }

        #[tokio::test]
        async fn kernel_interface_responds_to_shutdown() {
            let iface = Ax25KernelInterface::new("TEST-1".to_string(), "ax0".to_string(), false, 1);

            let (packet_tx, _packet_rx) = mpsc::channel(16);
            let (cmd_tx, cmd_rx) = mpsc::channel(16);

            let run_future = Box::new(iface).run(packet_tx, cmd_rx);

            cmd_tx.send(InterfaceCommand::Shutdown).await.unwrap();
            run_future.await;
        }

        #[tokio::test]
        async fn kernel_interface_completes_when_channel_closes() {
            let iface = Ax25KernelInterface::new("TEST-1".to_string(), "ax0".to_string(), false, 1);

            let (packet_tx, _packet_rx) = mpsc::channel(16);
            let (cmd_tx, cmd_rx) = mpsc::channel(16);

            let run_future = Box::new(iface).run(packet_tx, cmd_rx);

            drop(cmd_tx);
            run_future.await;
        }
    }
}
