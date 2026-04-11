mod audio;
mod hid;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use audio::sinks::{self, VirtualSinks};
use audio::router::{self, AudioRouter};
use hid::protocol::{self, HidEvent};

fn main() {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();

    // Find the headset audio sink
    log::info!("Looking for Arctis Nova Elite audio sink...");
    let headset_sink = match router::find_headset_sink() {
        Ok(sink) => {
            log::info!("Found headset sink: {sink}");
            sink
        }
        Err(e) => {
            log::error!("{e}");
            std::process::exit(1);
        }
    };

    // Create virtual sinks
    log::info!("Creating virtual ChatMix sinks...");
    let sinks = match VirtualSinks::create() {
        Ok(s) => s,
        Err(e) => {
            log::error!("Failed to create virtual sinks: {e}");
            std::process::exit(1);
        }
    };

    // Route virtual sinks to headset
    log::info!("Setting up audio routing...");
    let router = match AudioRouter::create(&headset_sink) {
        Ok(r) => r,
        Err(e) => {
            log::error!("Failed to create audio routing: {e}");
            std::process::exit(1);
        }
    };

    // Open HID device
    log::info!("Searching for Arctis Nova Elite HID device...");
    let mut device = match hid::device::find_and_open() {
        Ok(dev) => dev,
        Err(e) => {
            log::error!("{e}");
            std::process::exit(1);
        }
    };

    // Enable ChatMix mode on the GameDAC
    log::info!("Enabling ChatMix mode...");
    match device.write_command(&protocol::chatmix_enable_command()) {
        Ok(()) => log::info!("ChatMix enabled on device"),
        Err(e) => log::warn!("Failed to enable ChatMix (may still work): {e}"),
    }

    // Set up Ctrl+C handler
    let shutdown = Arc::new(AtomicBool::new(false));
    let flag = shutdown.clone();
    if let Err(e) = ctrlc::set_handler(move || {
        flag.store(true, Ordering::Relaxed);
    }) {
        log::warn!("Could not set Ctrl+C handler: {e}");
    }

    // Start HID listener
    let (_handle, rx) = hid::listener::start(device, shutdown.clone());

    log::info!("ChatMix active! Assign apps to 'ChatMix Game' or 'ChatMix Chat' sinks.");
    log::info!("Listening for events (Ctrl+C to quit)...");

    while !shutdown.load(Ordering::Relaxed) {
        match rx.recv_timeout(std::time::Duration::from_millis(500)) {
            Ok(event) => handle_event(&event),
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => continue,
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                log::error!("HID listener disconnected");
                break;
            }
        }
    }

    log::info!("Shutting down...");
    // Drop order: router first, then sinks (loopbacks reference the sinks)
    drop(router);
    drop(sinks);
    log::info!("Cleanup complete");
}

fn handle_event(event: &HidEvent) {
    match event {
        HidEvent::ChatMixLevels { game, chat } => {
            log::info!("ChatMix: Game={game}%, Chat={chat}%");

            if let Err(e) = sinks::set_sink_volume(sinks::GAME_SINK_NAME, *game as u32) {
                log::error!("Failed to set game volume: {e}");
            }
            if let Err(e) = sinks::set_sink_volume(sinks::CHAT_SINK_NAME, *chat as u32) {
                log::error!("Failed to set chat volume: {e}");
            }
        }
        HidEvent::DialPosition(pos) => {
            log::info!("Dial position: {pos} (volume mode — switch to ChatMix for balance control)");
        }
        HidEvent::NoiseControl(mode) => {
            log::info!("Noise control: {mode}");
        }
        HidEvent::AncHardware(val) => {
            log::debug!("ANC hardware event: 0x{val:02x}");
        }
        HidEvent::Unknown { feature, value } => {
            log::debug!("Unknown: feature=0x{feature:02x} value=0x{value:02x}");
        }
    }
}
