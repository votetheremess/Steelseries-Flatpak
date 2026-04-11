use std::cell::RefCell;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, TryRecvError};
use std::sync::Arc;
use std::time::Duration;

use adw::prelude::*;
use gtk::glib;

use crate::audio::persistence;
use crate::audio::router::{self, AudioRouter};
use crate::audio::sinks::{self, VirtualSinks};
use crate::hid::device::HidWriter;
use crate::hid::{self, protocol::HidEvent};
use crate::window::ChatMixWindow;

const APP_ID: &str = "com.github.arctis_chatmix.ArctisNovaEliteChatMix";
const BATTERY_REFRESH_SECS: u32 = 30;

struct AppResources {
    _sinks: VirtualSinks,
    _router: AudioRouter,
    shutdown: Arc<AtomicBool>,
    rx: Option<Receiver<HidEvent>>,
    writer: Option<HidWriter>,
}

pub fn run() {
    let app = adw::Application::builder().application_id(APP_ID).build();

    let resources: Rc<RefCell<Option<AppResources>>> = Rc::new(RefCell::new(None));

    {
        let resources = resources.clone();
        app.connect_startup(move |_| match init_pipeline() {
            Ok(res) => *resources.borrow_mut() = Some(res),
            Err(e) => {
                log::error!("Failed to initialize pipeline: {e}");
            }
        });
    }

    {
        let resources = resources.clone();
        app.connect_activate(move |app| {
            let window = ChatMixWindow::new(app);

            // Ensure closing the window quits the application
            let app_weak = app.downgrade();
            window.window.connect_close_request(move |_| {
                if let Some(app) = app_weak.upgrade() {
                    app.quit();
                }
                glib::Propagation::Proceed
            });

            window.window.present();

            // If startup failed, show disconnected state
            if resources.borrow().is_none() {
                window.set_connected(false, None);
                return;
            }

            // Take the receiver and writer out of resources
            let (rx, writer) = {
                let mut res = resources.borrow_mut();
                let res = res.as_mut().unwrap();
                (res.rx.take(), res.writer.take())
            };

            let Some(rx) = rx else {
                log::warn!("No HID receiver available");
                return;
            };

            let window = Rc::new(window);
            let window_for_poll = window.clone();

            // HID event poll loop
            glib::timeout_add_local(Duration::from_millis(50), move || {
                loop {
                    match rx.try_recv() {
                        Ok(event) => handle_event(&event, &window_for_poll),
                        Err(TryRecvError::Empty) => break,
                        Err(TryRecvError::Disconnected) => {
                            log::error!("HID listener disconnected");
                            window_for_poll.set_connected(false, None);
                            return glib::ControlFlow::Break;
                        }
                    }
                }
                glib::ControlFlow::Continue
            });

            // Battery query: send once immediately, then refresh periodically
            if let Some(writer) = writer {
                let writer = Rc::new(RefCell::new(writer));

                // Initial query
                query_battery(&writer);

                // Periodic refresh
                let writer_periodic = writer.clone();
                glib::timeout_add_seconds_local(BATTERY_REFRESH_SECS, move || {
                    query_battery(&writer_periodic);
                    glib::ControlFlow::Continue
                });
            }
        });
    }

    {
        let resources = resources.clone();
        app.connect_shutdown(move |_| {
            log::info!("Shutting down...");
            // Save the current app assignments BEFORE destroying sinks,
            // otherwise PipeWire moves all streams to the fallback sink first.
            persistence::save_assignments();
            if let Some(res) = resources.borrow_mut().take() {
                res.shutdown.store(true, Ordering::Relaxed);
                // Drop order: router first, then sinks (Drop impls run automatically)
                drop(res);
            }
            log::info!("Cleanup complete");
        });
    }

    app.run();
}

fn init_pipeline() -> Result<AppResources, String> {
    log::info!("Looking for Arctis Nova Elite audio sink...");
    let headset_sink = router::find_headset_sink()?;
    log::info!("Found headset sink: {headset_sink}");

    log::info!("Creating virtual ChatMix sinks...");
    let sinks = VirtualSinks::create()?;

    log::info!("Setting up audio routing...");
    let router = AudioRouter::create(&headset_sink)?;

    // Give PipeWire a moment to register the new sinks before trying to move
    // existing sink-inputs to them.
    std::thread::sleep(std::time::Duration::from_millis(100));
    log::info!("Restoring saved app assignments...");
    persistence::restore_assignments();

    log::info!("Searching for Arctis Nova Elite HID device...");
    let mut device = hid::device::find_and_open()?;

    log::info!("Enabling ChatMix mode...");
    // Send disable first to force a clean state reset — otherwise if the GameDAC
    // was already in ChatMix mode, the enable is a no-op and the dial keeps sending
    // 0x25 (Volume mode) events instead of 0x45 (ChatMix mode) events.
    let _ = device.write_command(&hid::protocol::chatmix_disable_command());
    std::thread::sleep(std::time::Duration::from_millis(50));
    match device.write_command(&hid::protocol::chatmix_enable_command()) {
        Ok(()) => log::info!("ChatMix enabled on device"),
        Err(e) => log::warn!("Failed to enable ChatMix (may still work): {e}"),
    }

    // Clone a separate file handle for queries from the GTK main thread.
    // The listener thread keeps the original handle for reads.
    let writer = device
        .try_clone_writer()
        .map_err(|e| format!("Failed to clone HID writer: {e}"))?;

    let shutdown = Arc::new(AtomicBool::new(false));
    let (_handle, rx) = hid::listener::start(device, shutdown.clone());

    log::info!("ChatMix active! Assign apps to 'ChatMix Game' or 'ChatMix Chat' sinks.");

    Ok(AppResources {
        _sinks: sinks,
        _router: router,
        shutdown,
        rx: Some(rx),
        writer: Some(writer),
    })
}

fn query_battery(writer: &Rc<RefCell<HidWriter>>) {
    if let Err(e) = writer
        .borrow_mut()
        .write_command(&hid::protocol::battery_query_command())
    {
        log::warn!("Failed to query battery: {e}");
    }
}

fn handle_event(event: &HidEvent, window: &ChatMixWindow) {
    match event {
        HidEvent::ChatMixLevels { game, chat } => {
            log::info!("ChatMix: Game={game}%, Chat={chat}%");

            if let Err(e) = sinks::set_sink_volume(sinks::GAME_SINK_NAME, *game as u32) {
                log::error!("Failed to set game volume: {e}");
            }
            if let Err(e) = sinks::set_sink_volume(sinks::CHAT_SINK_NAME, *chat as u32) {
                log::error!("Failed to set chat volume: {e}");
            }

            window.set_chatmix(*game, *chat);
        }
        HidEvent::DialPosition(pos) => {
            log::debug!("Dial position: {pos} (volume mode — ignored)");
        }
        HidEvent::NoiseControl(mode) => {
            log::info!("Noise control: {mode}");
            window.set_noise_mode(*mode);
        }
        HidEvent::AncHardware(val) => {
            log::debug!("ANC hardware event: 0x{val:02x}");
        }
        HidEvent::BatteryStatus { headset, spare, flags } => {
            log::info!("Battery: headset={headset}%, spare={spare}%, flags=0x{flags:02x}");
            window.set_battery(*headset, *spare);
        }
        HidEvent::Unknown { feature, value } => {
            log::debug!("Unknown: feature=0x{feature:02x} value=0x{value:02x}");
        }
    }
}
