use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::Arc;

use super::device::HidDevice;
use super::protocol::{self, HidEvent};

pub fn start(
    mut device: HidDevice,
    shutdown: Arc<AtomicBool>,
) -> (std::thread::JoinHandle<()>, mpsc::Receiver<HidEvent>) {
    let (tx, rx) = mpsc::channel();

    let handle = std::thread::spawn(move || {
        let mut buf = [0u8; 64];
        log::info!("HID listener started on {}", device.path.display());

        loop {
            if shutdown.load(Ordering::Relaxed) {
                log::info!("HID listener shutting down");
                break;
            }

            match device.read_timeout(&mut buf, 1000) {
                Ok(0) => continue, // timeout, no data
                Ok(_) => {
                    if let Some(event) = protocol::parse(&buf) {
                        if tx.send(event).is_err() {
                            log::info!("Receiver dropped, stopping listener");
                            break;
                        }
                    }
                    buf = [0u8; 64];
                }
                Err(e) => {
                    log::error!("HID read error: {e}");
                    break;
                }
            }
        }
    });

    (handle, rx)
}
