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
use crate::eq::model::{Band, EqTarget, SpatialState, NUM_BANDS};
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
    router: Rc<RefCell<Option<AudioRouter>>>,
    shutdown: Arc<AtomicBool>,
    rx: Option<Receiver<HidEvent>>,
    writer: Option<HidWriter>,
    headset_sink: String,
    clip_backend: Option<crate::clips::backend::BackendHandle>,
    clip_events: Option<Receiver<crate::clips::BackendEvent>>,
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

            // Build the debounced EQ apply callback.
            // When the EQ UI changes, this schedules a 16ms debounced update
            // (one frame) that sends the new band settings to the EQ pipeline
            // thread. Short debounce for live responsiveness — the EQ thread
            // uses in-place set-param for Freq/Q/Gain changes (no audio gap).
            let on_eq_apply: Option<Rc<dyn Fn(EqTarget, [Band; NUM_BANDS])>> = {
                let router_ref = resources
                    .borrow()
                    .as_ref()
                    .map(|r| r.router.clone());

                router_ref.map(|router| {
                    let debounce: Rc<RefCell<Option<glib::SourceId>>> =
                        Rc::new(RefCell::new(None));
                    Rc::new(move |target: EqTarget, bands: [Band; NUM_BANDS]| {
                        // Cancel any pending debounce
                        if let Some(id) = debounce.borrow_mut().take() {
                            id.remove();
                        }
                        let router = router.clone();
                        let debounce_ref = debounce.clone();
                        let id = glib::timeout_add_local_once(
                            Duration::from_millis(16),
                            move || {
                                if let Some(ref r) = *router.borrow() {
                                    r.update_eq(target.sink_name(), &bands);
                                }
                                *debounce_ref.borrow_mut() = None;
                            },
                        );
                        *debounce.borrow_mut() = Some(id);
                    }) as Rc<dyn Fn(EqTarget, [Band; NUM_BANDS])>
                })
            };

            // Build the debounced spatial apply callback. Same debounce
            // cadence as EQ so rapid slider drags / stage drags coalesce into
            // one update per frame.
            let on_spatial_apply: Option<Rc<dyn Fn(EqTarget, SpatialState)>> = {
                let router_ref = resources
                    .borrow()
                    .as_ref()
                    .map(|r| r.router.clone());
                router_ref.map(|router| {
                    let debounce: Rc<RefCell<Option<glib::SourceId>>> =
                        Rc::new(RefCell::new(None));
                    Rc::new(move |target: EqTarget, state: SpatialState| {
                        if let Some(id) = debounce.borrow_mut().take() {
                            id.remove();
                        }
                        let router = router.clone();
                        let debounce_ref = debounce.clone();
                        let id = glib::timeout_add_local_once(
                            Duration::from_millis(16),
                            move || {
                                if let Some(ref r) = *router.borrow() {
                                    r.set_spatial(target.sink_name(), state);
                                }
                                *debounce_ref.borrow_mut() = None;
                            },
                        );
                        *debounce.borrow_mut() = Some(id);
                    }) as Rc<dyn Fn(EqTarget, SpatialState)>
                })
            };

            // Mixer reroute callbacks
            let on_reroute: Option<Rc<dyn Fn(&str, &str)>> = {
                let router_ref = resources
                    .borrow()
                    .as_ref()
                    .map(|r| r.router.clone());
                router_ref.map(|router| {
                    Rc::new(move |sink_name: &str, new_target: &str| {
                        if let Some(ref r) = *router.borrow() {
                            r.reroute_sink(sink_name, new_target);
                        }
                    }) as Rc<dyn Fn(&str, &str)>
                })
            };

            let on_mic_reroute: Option<Rc<dyn Fn(&str)>> = {
                let router_ref = resources
                    .borrow()
                    .as_ref()
                    .map(|r| r.router.clone());
                router_ref.map(|router| {
                    Rc::new(move |new_source: &str| {
                        if let Some(ref mut r) = *router.borrow_mut() {
                            r.reroute_mic(new_source);
                        }
                    }) as Rc<dyn Fn(&str)>
                })
            };

            let headset_sink = resources
                .borrow()
                .as_ref()
                .map(|r| r.headset_sink.clone());

            let window = ChatMixWindow::new(
                app,
                on_eq_apply,
                on_spatial_apply,
                on_reroute,
                on_mic_reroute,
                headset_sink,
            );

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

            // Build the BufferController seeded with the user's headset sink
            // monitor and default Videos/Clips output dir. The portal restore
            // token is wired in later (Phase 3) — until then the controller
            // stays in Uninitialized and never sends StartReplay.
            let buffer = {
                let mut initial_cfg = crate::clips::CaptureConfig::default();
                if let Some(res) = resources.borrow().as_ref() {
                    initial_cfg.headset_sink_monitor = format!("{}.monitor", res.headset_sink);
                }
                initial_cfg.output_dir = std::path::PathBuf::from(
                    std::env::var("HOME").unwrap_or_default(),
                )
                .join("Videos/Clips");
                Rc::new(RefCell::new(crate::clips::BufferController::new(initial_cfg)))
            };

            // -------------------------------------------------------------
            // Wizard actions (Phase 3 Task 3.5)
            // -------------------------------------------------------------
            //
            // Each action is registered against the GApplication so the
            // wizard buttons (which reference `app.<name>` action names) can
            // dispatch into Rust closures. Widget references and shared state
            // (clips_page, buffer, resources) are captured via Rc clones so
            // closures can outlive `connect_activate`.

            // app.gsr-install — kick off async install, stream progress to
            // the Page 1 status label. Polls the install-progress receiver
            // on a glib timer (every 100 ms) and flips the Next button to
            // enabled once installation completes successfully.
            //
            // Guarded with `install_in_progress` so a double-click (or a
            // user mashing the Install button while flatpak is still
            // running) is a no-op rather than spawning a second flatpak
            // install thread + second progress timer (which would race on
            // the shared status label / Next button).
            {
                let clips_page = window.clips_page().clone();
                let install_in_progress = Rc::new(Cell::new(false));
                let install_in_progress_for_action = install_in_progress.clone();
                let install_action = gtk::gio::ActionEntry::builder("gsr-install")
                    .activate(move |_app: &adw::Application, _action, _param| {
                        if install_in_progress_for_action.get() {
                            log::info!(
                                "gsr-install: already in progress, ignoring duplicate click"
                            );
                            return;
                        }
                        install_in_progress_for_action.set(true);
                        let in_progress = install_in_progress_for_action.clone();
                        let rx = crate::clips::gsr_install::install();
                        clips_page.wizard.install_status_label.set_visible(true);
                        clips_page
                            .wizard
                            .install_status_label
                            .set_label("Starting install…");
                        let label = clips_page.wizard.install_status_label.clone();
                        let next_btn = clips_page.wizard.install_next_btn.clone();
                        glib::timeout_add_local(Duration::from_millis(100), move || {
                            while let Ok(evt) = rx.try_recv() {
                                use crate::clips::gsr_install::InstallProgress;
                                match evt {
                                    InstallProgress::Started => {
                                        label.set_label("Starting install…");
                                    }
                                    InstallProgress::Status(s) => label.set_label(&s),
                                    InstallProgress::Done => {
                                        label.set_label("Installed.");
                                        next_btn.set_sensitive(true);
                                        in_progress.set(false);
                                        return glib::ControlFlow::Break;
                                    }
                                    InstallProgress::Failed { reason } => {
                                        label.set_label(&format!("Install failed: {reason}"));
                                        // Next stays disabled — user can retry.
                                        in_progress.set(false);
                                        return glib::ControlFlow::Break;
                                    }
                                }
                            }
                            glib::ControlFlow::Continue
                        });
                    })
                    .build();
                app.add_action_entries([install_action]);
            }

            // app.gsr-open-in-app-store — opens appstream:// URL via xdg-open.
            // The DE's appstream:// handler decides which app store opens
            // (Discover on KDE, Software on GNOME, Bazaar on Bazzite, etc.).
            {
                let app_store_action = gtk::gio::ActionEntry::builder("gsr-open-in-app-store")
                    .activate(|_app: &adw::Application, _action, _param| {
                        if let Err(e) = crate::clips::gsr_install::open_in_app_store() {
                            log::warn!("open_in_app_store failed: {e}");
                        }
                    })
                    .build();
                app.add_action_entries([app_store_action]);
            }

            // app.gsr-copy-cli — copy the terminal install command to the
            // system clipboard so the user can paste it into a shell.
            {
                let win_for_copy = window.clone();
                let copy_action = gtk::gio::ActionEntry::builder("gsr-copy-cli")
                    .activate(move |_app: &adw::Application, _action, _param| {
                        let display = gtk::prelude::WidgetExt::display(&win_for_copy.window);
                        let clipboard = display.clipboard();
                        clipboard.set_text(crate::clips::gsr_install::GSR_TERMINAL_INSTALL_COMMAND);
                    })
                    .build();
                app.add_action_entries([copy_action]);
            }

            // app.wizard-next — advance the wizard's internal step or, on
            // Page 3 Done, transition the page state to Empty (browser).
            // Pressing Next on Page 2 also flips onboarding_complete = true
            // so subsequent app launches skip directly to the browser.
            {
                let buffer_for_next = buffer.clone();
                let clips_page = window.clips_page();
                let next_action = gtk::gio::ActionEntry::builder("wizard-next")
                    .activate(move |_app: &adw::Application, _action, _param| {
                        use crate::clips::WizardStep;
                        match clips_page.current_wizard_step() {
                            WizardStep::InstallGsr => {
                                clips_page.set_wizard_step(WizardStep::PickScreen);
                            }
                            WizardStep::PickScreen => {
                                if let Err(e) = crate::clips::settings::mark_onboarding_complete()
                                {
                                    log::warn!(
                                        "failed to persist onboarding_complete: {e}"
                                    );
                                }
                                clips_page.set_wizard_step(WizardStep::Settings);
                            }
                            WizardStep::Settings => {
                                clips_page.set_state(crate::clips::PageState::Empty);
                            }
                        }
                        // Buffer is held captured for symmetry with future
                        // expansions (e.g., flushing settings to it on Done).
                        let _ = &buffer_for_next;
                    })
                    .build();
                app.add_action_entries([next_action]);
            }

            // app.setup-clips — open the ScreenCast portal picker, save the
            // restore token, notify the BufferController, and update Page 2
            // of the wizard.
            //
            // The async portal call is driven on the GLib main context via
            // spawn_future_local so we don't block the GTK event loop while
            // the user is interacting with the portal dialog.
            {
                let clips_page = window.clips_page();
                let buffer_for_setup = buffer.clone();
                let resources_for_setup = resources.clone();
                let setup_action = gtk::gio::ActionEntry::builder("setup-clips")
                    .activate(move |_app: &adw::Application, _action, _param| {
                        let clips_page = clips_page.clone();
                        let buffer_for_setup = buffer_for_setup.clone();
                        let resources_for_setup = resources_for_setup.clone();
                        glib::spawn_future_local(async move {
                            match crate::clips::portal::pick_screencast_source().await {
                                Ok(token) => {
                                    if token.is_empty() {
                                        // Portal succeeded but returned no
                                        // persistent token. Surface as a
                                        // try-again rather than saving an
                                        // empty token (which would later
                                        // round-trip to "no token" and
                                        // re-trigger the picker anyway).
                                        log::warn!(
                                            "portal returned empty restore token; \
                                             treating as try-again"
                                        );
                                        clips_page.wizard.screen_picked_label.set_visible(true);
                                        clips_page.wizard.screen_picked_label.set_label(
                                            "Picker returned no persistent token \
                                             — try again.",
                                        );
                                        clips_page.wizard.screen_next_btn.set_sensitive(false);
                                        return;
                                    }
                                    if let Err(e) = crate::clips::portal::save_token(&token) {
                                        log::warn!("save_token failed: {e}");
                                        return;
                                    }
                                    // Notify BufferController. Looked up at
                                    // call time so a recreated backend
                                    // handle (future) is picked up.
                                    let cmd_tx = resources_for_setup
                                        .borrow()
                                        .as_ref()
                                        .and_then(|r| {
                                            r.clip_backend.as_ref().map(|h| h.sender())
                                        });
                                    if let Some(tx) = cmd_tx {
                                        buffer_for_setup
                                            .borrow_mut()
                                            .on_portal_pick_complete(token, &tx);
                                    } else {
                                        log::warn!(
                                            "no clip backend available to receive portal pick"
                                        );
                                    }
                                    // Update wizard Page 2.
                                    clips_page.wizard.screen_picked_label.set_visible(true);
                                    clips_page
                                        .wizard
                                        .screen_picked_label
                                        .set_label("Screen picked.");
                                    clips_page.wizard.screen_next_btn.set_sensitive(true);
                                }
                                Err(e) => {
                                    log::warn!("portal pick failed: {e}");
                                }
                            }
                        });
                    })
                    .build();
                app.add_action_entries([setup_action]);
            }

            // app.save-clip[-short|-medium|-long] — driven by the
            // GlobalShortcuts portal in Phase 4 Task 4.2 (and exposed as
            // GApplication actions so the portal listener — and any future
            // in-app button — can trigger them via activate_action()).
            //
            // Each handler asks the BufferController to send the
            // corresponding ClipCommand. The bare `save-clip` uses the
            // existing on_save_hotkey path (full buffer length); the three
            // duration variants share on_save_hotkey_duration so the backend
            // can pick SIGRTMIN+1/2/3 for GSR. All four are state-machine
            // safe — they no-op outside the Armed state.
            {
                let buffer_for_save = buffer.clone();
                let resources_for_save = resources.clone();
                let save_action = gtk::gio::ActionEntry::builder("save-clip")
                    .activate(move |_app: &adw::Application, _action, _param| {
                        let cmd_tx = resources_for_save
                            .borrow()
                            .as_ref()
                            .and_then(|r| r.clip_backend.as_ref().map(|h| h.sender()));
                        if let Some(tx) = cmd_tx {
                            buffer_for_save.borrow_mut().on_save_hotkey(&tx);
                        } else {
                            log::warn!("no clip backend available for save-clip");
                        }
                    })
                    .build();
                app.add_action_entries([save_action]);
            }
            {
                let buffer_for_save = buffer.clone();
                let resources_for_save = resources.clone();
                let save_action = gtk::gio::ActionEntry::builder("save-clip-short")
                    .activate(move |_app: &adw::Application, _action, _param| {
                        let cmd_tx = resources_for_save
                            .borrow()
                            .as_ref()
                            .and_then(|r| r.clip_backend.as_ref().map(|h| h.sender()));
                        if let Some(tx) = cmd_tx {
                            buffer_for_save
                                .borrow_mut()
                                .on_save_hotkey_duration(crate::clips::ClipCommand::SaveClipShort, &tx);
                        } else {
                            log::warn!("no clip backend available for save-clip-short");
                        }
                    })
                    .build();
                app.add_action_entries([save_action]);
            }
            {
                let buffer_for_save = buffer.clone();
                let resources_for_save = resources.clone();
                let save_action = gtk::gio::ActionEntry::builder("save-clip-medium")
                    .activate(move |_app: &adw::Application, _action, _param| {
                        let cmd_tx = resources_for_save
                            .borrow()
                            .as_ref()
                            .and_then(|r| r.clip_backend.as_ref().map(|h| h.sender()));
                        if let Some(tx) = cmd_tx {
                            buffer_for_save
                                .borrow_mut()
                                .on_save_hotkey_duration(crate::clips::ClipCommand::SaveClipMedium, &tx);
                        } else {
                            log::warn!("no clip backend available for save-clip-medium");
                        }
                    })
                    .build();
                app.add_action_entries([save_action]);
            }
            {
                let buffer_for_save = buffer.clone();
                let resources_for_save = resources.clone();
                let save_action = gtk::gio::ActionEntry::builder("save-clip-long")
                    .activate(move |_app: &adw::Application, _action, _param| {
                        let cmd_tx = resources_for_save
                            .borrow()
                            .as_ref()
                            .and_then(|r| r.clip_backend.as_ref().map(|h| h.sender()));
                        if let Some(tx) = cmd_tx {
                            buffer_for_save
                                .borrow_mut()
                                .on_save_hotkey_duration(crate::clips::ClipCommand::SaveClipLong, &tx);
                        } else {
                            log::warn!("no clip backend available for save-clip-long");
                        }
                    })
                    .build();
                app.add_action_entries([save_action]);
            }

            // app.reset-clips-capture — clears the persisted portal token,
            // disarms the buffer if currently capturing, and returns the
            // Clips page to the onboarding wizard (PickScreen step, since
            // GSR is still installed). Triggered from Settings → Clips →
            // Reset.
            {
                let clips_page = window.clips_page();
                let buffer_for_reset = buffer.clone();
                let resources_for_reset = resources.clone();
                let reset_action = gtk::gio::ActionEntry::builder("reset-clips-capture")
                    .activate(move |_app: &adw::Application, _action, _param| {
                        if let Err(e) = crate::clips::portal::clear_token() {
                            log::warn!("clear_token failed: {e}");
                        }
                        let cmd_tx = resources_for_reset
                            .borrow()
                            .as_ref()
                            .and_then(|r| r.clip_backend.as_ref().map(|h| h.sender()));
                        if let Some(tx) = cmd_tx {
                            buffer_for_reset.borrow_mut().on_portal_reset(&tx);
                        } else {
                            log::warn!("no clip backend available to reset capture");
                        }
                        // GSR is still installed (the user just wants a new
                        // screen pick), so jump straight to PickScreen rather
                        // than the install step.
                        clips_page.set_wizard_step(crate::clips::WizardStep::PickScreen);
                        clips_page.set_state(crate::clips::PageState::Onboarding);
                    })
                    .build();
                app.add_action_entries([reset_action]);
            }

            // app.test-clip-capture — proactive xdg-portal #1371 mitigation.
            // Triggers the Screenshot portal (which shares consent with the
            // ScreenCast portal we already used) and shows the captured
            // image in a dialog so the user can verify their persisted
            // capture target without saving an actual clip.
            {
                let app_for_test = app.clone();
                let test_action = gtk::gio::ActionEntry::builder("test-clip-capture")
                    .activate(move |_app: &adw::Application, _action, _param| {
                        let app_for_async = app_for_test.clone();
                        glib::MainContext::default().spawn_local(async move {
                            match crate::clips::portal::screenshot_current_target().await {
                                Ok(path) => show_test_capture_dialog(&app_for_async, &path),
                                Err(e) => log::warn!("test capture failed: {e}"),
                            }
                        });
                    })
                    .build();
                app.add_action_entries([test_action]);
            }

            // app.pick-clip-storage — folder picker for the clips storage
            // path. Stub for now; full implementation comes in a later
            // phase. Registered so the Page 3 button doesn't error out.
            {
                let pick_storage_action =
                    gtk::gio::ActionEntry::builder("pick-clip-storage")
                        .activate(|_app: &adw::Application, _action, _param| {
                            log::info!(
                                "app.pick-clip-storage invoked (stub — not yet implemented)"
                            );
                        })
                        .build();
                app.add_action_entries([pick_storage_action]);
            }

            // app.rebind-clip-hotkey — hotkey rebinder for the clips save
            // shortcut. Stub for now; full implementation lands in Phase 4
            // alongside the global-shortcuts portal wiring. Registered so
            // the Page 3 "Change…" button doesn't fail with an unknown
            // action.
            {
                let rebind_action = gtk::gio::ActionEntry::builder("rebind-clip-hotkey")
                    .activate(|_app: &adw::Application, _action, _param| {
                        log::info!(
                            "app.rebind-clip-hotkey invoked (stub — Phase 4)"
                        );
                    })
                    .build();
                app.add_action_entries([rebind_action]);
            }

            // GSR-install detection poll. If the user installs GSR via the
            // system app store / a terminal while sitting on Page 1, this
            // timer notices and enables the Next button. The in-app install
            // path enables Next via its own progress watcher; whichever
            // finishes first wins.
            //
            // Stops polling once we're past Page 1 (no GSR-install detection
            // is possible from any other wizard step, and the alternative —
            // running a flatpak query forever — both leaks memory and
            // creates a fork-storm). Bidirectional too: if GSR is detected
            // as uninstalled mid-poll, Next is disabled again.
            //
            // Note: if the user later does Reset capture source and returns
            // to the wizard, the wizard moves to PickScreen (not InstallGsr),
            // so this timer doesn't get re-spawned. That's fine — GSR is
            // presumably still installed from the original onboarding, and
            // arm()'s missing-GSR check still catches the rare case where
            // it isn't.
            {
                let clips_page = window.clips_page();
                glib::timeout_add_seconds_local(2, move || {
                    if !matches!(
                        clips_page.current_wizard_step(),
                        crate::clips::WizardStep::InstallGsr
                    ) {
                        return glib::ControlFlow::Break;
                    }
                    let installed = crate::clips::gsr_install::is_installed();
                    clips_page.wizard.install_next_btn.set_sensitive(installed);
                    if installed {
                        clips_page.wizard.install_status_label.set_visible(true);
                        clips_page
                            .wizard
                            .install_status_label
                            .set_label("Installed.");
                    }
                    glib::ControlFlow::Continue
                });
            }

            // Auto-resume: jump to the first incomplete wizard step (or
            // skip the wizard entirely) based on persisted state.
            //
            //   onboarding_complete + GSR + token → skip wizard, go to
            //                                       browser (Empty state).
            //   GSR missing                       → Page 1 (install).
            //   GSR present but no token          → Page 2 (pick screen),
            //                                       enable Page 1 Next.
            //   GSR + token + flag NOT set        → user reached Page 2
            //                                       last session but never
            //                                       pressed Next. Connect
            //                                       buffer with the
            //                                       persisted token (so
            //                                       auto-arm works
            //                                       immediately) AND show
            //                                       Page 2 with a hint so
            //                                       they confirm the pick.
            //                                       Don't silently flip
            //                                       onboarding_complete —
            //                                       wait for user's Next.
            {
                let clip_settings = crate::clips::settings::load();
                let token = crate::clips::portal::load_token();
                let gsr_ok = crate::clips::gsr_install::is_installed();

                if clip_settings.onboarding_complete && gsr_ok && token.is_some() {
                    if let Some(t) = token {
                        let cmd_tx = resources
                            .borrow()
                            .as_ref()
                            .and_then(|r| r.clip_backend.as_ref().map(|h| h.sender()));
                        if let Some(tx) = cmd_tx {
                            buffer.borrow_mut().on_portal_pick_complete(t, &tx);
                        }
                    }
                    window.clips_page().set_state(crate::clips::PageState::Empty);
                } else if !gsr_ok {
                    window
                        .clips_page()
                        .set_wizard_step(crate::clips::WizardStep::InstallGsr);
                } else if token.is_none() {
                    window
                        .clips_page()
                        .set_wizard_step(crate::clips::WizardStep::PickScreen);
                    // Already past page 1 — keep its Next enabled too in case
                    // the user navigates back.
                    window
                        .clips_page()
                        .wizard
                        .install_next_btn
                        .set_sensitive(true);
                } else {
                    // GSR installed + token present + onboarding flag false.
                    // The user reached Page 2 last session but never pressed
                    // Next. Connect the buffer with the persisted token (so
                    // auto-arm works immediately) AND show the wizard at
                    // PickScreen for explicit user confirmation. Don't
                    // silently flip onboarding_complete — wait for the
                    // user's Next click.
                    if let Some(t) = token {
                        let cmd_tx = resources
                            .borrow()
                            .as_ref()
                            .and_then(|r| r.clip_backend.as_ref().map(|h| h.sender()));
                        if let Some(tx) = cmd_tx {
                            buffer.borrow_mut().on_portal_pick_complete(t, &tx);
                        }
                    }
                    let cp = window.clips_page();
                    cp.set_wizard_step(crate::clips::WizardStep::PickScreen);
                    cp.wizard.screen_picked_label.set_visible(true);
                    cp.wizard.screen_picked_label.set_label(
                        "Previously picked — click Next to keep, or Pick screen to change.",
                    );
                    cp.wizard.screen_next_btn.set_sensitive(true);
                    // Already past page 1 — keep its Next enabled too in
                    // case the user navigates back.
                    cp.wizard.install_next_btn.set_sensitive(true);
                    // Onboarding state remains incomplete until user
                    // confirms via Next.
                }
            }

            // Clip backend event poll. Drains BackendEvents into the
            // BufferController so its state stays in sync with the backend
            // thread (Armed / Disarmed / Saved / errors).
            let resources_for_clips = resources.clone();
            let buf_for_events = buffer.clone();
            glib::timeout_add_local(Duration::from_millis(100), move || {
                if let Some(res) = resources_for_clips.borrow().as_ref() {
                    if let (Some(rx), Some(tx)) = (
                        res.clip_events.as_ref(),
                        res.clip_backend.as_ref().map(|h| h.sender()),
                    ) {
                        let mut buf = buf_for_events.borrow_mut();
                        loop {
                            match rx.try_recv() {
                                Ok(evt) => {
                                    log::info!("clip event: {evt:?}");
                                    buf.on_backend_event(&evt, &tx);
                                }
                                Err(TryRecvError::Empty) => break,
                                Err(TryRecvError::Disconnected) => {
                                    return glib::ControlFlow::Break;
                                }
                            }
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

            // Game detector: scan /proc every 2 seconds, feed the debouncing
            // GameDetector, and forward state-change events into the
            // BufferController. The controller is responsible for deciding
            // whether to actually arm (it stays in Uninitialized until the
            // portal pick lands in Phase 3, so no StartReplay is sent yet).
            let detector_state = Rc::new(RefCell::new(crate::clips::GameDetector::new()));
            let buf_for_detector = buffer.clone();
            let resources_for_detector = resources.clone();
            glib::timeout_add_seconds_local(2, move || {
                let games = crate::clips::detector::scan_once();
                let evts = detector_state.borrow_mut().tick(&games);
                if !evts.is_empty() {
                    if let Some(res) = resources_for_detector.borrow().as_ref() {
                        if let Some(tx) =
                            res.clip_backend.as_ref().map(|h| h.sender())
                        {
                            let mut buf = buf_for_detector.borrow_mut();
                            for e in evts {
                                log::info!("detector event: {e:?}");
                                match e {
                                    crate::clips::DetectorEvent::GameStarted(g) => {
                                        buf.on_game_started(g, &tx);
                                    }
                                    crate::clips::DetectorEvent::GameStopped { pid } => {
                                        buf.on_game_stopped(pid, &tx);
                                    }
                                }
                            }
                        }
                    }
                }
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
                // Drop the router before sinks so filter-chains/loopbacks are
                // unloaded while the sinks still exist.
                res.router.borrow_mut().take();
                drop(res);
            }
            log::info!("Cleanup complete");
        });
    }

    app.run_with_args(&[] as &[&str]);
}

fn init_pipeline() -> Result<AppResources, String> {
    log::info!("Looking for Arctis Nova Elite audio sink...");
    let headset_sink = router::find_headset_sink()?;
    log::info!("Found headset sink: {headset_sink}");

    log::info!("Creating virtual ChatMix sinks...");
    let sinks = VirtualSinks::create()?;

    log::info!("Setting up audio routing...");
    let router = AudioRouter::create(&headset_sink)?;

    // Spawn the clip-backend thread. It idles until later phases send a
    // StartReplay command — no GSR child is launched here.
    log::info!("Spawning clip backend thread (idle)...");
    let (clip_backend, clip_events) = crate::clips::backend::spawn_backend();

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
        router: Rc::new(RefCell::new(Some(router))),
        shutdown,
        rx: Some(rx),
        writer: Some(writer),
        headset_sink,
        clip_backend: Some(clip_backend),
        clip_events: Some(clip_events),
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
            window.set_sink_volume(sinks::GAME_SINK_NAME, *game);
            window.set_sink_volume(sinks::CHAT_SINK_NAME, *chat);
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

/// Present an AlertDialog with a screenshot of the persisted capture target.
/// Used by the `app.test-clip-capture` action (xdg-portal #1371 mitigation).
fn show_test_capture_dialog(app: &adw::Application, image_path: &std::path::Path) {
    // Note: the Screenshot portal does NOT consult ScreenCast's restore_token.
    // It has its own consent and may pick a different screen on multi-monitor
    // setups. The body text reflects this honestly so we don't lie to the user.
    let dialog = adw::AlertDialog::builder()
        .heading("Capture source preview")
        .body(
            "This is a system screenshot. On multi-monitor setups it may \
             differ from the actual capture target — to verify the real \
             target, save a brief test clip and check which screen was \
             captured.",
        )
        .build();
    let pic = gtk::Picture::for_filename(image_path);
    pic.set_height_request(360);
    pic.set_width_request(640);
    dialog.set_extra_child(Some(&pic));
    dialog.add_response("close", "Close");
    dialog.set_default_response(Some("close"));
    if let Some(window) = app.active_window() {
        dialog.present(Some(&window));
    }
}
