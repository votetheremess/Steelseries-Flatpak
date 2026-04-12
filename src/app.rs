use std::cell::{Cell, RefCell};
use std::collections::HashSet;
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
use crate::icons;
use crate::tray;
use crate::window::ChatMixWindow;

const APP_ID: &str = "com.github.arctis_chatmix.ArctisNovaEliteChatMix";
const BATTERY_REFRESH_SECS: u32 = 30;
const STREAM_WATCH_SECS: u32 = 2;

struct AppResources {
    _sinks: VirtualSinks,
    _router: AudioRouter,
    shutdown: Arc<AtomicBool>,
    rx: Option<Receiver<HidEvent>>,
    writer: Option<HidWriter>,
}

pub fn run(start_hidden: bool) {
    let app = adw::Application::builder().application_id(APP_ID).build();

    let resources: Rc<RefCell<Option<AppResources>>> = Rc::new(RefCell::new(None));
    let setup_done: Rc<Cell<bool>> = Rc::new(Cell::new(false));
    // Only honor --hidden on the first activate
    let first_activate_hidden: Rc<Cell<bool>> = Rc::new(Cell::new(start_hidden));

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
        let setup_done = setup_done.clone();
        let first_activate_hidden = first_activate_hidden.clone();
        app.connect_activate(move |app| {
            // If setup already ran, this is a subsequent activation (user ran
            // `arctis-chatmix` while the first instance is still alive).
            // Just bring the existing window forward.
            if setup_done.get() {
                if let Some(window) = app.active_window() {
                    window.set_visible(true);
                    window.present();
                }
                return;
            }
            setup_done.set(true);

            // Register Lucide icon theme path (needs a gdk::Display, available here)
            icons::install();

            let window = ChatMixWindow::new(app);

            // Hide on close, keep the process running
            window.window.connect_close_request(move |w| {
                w.set_visible(false);
                glib::Propagation::Stop
            });

            // Present the window unless we were launched with --hidden
            if !first_activate_hidden.get() {
                window.window.present();
            }
            first_activate_hidden.set(false);

            // If startup failed, show disconnected state and stop here
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
                query_battery(&writer);
                let writer_periodic = writer.clone();
                glib::timeout_add_seconds_local(BATTERY_REFRESH_SECS, move || {
                    query_battery(&writer_periodic);
                    glib::ControlFlow::Continue
                });
            }

            // New-stream watcher: auto-route newly launched apps to saved ChatMix sinks.
            // Seed with currently-existing IDs so the first tick doesn't re-process them
            // (the startup restore_assignments() already handled those).
            let seen_ids: Rc<RefCell<HashSet<u32>>> =
                Rc::new(RefCell::new(persistence::initial_seen_ids()));
            glib::timeout_add_seconds_local(STREAM_WATCH_SECS, move || {
                persistence::restore_new_streams(&mut seen_ids.borrow_mut());
                glib::ControlFlow::Continue
            });

            // Spawn the system tray icon and poll for commands on the main thread
            let tray_rx = tray::spawn();
            let app_for_tray = app.clone();
            let window_for_tray = window.clone();
            glib::timeout_add_local(Duration::from_millis(100), move || {
                while let Ok(cmd) = tray_rx.try_recv() {
                    match cmd {
                        tray::TrayCommand::Show => {
                            window_for_tray.window.set_visible(true);
                            window_for_tray.window.present();
                        }
                        tray::TrayCommand::Quit => {
                            app_for_tray.quit();
                            return glib::ControlFlow::Break;
                        }
                    }
                }
                glib::ControlFlow::Continue
            });
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
