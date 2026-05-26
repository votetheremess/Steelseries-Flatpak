use std::cell::{Cell, RefCell};
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
use crate::audio::state_sync;
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

    // Convert SIGTERM / SIGINT into a graceful GApplication quit on the main
    // loop. Without this, `pkill arctis-chatmix` (SIGTERM) and logout terminate
    // the process before `connect_shutdown` runs, so the clip backend's Drop →
    // Shutdown → disarm path never executes and the inner gpu-screen-recorder
    // (which bwrap shields from our PR_SET_PDEATHSIG) reparents to the session
    // reaper and keeps recording, holding a portal screencast session forever.
    //
    // `glib::unix_signal_add_local` routes the signal through the main loop
    // (async-signal-safe; no handler running in signal context), then we call
    // `app.quit()` which emits `shutdown`, drops `AppResources`, and disarms
    // GSR cleanly. `pkill -9` (SIGKILL) is uncatchable and will still orphan —
    // unavoidable.
    for sig in [libc::SIGTERM, libc::SIGINT] {
        let app_for_signal = app.clone();
        glib::unix_signal_add_local(sig, move || {
            log::info!("Received termination signal ({sig}); quitting cleanly");
            app_for_signal.quit();
            glib::ControlFlow::Break
        });
    }

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

            // Headset-monitor name for the clip pipeline's audio capture.
            // `<headset_sink>.monitor` is the PipeWire convention.
            let headset_sink_monitor = headset_sink
                .as_deref()
                .map(|s| format!("{s}.monitor"))
                .unwrap_or_default();

            // Load persisted clip settings once, share with both the buffer
            // controller (initial CaptureConfig seed) and the settings
            // widgets (live editing). Each settings widget mutates this
            // shared cell and re-saves to disk.
            let clip_settings = Rc::new(RefCell::new(crate::clips::settings::load()));

            // Build the BufferController seeded from saved settings. The
            // portal restore token is wired in later via the auto-resume
            // / wizard actions — until then the controller stays in
            // Uninitialized and never sends StartReplay.
            let buffer = {
                let mut initial_cfg = crate::clips::settings::cfg_from_settings(
                    &clip_settings.borrow(),
                    &headset_sink_monitor,
                );
                // settings::cfg_from_settings doesn't know about a portal
                // token; preserve the None default. The auto-resume block
                // below feeds the persisted token in via on_portal_pick_complete.
                initial_cfg.portal_restore_token = None;
                let buf = crate::clips::BufferController::new(initial_cfg);
                // No detector-driven flags to copy: `auto_arm` /
                // `always_armed` were dropped with the game-detector
                // removal. `onboarding_complete` is seeded later in the
                // auto-resume block once we've reloaded settings.
                Rc::new(RefCell::new(buf))
            };

            // Build the clips-settings hook bundle, but only if the clip
            // backend exists (init_pipeline may have skipped it on
            // headless/error paths). Settings page silently omits the
            // Clips group when this is None — the user can recover by
            // restarting once the device is connected.
            //
            // `clips_page` is filled in by ChatMixWindow::new after it
            // builds the page; the partial context built here carries the
            // app-side runtime hooks (settings cell, buffer, command
            // channel, headset sink) and ChatMixWindow attaches the page
            // before passing it to build_settings_page.
            let clips_settings_ctx_partial = resources
                .borrow()
                .as_ref()
                .and_then(|r| r.clip_backend.as_ref().map(|h| h.sender()))
                .map(|tx| crate::window::ClipsSettingsContextPartial {
                    clip_settings: clip_settings.clone(),
                    buffer: buffer.clone(),
                    cmd_tx: tx,
                    headset_sink_monitor: headset_sink_monitor.clone(),
                });

            let window = ChatMixWindow::new(
                app,
                on_eq_apply,
                on_spatial_apply,
                on_reroute,
                on_mic_reroute,
                headset_sink,
                clips_settings_ctx_partial,
            );

            // After the window is up, push the persisted storage_path into
            // the ClipsPage so the browser's reconcile target matches the
            // user's saved setting (otherwise it shows ~/Videos/Clips on
            // every launch, regardless of any custom path the user picked).
            window
                .clips_page()
                .set_storage_dir(clip_settings.borrow().storage_path.clone());

            // Now that the model has been reconciled against the persisted
            // storage_path, switch the page-state to match the user's
            // onboarding status. Without this the Clips sidebar tab opens
            // on the wizard even when onboarding is long since complete and
            // there are clips on disk — the previously-set
            // `set_visible_child_name("onboarding")` in `build_clips_page`
            // is the construction-time default and the auto-resume block
            // below only runs after the wizard actions are registered, so
            // a returning user with clips would still see the wizard for a
            // moment (and silently — there are no log lines covering that
            // transition).
            window
                .clips_page()
                .sync_to_onboarding_state(clip_settings.borrow().onboarding_complete);

            // Seed the wizard Page 3 widgets with the persisted settings
            // so a returning user lands on their previous values (clip
            // length, storage folder name) rather than the build-time
            // defaults. The hotkey button label stays static for now —
            // see the comment in build_page3_settings.
            {
                let cp = window.clips_page();
                let s = clip_settings.borrow();
                cp.wizard
                    .clip_length_scale
                    .set_value(s.buffer_length as f64);
                cp.wizard
                    .clip_length_label
                    .set_label(&format!("{}s", s.buffer_length));
                cp.wizard.update_storage_label(&s.storage_path);
            }

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
                        let wizard = clips_page.wizard.clone();
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
                                        // Hide install controls, reveal Next.
                                        // The 2-s install watcher will
                                        // confirm-and-keep this state on its
                                        // next tick (and reverse it if GSR
                                        // disappears later).
                                        wizard.show_installed_state();
                                        in_progress.set(false);
                                        return glib::ControlFlow::Break;
                                    }
                                    InstallProgress::Failed { reason } => {
                                        label.set_label(&format!("Install failed: {reason}"));
                                        // Install controls stay visible —
                                        // user can retry. Next remains
                                        // hidden.
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

            // app.gsr-install-manually — present a custom adw::Dialog that
            // exposes the two manual-install fallbacks (app store + copy
            // command) plus a Close button. Wired to the secondary
            // "Install Manually" button on wizard Page 1; keeps the page
            // uncluttered for the happy path.
            //
            // We use a base `adw::Dialog` (not `adw::AlertDialog`) so all
            // three buttons can share identical pill styling + 200 px
            // width. AlertDialog renders its responses as a separate row
            // with non-pill chrome that breaks visual symmetry. Close
            // sits at the bottom of the same vertical Box with a small
            // spacer above it so it reads as "the way out" rather than as
            // a third action peer. Escape-to-close works natively on
            // adw::Dialog (set_can_close is true by default).
            {
                let manual_action = gtk::gio::ActionEntry::builder("gsr-install-manually")
                    .activate(move |app: &adw::Application, _action, _param| {
                        let dialog = adw::Dialog::new();
                        dialog.set_title("Install gpu-screen-recorder manually");

                        let content = gtk::Box::builder()
                            .orientation(gtk::Orientation::Vertical)
                            .spacing(16)
                            .margin_top(24)
                            .margin_bottom(24)
                            .margin_start(24)
                            .margin_end(24)
                            .halign(gtk::Align::Center)
                            .build();

                        let heading = gtk::Label::builder()
                            .label("Install gpu-screen-recorder manually")
                            .css_classes(["title-3"])
                            .halign(gtk::Align::Center)
                            .build();
                        content.append(&heading);

                        let body = gtk::Label::builder()
                            .label(
                                "Choose one of these options to install \
                                 the recorder yourself.",
                            )
                            .wrap(true)
                            .max_width_chars(40)
                            .justify(gtk::Justification::Center)
                            .xalign(0.5)
                            .css_classes(["dim-label"])
                            .build();
                        content.append(&body);

                        let app_for_store = app.clone();
                        let dialog_for_store = dialog.clone();
                        let app_store_btn = gtk::Button::builder()
                            .label("Open in app store")
                            .css_classes(["pill"])
                            .width_request(200)
                            .halign(gtk::Align::Center)
                            .build();
                        app_store_btn.connect_clicked(move |_btn| {
                            app_for_store.activate_action("gsr-open-in-app-store", None);
                            dialog_for_store.close();
                        });
                        content.append(&app_store_btn);

                        let app_for_copy = app.clone();
                        let dialog_for_copy = dialog.clone();
                        let copy_btn = gtk::Button::builder()
                            .label("Copy install command")
                            .css_classes(["pill"])
                            .width_request(200)
                            .halign(gtk::Align::Center)
                            .build();
                        copy_btn.connect_clicked(move |_btn| {
                            app_for_copy.activate_action("gsr-copy-cli", None);
                            // Best-effort confirmation toast so the
                            // silent clipboard set feels responsive.
                            if let Some(window) = app_for_copy.active_window()
                                && let Ok(adw_win) =
                                    window.downcast::<adw::ApplicationWindow>()
                                && let Some(overlay) =
                                    crate::clips::notifications::find_toast_overlay(
                                        &adw_win,
                                    )
                            {
                                let toast = adw::Toast::builder()
                                    .title("Install command copied")
                                    .timeout(3)
                                    .build();
                                overlay.add_toast(toast);
                            }
                            dialog_for_copy.close();
                        });
                        content.append(&copy_btn);

                        // Spacer keeps the visual gap between the two
                        // action buttons and the dismissive Close button
                        // — without it the three buttons would read as a
                        // peer trio instead of "two actions, one exit".
                        content.append(
                            &gtk::Box::builder().height_request(8).build(),
                        );

                        let close_btn = gtk::Button::builder()
                            .label("Close")
                            .css_classes(["pill"])
                            .width_request(200)
                            .halign(gtk::Align::Center)
                            .build();
                        {
                            let dialog = dialog.clone();
                            close_btn.connect_clicked(move |_| {
                                dialog.close();
                            });
                        }
                        content.append(&close_btn);

                        dialog.set_child(Some(&content));

                        if let Some(window) = app.active_window() {
                            dialog.present(Some(&window));
                        }
                    })
                    .build();
                app.add_action_entries([manual_action]);
            }

            // app.copy-clip-hotkey-cmd — copy the dbus-send command that
            // triggers `save-clip` to the system clipboard. Lets users
            // bind the save action from any external shortcut daemon
            // (sxhkd, AutoKey, hyprbinds, …) without having to remember
            // the exact GAction wire format.
            {
                let win_for_copy = window.clone();
                let copy_action = gtk::gio::ActionEntry::builder("copy-clip-hotkey-cmd")
                    .activate(move |_app: &adw::Application, _action, _param| {
                        let display = gtk::prelude::WidgetExt::display(&win_for_copy.window);
                        let clipboard = display.clipboard();
                        clipboard.set_text(crate::clips::settings::save_clip_dbus_command());
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
                let clip_settings_for_next = clip_settings.clone();
                let resources_for_next = resources.clone();
                let next_action = gtk::gio::ActionEntry::builder("wizard-next")
                    .activate(move |_app: &adw::Application, _action, _param| {
                        use crate::clips::WizardStep;
                        match clips_page.current_wizard_step() {
                            WizardStep::InstallGsr => {
                                clips_page.set_wizard_step(WizardStep::PickScreen);
                            }
                            WizardStep::PickScreen => {
                                log::info!(
                                    "wizard-next: PickScreen → Settings; \
                                     marking onboarding_complete=true"
                                );
                                if let Err(e) = crate::clips::settings::mark_onboarding_complete(
                                    &clip_settings_for_next,
                                ) {
                                    log::warn!(
                                        "failed to persist onboarding_complete: {e}"
                                    );
                                } else {
                                    log::info!(
                                        "wizard-next: onboarding_complete persisted to disk"
                                    );
                                }
                                // Mirror the disk flip into the running
                                // BufferController so arming engages
                                // without an app restart. The setter
                                // re-evaluates maybe_arm internally if the
                                // other gates are satisfied (portal
                                // pick + not paused).
                                if let Some(tx) = resources_for_next
                                    .borrow()
                                    .as_ref()
                                    .and_then(|r| r.clip_backend.as_ref().map(|h| h.sender()))
                                {
                                    buffer_for_next
                                        .borrow_mut()
                                        .set_onboarding_complete(true, &tx);
                                }
                                clips_page.set_wizard_step(WizardStep::Settings);
                            }
                            WizardStep::Settings => {
                                // Onboarding is now complete (the flag was
                                // flipped at the PickScreen step above);
                                // hand to the helper so we land on Loaded
                                // if there are already clips on disk, or
                                // Empty otherwise. Using set_state(Empty)
                                // unconditionally would hide existing
                                // clips behind the empty placeholder.
                                clips_page.sync_to_onboarding_state(true);
                            }
                        }
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
                            log::info!("setup-clips: invoking portal picker");
                            match crate::clips::portal::pick_screencast_source().await {
                                Ok(token) => {
                                    log::info!(
                                        "setup-clips: portal returned token (len={})",
                                        token.len()
                                    );
                                    if token.is_empty() {
                                        // Portal succeeded but returned no
                                        // persistent token. Surface as a
                                        // try-again rather than saving an
                                        // empty token (which would later
                                        // round-trip to "no token" and
                                        // re-trigger the picker anyway).
                                        log::warn!(
                                            "setup-clips: portal returned empty \
                                             restore token; treating as try-again"
                                        );
                                        clips_page.wizard.screen_picked_label.set_visible(true);
                                        clips_page.wizard.screen_picked_label.set_label(
                                            "Picker returned no persistent token. \
                                             Try again.",
                                        );
                                        // Stay in not-picked state so the
                                        // user can retry via the still-
                                        // visible Pick screen button.
                                        clips_page.wizard.show_screen_not_picked_state();
                                        return;
                                    }
                                    if let Err(e) = crate::clips::portal::save_token(&token) {
                                        log::warn!("setup-clips: save_token failed: {e}");
                                        return;
                                    }
                                    log::info!(
                                        "setup-clips: token saved successfully \
                                         (will persist across launches)"
                                    );
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
                                            "setup-clips: no clip backend available to \
                                             receive portal pick"
                                        );
                                    }
                                    // Update wizard Page 2: hide Pick
                                    // screen, reveal Next (mirrors Page 1's
                                    // post-install flip).
                                    clips_page.wizard.screen_picked_label.set_visible(true);
                                    clips_page
                                        .wizard
                                        .screen_picked_label
                                        .set_label("Screen picked.");
                                    clips_page.wizard.show_screen_picked_state();
                                }
                                Err(e) => {
                                    log::warn!("setup-clips: portal pick failed: {e}");
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

            // app.pause-recording-toggle — bound to the Pause / Resume /
            // Retry button in the dashboard's row-1 Clips section. The
            // single button serves three distinct verbs depending on the
            // current BufferState:
            //
            //   - Capturing  (Arming / Armed, not user-paused) → pause:
            //     send PauseRecording so the backend disarms its active
            //     GSR session and clears the auto-restart limiter.
            //   - Start Capturing (Paused, or user_paused while in Idle)
            //     → resume: send ResumeRecording (in addition to the
            //     StartReplay that BufferController::resume queues via
            //     maybe_arm) so the restart-attempt budget starts fresh.
            //   - Retry Capturing (ErrorState) → retry: call
            //     BufferController::retry which transitions
            //     ErrorState → Idle and runs maybe_arm.
            //
            // The ErrorState branch is the round-4 recovery path: before
            // it existed, an underlying save-failure dropped the user
            // into ErrorState with the toggle greyed out and no in-app
            // way back. (`app.retry-clip-capture` exists as a separate
            // GAction for D-Bus callers, but no UI surface fired it.)
            //
            // Refreshes the section UI immediately rather than waiting
            // for the next BackendEvent.
            {
                let buffer_for_toggle = buffer.clone();
                let resources_for_toggle = resources.clone();
                let settings_for_toggle = clip_settings.clone();
                let window_for_toggle = window.clone();
                let toggle_action = gtk::gio::ActionEntry::builder("pause-recording-toggle")
                    .activate(move |_app: &adw::Application, _action, _param| {
                        let cmd_tx = resources_for_toggle
                            .borrow()
                            .as_ref()
                            .and_then(|r| r.clip_backend.as_ref().map(|h| h.sender()));
                        let Some(tx) = cmd_tx else {
                            log::warn!("no clip backend available for pause-recording-toggle");
                            return;
                        };
                        // Decide the action based on the buffer's current
                        // state. Read state before mutating so we don't
                        // observe a transient post-mutation state.
                        let current_state = buffer_for_toggle.borrow().state();
                        match current_state {
                            crate::clips::buffer::BufferState::ErrorState => {
                                buffer_for_toggle.borrow_mut().retry(&tx);
                            }
                            _ => {
                                let next_paused = !buffer_for_toggle.borrow().user_paused();
                                if next_paused {
                                    buffer_for_toggle.borrow_mut().pause();
                                    let _ = tx.send(crate::clips::ClipCommand::PauseRecording);
                                } else {
                                    buffer_for_toggle.borrow_mut().resume(&tx);
                                    // resume() may have already sent StartReplay via maybe_arm;
                                    // also send ResumeRecording so the supervisor's
                                    // restart-attempts limiter starts fresh.
                                    let _ = tx.send(crate::clips::ClipCommand::ResumeRecording);
                                }
                            }
                        }
                        // Refresh the clips section UI via the public
                        // accessor on ChatMixWindow so the button label /
                        // sensitivity reflects the new state immediately.
                        let state = buffer_for_toggle.borrow().state();
                        let paused = buffer_for_toggle.borrow().user_paused();
                        let settings = settings_for_toggle.borrow();
                        window_for_toggle.refresh_clips_section(state, paused, &settings);
                    })
                    .build();
                app.add_action_entries([toggle_action]);
            }

            // app.retry-clip-capture — bound to the Retry button shown
            // alongside the dashboard's clip-status badge when
            // BufferState::ErrorState is active. Calls
            // BufferController::retry which transitions ErrorState → Idle
            // and runs maybe_arm; maybe_arm now arms whenever the three
            // always-armed gates are satisfied (portal token + onboarding
            // complete + not user-paused). No-op outside ErrorState (the
            // button is hidden in non-error states, but the action is
            // still safe to dispatch from D-Bus or scripted callers).
            //
            // After retry transitions the buffer to a non-error state we
            // refresh the dashboard indicator so the Retry button hides
            // and the dot color matches the new state without waiting for
            // the next BackendEvent (which may not arrive promptly if the
            // root cause was transient and the next StartReplay succeeds
            // immediately on its own).
            {
                let buffer_for_retry = buffer.clone();
                let resources_for_retry = resources.clone();
                let window_for_retry = window.clone();
                let retry_action = gtk::gio::ActionEntry::builder("retry-clip-capture")
                    .activate(move |_app: &adw::Application, _action, _param| {
                        let cmd_tx = resources_for_retry
                            .borrow()
                            .as_ref()
                            .and_then(|r| r.clip_backend.as_ref().map(|h| h.sender()));
                        if let Some(tx) = cmd_tx {
                            buffer_for_retry.borrow_mut().retry(&tx);
                            let buf = buffer_for_retry.borrow();
                            window_for_retry.set_clips_state(buf.state(), buf.user_paused());
                        } else {
                            log::warn!("no clip backend available for retry-clip-capture");
                        }
                    })
                    .build();
                app.add_action_entries([retry_action]);
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
                        // than the install step. Reset Page 2's button
                        // visibility back to the not-picked state so the
                        // user sees Pick screen again instead of a stale
                        // Next from the previous successful pick.
                        clips_page.wizard.show_screen_not_picked_state();
                        clips_page.wizard.screen_picked_label.set_visible(false);
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
            // path. Triggered from the wizard Page 3 "Pick folder" button.
            // Uses the same `pick_clip_storage_folder` helper that the
            // Settings → Clips → Storage location row uses, so both
            // surfaces present identical UX (modal dialog, same accept
            // label, same "seed initial folder from current setting" UX,
            // same cancellation handling). On a successful pick we:
            //   - update the in-memory ClipSettings cell + persist,
            //   - update the wizard's storage_label preview,
            //   - update the ClipsPage's storage_dir so the browser
            //     reconciles against the new dir,
            //   - push a fresh CaptureConfig to the buffer so the next
            //     arm uses the new output_dir.
            {
                let clip_settings_for_pick = clip_settings.clone();
                let buffer_for_pick = buffer.clone();
                let resources_for_pick = resources.clone();
                let monitor_for_pick = headset_sink_monitor.clone();
                let window_for_pick = window.clone();
                let pick_storage_action =
                    gtk::gio::ActionEntry::builder("pick-clip-storage")
                        .activate(move |_app: &adw::Application, _action, _param| {
                            let parent: Option<gtk::Window> =
                                Some(window_for_pick.window.clone().upcast());
                            let initial = clip_settings_for_pick.borrow().storage_path.clone();
                            // Clones for the move into the picker callback.
                            let clip_settings = clip_settings_for_pick.clone();
                            let buffer = buffer_for_pick.clone();
                            let resources = resources_for_pick.clone();
                            let monitor = monitor_for_pick.clone();
                            let window = window_for_pick.clone();
                            crate::clips::settings::pick_clip_storage_folder(
                                parent,
                                &initial,
                                move |path| {
                                    {
                                        let mut s = clip_settings.borrow_mut();
                                        if s.storage_path == path {
                                            return;
                                        }
                                        s.storage_path = path.clone();
                                    }
                                    if let Err(e) = crate::clips::settings::save(
                                        &clip_settings.borrow(),
                                    ) {
                                        log::warn!("clip settings save failed: {e}");
                                    }
                                    // Mirror the new folder name into the
                                    // wizard Page 3 button so the user sees
                                    // their pick confirmed inline. Only the
                                    // basename — full paths blow out the
                                    // narrow card.
                                    window
                                        .clips_page()
                                        .wizard
                                        .update_storage_label(&path);
                                    // Update the browser dir so the next
                                    // reconcile lands on the new folder.
                                    window.clips_page().set_storage_dir(path.clone());
                                    // Push a fresh CaptureConfig so the
                                    // next arm uses the new output_dir.
                                    let cmd_tx = resources
                                        .borrow()
                                        .as_ref()
                                        .and_then(|r| {
                                            r.clip_backend.as_ref().map(|h| h.sender())
                                        });
                                    if let Some(tx) = cmd_tx {
                                        let cfg = crate::clips::settings::cfg_from_settings(
                                            &clip_settings.borrow(),
                                            &monitor,
                                        );
                                        buffer.borrow_mut().on_config_change(cfg, &tx);
                                    }
                                },
                            );
                        })
                        .build();
                app.add_action_entries([pick_storage_action]);
            }

            // app.show-clip(s: path) — invoked from the saved-clip toast
            // (when the window is visible) or the desktop notification (when
            // hidden). The parameter is the absolute path to the saved clip.
            // We present the window and switch to the Clips tab so the user
            // lands on the new clip. Selecting the specific item in the
            // GridView is intentionally deferred — selection-by-filename in
            // a `gio::ListStore`-backed GridView is non-trivial and the
            // tab-level navigation already gives the user direct access to
            // the freshly-saved clip (which appears at the top of the grid
            // after the next reconcile, normally within the next 100 ms).
            {
                let win_for_show = window.clone();
                let show_action = gtk::gio::ActionEntry::builder("show-clip")
                    .parameter_type(Some(&String::static_variant_type()))
                    .activate(move |_app: &adw::Application, _action, param| {
                        let path = param.and_then(|p| p.get::<String>()).unwrap_or_default();
                        log::info!("app.show-clip activated for {path:?}");
                        win_for_show.window.set_visible(true);
                        win_for_show.window.present();
                        win_for_show.show_clips_tab();
                    })
                    .build();
                app.add_action_entries([show_action]);
            }

            // app.show-clips-settings — bound to the home-page Clips
            // card's Duration / Quick Capture buttons. Presents the
            // window and flips the sidebar to Settings so the user can
            // edit recording length / hotkey without hunting the menu.
            // Settings page lays the Clips group near the top, so a
            // plain tab jump is fine (no need to scroll the
            // PreferencesPage).
            {
                let win_for_settings = window.clone();
                let settings_jump = gtk::gio::ActionEntry::builder("show-clips-settings")
                    .activate(move |_app: &adw::Application, _action, _param| {
                        win_for_settings.window.set_visible(true);
                        win_for_settings.window.present();
                        win_for_settings.show_settings_tab();
                    })
                    .build();
                app.add_action_entries([settings_jump]);
            }

            // app.rebind-clip-hotkey — re-open the GlobalShortcuts portal
            // dialog so the user can change the chord bound to save-clip.
            // ashpd 0.10 has no `ConfigureShortcuts` API, so the workaround
            // is to fire off a fresh bind_shortcuts call: KDE's portal
            // re-displays its picker for any unconfirmed/changed bindings,
            // which is the closest thing to a "rebind" affordance we get.
            // Existing activations from the startup session keep flowing.
            {
                let win_for_rebind = window.clone();
                let settings_for_rebind = clip_settings.clone();
                let rebind_action = gtk::gio::ActionEntry::builder("rebind-clip-hotkey")
                    .activate(move |_app: &adw::Application, _action, _param| {
                        // The activate handler is `Fn`, so we have to
                        // clone per-call. Cheap (Rc + GObject ref bumps).
                        let win_for_async = win_for_rebind.clone();
                        let parent = win_for_rebind.window.clone();
                        let settings_for_async = settings_for_rebind.clone();
                        glib::MainContext::default().spawn_local(async move {
                            // Pass the parent window so the portal can
                            // resolve our app id from xdg-foreign. Without
                            // this, KDE's portal returns NotAllowed when
                            // we're running outside a Flatpak (the
                            // standard dev-mode launch).
                            match crate::clips::hotkey::rebind_shortcuts(
                                Some(&parent),
                                Some(settings_for_async),
                            )
                            .await
                            {
                                Ok(()) => {}
                                Err(e) => {
                                    let msg = e.to_string();
                                    if msg.contains("NotAllowed") || msg.contains("app id") {
                                        // Surface as a toast — the only
                                        // real fix is "install as a
                                        // Flatpak", which we can't do
                                        // from here. Tell the user how
                                        // to work around it for now.
                                        if let Some(overlay) =
                                            crate::clips::notifications::find_toast_overlay(
                                                &win_for_async.window,
                                            )
                                        {
                                            overlay.add_toast(
                                                adw::Toast::builder()
                                                    .title(
                                                        "Rebinding requires the app to be installed as a Flatpak. \
                                                         For now, change the shortcut in System Settings.",
                                                    )
                                                    .timeout(8)
                                                    .build(),
                                            );
                                        }
                                        log::warn!("rebind_shortcuts portal rejected: {msg}");
                                    } else {
                                        log::warn!("rebind_shortcuts failed: {msg}");
                                    }
                                }
                            }
                        });
                    })
                    .build();
                app.add_action_entries([rebind_action]);
            }

            // GlobalShortcuts portal listener. Spawned on the glib main
            // context so the future's !Send constraints (it forwards into
            // app.activate_action, which touches the GApplication) are
            // satisfied. Runs forever; if the portal session dies, the
            // future returns and we log it — the user can retry by
            // triggering app.rebind-clip-hotkey, which spawns a fresh
            // session.
            //
            // First, call `ashpd::register_host_app(APP_ID)`. xdg-desktop-portal
            // 1.19.4+ exposes `org.freedesktop.host.portal.Registry` which lets
            // a host-installed (non-Flatpak) caller publish its application id
            // to the portal daemon. Without it, KDE's xdg-desktop-portal-kde
            // returns `NotAllowed: An app id is required` from
            // `bind_shortcuts` and the listener exits before any shortcut can
            // fire. Older portal daemons silently ignore the call (ashpd
            // returns an error which we log but tolerate; it just means
            // GlobalShortcuts may not work — passing the parent window via
            // xdg-foreign is the legacy fallback for that case).
            //
            // We pass the active window so the portal can also fall back to
            // xdg-foreign for app-id derivation on portal versions that
            // pre-date Registry.
            {
                let app_for_hotkey = app.clone();
                let parent_for_hotkey = window.window.clone();
                let settings_for_hotkey = clip_settings.clone();
                glib::MainContext::default().spawn_local(async move {
                    // Construct the AppID from the existing constant. The
                    // unwrap is justified: APP_ID is a compile-time literal
                    // we've validated against ashpd's app-id rules.
                    match APP_ID.parse::<ashpd::AppID>() {
                        Ok(app_id) => {
                            match ashpd::register_host_app(app_id).await {
                                Ok(()) => {
                                    log::info!(
                                        "ashpd::register_host_app succeeded with id: {APP_ID}"
                                    );
                                }
                                Err(e) => {
                                    log::warn!(
                                        "ashpd::register_host_app failed (xdg-desktop-portal may be older than 1.19.4): {e}. \
                                         Falling back to no-app-id mode; GlobalShortcuts portal may not work."
                                    );
                                }
                            }
                        }
                        Err(e) => {
                            log::warn!(
                                "APP_ID failed ashpd validation, skipping register_host_app: {e}"
                            );
                        }
                    }

                    let app_for_cb = app_for_hotkey.clone();
                    let result = crate::clips::hotkey::run_global_shortcuts(
                        Some(&parent_for_hotkey),
                        move |id| {
                            let action_name = match id {
                                "save-clip" => "save-clip",
                                "save-clip-short" => "save-clip-short",
                                "save-clip-medium" => "save-clip-medium",
                                "save-clip-long" => "save-clip-long",
                                other => {
                                    log::debug!("ignoring unknown shortcut id: {other}");
                                    return;
                                }
                            };
                            app_for_cb.activate_action(action_name, None);
                        },
                        Some(settings_for_hotkey),
                    )
                    .await;
                    if let Err(e) = result {
                        log::warn!("GlobalShortcuts portal listener exited: {e}");
                    }
                });
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
                    if installed {
                        clips_page.wizard.show_installed_state();
                        clips_page.wizard.install_status_label.set_visible(true);
                        clips_page
                            .wizard
                            .install_status_label
                            .set_label("Installed.");
                    } else {
                        // GSR went missing (uninstalled from the system app
                        // store, or never installed). Restore Install +
                        // Install Manually so the user can re-install; hide
                        // Next.
                        clips_page.wizard.show_not_installed_state();
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
                let resumed_settings = crate::clips::settings::load();
                let token = crate::clips::portal::load_token();
                let gsr_ok = crate::clips::gsr_install::is_installed();

                log::info!(
                    "auto-resume: gsr_ok={} token={} onboarding_complete={}",
                    gsr_ok,
                    if let Some(ref t) = token {
                        format!("Some(len={})", t.len())
                    } else {
                        "None".to_string()
                    },
                    resumed_settings.onboarding_complete
                );

                // Seed the BufferController's onboarding gate from disk
                // BEFORE any `on_portal_pick_complete` call below — without
                // this, arm 4 (token saved but wizard incomplete) would
                // auto-arm immediately on every relaunch, defeating the
                // gate. (Critic Blocker 1.)
                if let Some(tx) = resources
                    .borrow()
                    .as_ref()
                    .and_then(|r| r.clip_backend.as_ref().map(|h| h.sender()))
                {
                    buffer
                        .borrow_mut()
                        .set_onboarding_complete(resumed_settings.onboarding_complete, &tx);
                }

                if resumed_settings.onboarding_complete && gsr_ok && token.is_some() {
                    log::info!(
                        "auto-resume: arm 1 (skip wizard, go to browser)"
                    );
                    if let Some(t) = token {
                        let cmd_tx = resources
                            .borrow()
                            .as_ref()
                            .and_then(|r| r.clip_backend.as_ref().map(|h| h.sender()));
                        if let Some(tx) = cmd_tx {
                            buffer.borrow_mut().on_portal_pick_complete(t, &tx);
                        }
                    }
                    // Pick Loaded vs Empty based on the reconciled model.
                    // The earlier post-`set_storage_dir` call already did
                    // this once, but the auto-resume block runs late in
                    // startup (after wizard actions are registered) so we
                    // re-sync defensively in case anything ran in between
                    // that would have flipped the state.
                    window
                        .clips_page()
                        .sync_to_onboarding_state(true);
                } else if !gsr_ok {
                    log::info!("auto-resume: arm 2 (Page 1 / install GSR)");
                    window
                        .clips_page()
                        .set_wizard_step(crate::clips::WizardStep::InstallGsr);
                } else if token.is_none() {
                    log::info!(
                        "auto-resume: arm 3 (Page 2 / pick screen — no token \
                         on disk)"
                    );
                    window
                        .clips_page()
                        .set_wizard_step(crate::clips::WizardStep::PickScreen);
                    // Already past page 1 — keep its Next visible (and
                    // hide the install controls) so navigating back from
                    // Page 2 lands on the post-install state, not the
                    // install prompt.
                    window.clips_page().wizard.show_installed_state();
                } else {
                    log::info!(
                        "auto-resume: arm 4 (Page 2 / token saved but \
                         onboarding_complete=false — confirm pick)"
                    );
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
                        "Previously picked. Click Next to keep.",
                    );
                    // Page 2 in picked state: only Next visible. The user
                    // can still change source later via Settings → Reset.
                    cp.wizard.show_screen_picked_state();
                    // Already past page 1 — keep its Next visible (and
                    // hide the install controls) so navigating back from
                    // Page 2 lands on the post-install state, not the
                    // install prompt.
                    cp.wizard.show_installed_state();
                    // Onboarding state remains incomplete until user
                    // confirms via Next.
                }
            }

            // Clip backend event poll. Drains BackendEvents into the
            // BufferController so its state stays in sync with the backend
            // thread (Armed / Disarmed / Saved / errors). Each tick also
            // refreshes the dashboard's clip-status badge so the user sees
            // live state transitions (Idle → Arming → Armed → Saving →
            // Armed) without hunting for the Clips tab.
            //
            // On `Saved` events we additionally:
            //   1. Spawn a worker to extract a thumbnail via ffmpeg (~50–100 ms).
            //   2. Post back to the GTK main thread to dispatch a toast (when
            //      the window is visible) or a desktop notification (when
            //      hidden) — see `clips::notifications::notify_saved`.
            //   3. If the user has a disk cap configured, also spawn a
            //      retention worker that deletes oldest clips until under cap
            //      and refreshes the Clips browser model afterwards.
            let resources_for_clips = resources.clone();
            let buf_for_events = buffer.clone();
            let window_for_indicator = window.clone();
            let app_for_clips = app.clone();
            let window_for_clips = window.clone();
            glib::timeout_add_local(Duration::from_millis(100), move || {
                let mut state_changed = false;
                let mut saved_paths: Vec<std::path::PathBuf> = Vec::new();
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
                                    if let crate::clips::BackendEvent::Saved { path, duration_ms } = &evt {
                                        log::info!(
                                            "[clip-save] GTK side received Saved {{ path={}, duration_ms={} }}; queuing thumbnail/notification + library refresh",
                                            path.display(),
                                            duration_ms
                                        );
                                        saved_paths.push(path.clone());
                                    }
                                    buf.on_backend_event(&evt, &tx);
                                    state_changed = true;
                                }
                                Err(TryRecvError::Empty) => break,
                                Err(TryRecvError::Disconnected) => {
                                    return glib::ControlFlow::Break;
                                }
                            }
                        }
                    }
                }
                if state_changed {
                    let buf = buf_for_events.borrow();
                    window_for_indicator.set_clips_state(buf.state(), buf.user_paused());
                }
                // Dispatch saved-clip notifications and retention. Done after
                // releasing the resources borrow above so the worker threads
                // we spawn can re-borrow if they need to (none currently do,
                // but the discipline makes adding new state lookups safer).
                for saved_path in saved_paths {
                    dispatch_saved_clip(&app_for_clips, &window_for_clips, saved_path);
                }
                glib::ControlFlow::Continue
            });

            // Seed the indicator with the buffer's initial state so the
            // badge reflects auto-resume / Uninitialized correctly without
            // waiting for the first BackendEvent (which only fires once
            // GSR is armed — could be never if the user is just sitting
            // on the dashboard).
            {
                let buf = buffer.borrow();
                window.set_clips_state(buf.state(), buf.user_paused());
            }

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

            // Unified state-sync tick (2 s). Orchestrates three concerns in one
            // closure on the GTK main thread:
            //   1. Stream reconciliation — auto-route new streams to saved
            //      sinks AND observe user-driven moves between managed sinks.
            //   2. Mic hotplug — if the user's preferred mic just appeared,
            //      reroute back to it.
            //   3. Virtual-source volume capture — write through the latest
            //      Music/Aux/Mic volumes so they survive an app restart.
            //
            // See docs/superpowers/specs/2026-05-13-routing-volume-clips-fixes-design.md.
            let sync_state = Rc::new(RefCell::new(state_sync::StateSyncState::new_seeded()));
            let sync_state_for_tick = sync_state.clone();
            let resources_for_tick = resources.clone();
            glib::timeout_add_seconds_local(STREAM_WATCH_SECS, move || {
                let mut state = sync_state_for_tick.borrow_mut();
                if let Some(res) = resources_for_tick.borrow().as_ref() {
                    if let Some(router) = res.router.borrow_mut().as_mut() {
                        state_sync::tick(&mut state, router);
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

            // Capture virtual-source volumes one final time before destroying
            // sinks. The periodic state-sync tick handles steady-state; this
            // catches anything the user changed in the last <2 s window.
            for name in [sinks::MUSIC_SINK_NAME, sinks::AUX_SINK_NAME] {
                if let Ok(vol) = sinks::get_sink_volume(name) {
                    persistence::save_volume_entry(name, vol);
                }
            }
            if let Ok(vol) = sinks::get_source_volume(sinks::MIC_SOURCE_NAME) {
                persistence::save_volume_entry(sinks::MIC_SOURCE_NAME, vol);
            }

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
    // Install the .desktop file at ~/.local/share/applications/ so xdg-desktop-portal
    // can resolve our APP_ID via its Registry interface. Without this,
    // `ashpd::register_host_app` succeeds at the protocol level but the portal still
    // logs "App info not found for '<APP_ID>'" and KDE rejects the GlobalShortcuts
    // bind. We previously only installed the autostart copy at ~/.config/autostart/,
    // which is NOT in the data search path. Idempotent — only writes if content
    // drifts (e.g. binary path changed between dev sessions).
    match crate::desktop_file::install_desktop_file(APP_ID) {
        Ok(path) => log::info!("Installed desktop file at {}", path.display()),
        Err(e) => log::warn!(
            "Failed to install desktop file (register_host_app may fail): {e}"
        ),
    }

    log::info!("Looking for Arctis Nova Elite audio sink...");
    let headset_sink = router::find_headset_sink()?;
    log::info!("Found headset sink: {headset_sink}");

    log::info!("Creating virtual ChatMix sinks...");
    let sinks = VirtualSinks::create()?;

    // Restore user-set volumes for Music/Aux/Mic — those sinks get recreated
    // above so their volumes reset to default every launch. Do this BEFORE
    // loading the EQ filter-chains so the filter-chains see the right levels.
    for (name, pct) in persistence::load_volumes() {
        let result = if name == sinks::MIC_SOURCE_NAME {
            sinks::set_source_volume(&name, pct)
        } else {
            sinks::set_sink_volume(&name, pct)
        };
        match result {
            Ok(()) => log::info!("Restored volume: {name} = {pct}%"),
            Err(e) => log::warn!("Failed to restore volume for {name}: {e}"),
        }
    }

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

/// Spawn workers for a freshly saved clip:
///   1. Thumbnail extraction → notify_saved (toast / desktop notification).
///   2. Retention enforcement (if disk_cap_gb is Some) → refresh the Clips
///      grid after deleting any over-cap clips.
///
/// Called from the BackendEvent poll handler when we see a `Saved` event.
/// Both `ensure_thumbnail` (ffmpeg shell-out, ~50–100 ms) and
/// `enforce_retention` (filesystem walk + delete, can be tens of ms on
/// large directories) are heavy enough that we can't run them on the GTK
/// main thread. We use the same `std::sync::mpsc` + glib timer pattern the
/// rest of the app uses (HID listener, tray, clip backend): the workers
/// write a one-shot result through a channel, a 50 ms glib timer drains it
/// on the main thread, and the dispatch (toast/notification + grid
/// refresh) happens there.
fn dispatch_saved_clip(
    app: &adw::Application,
    window: &Rc<ChatMixWindow>,
    saved_path: std::path::PathBuf,
) {
    use std::sync::mpsc;
    // Pull out the bare ApplicationWindow up front. The `Rc<ChatMixWindow>`
    // wrapper is `!Send` and would block the worker thread spawn, but the
    // raw GObject window inside is fine to clone — we only ever use it on
    // the main thread inside `notify_saved`.
    let app_for_notify = app.clone();
    let window_for_notify = window.window.clone();

    // ---- Thumbnail + notification -------------------------------------
    let saved_for_thread = saved_path.clone();
    let (tx, rx) = mpsc::channel::<Option<std::path::PathBuf>>();
    std::thread::spawn(move || {
        let storage_dir = saved_for_thread
            .parent()
            .map(|p| p.to_path_buf())
            .unwrap_or_default();
        let filename = saved_for_thread
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned();
        let thumb = if storage_dir.as_os_str().is_empty() {
            None
        } else {
            crate::clips::thumbnail::ensure_thumbnail(&storage_dir, &filename)
                .map_err(|e| log::warn!("thumbnail extract failed for {filename}: {e}"))
                .ok()
        };
        let _ = tx.send(thumb);
    });
    // Drain the one-shot channel from the main thread. We can't move `rx`
    // into a `timeout_add_local` closure that returns ControlFlow::Continue,
    // so the closure consumes `rx` and returns Break the moment it gets a
    // result (or the sender drops, which means the worker died). The 50 ms
    // tick is short enough that even back-to-back saves feel instant.
    let saved_for_main = saved_path.clone();
    glib::timeout_add_local(Duration::from_millis(50), move || {
        match rx.try_recv() {
            Ok(thumb) => {
                crate::clips::notifications::notify_saved(
                    &app_for_notify,
                    &window_for_notify,
                    &saved_for_main,
                    thumb.as_deref(),
                );
                glib::ControlFlow::Break
            }
            Err(mpsc::TryRecvError::Empty) => glib::ControlFlow::Continue,
            Err(mpsc::TryRecvError::Disconnected) => {
                log::warn!("dispatch_saved_clip: thumbnail worker disconnected");
                glib::ControlFlow::Break
            }
        }
    });

    // ---- Retention enforcement ----------------------------------------
    // Settings are loaded fresh each save so a user toggling the cap in
    // Settings → Clips takes effect on the next save without needing to
    // restart the app. `load()` is a small file read + parse, ~ms-level.
    let settings = crate::clips::settings::load();
    if let Some(cap_gb) = settings.disk_cap_gb {
        let storage_dir = match saved_path.parent() {
            Some(p) => p.to_path_buf(),
            None => return,
        };
        let clips_page = window.clips_page();
        let (rtx, rrx) = mpsc::channel::<Vec<String>>();
        std::thread::spawn(move || {
            let deleted = crate::clips::library::enforce_retention(&storage_dir, cap_gb);
            let _ = rtx.send(deleted);
        });
        glib::timeout_add_local(Duration::from_millis(100), move || {
            match rrx.try_recv() {
                Ok(deleted) => {
                    if !deleted.is_empty() {
                        log::info!(
                            "retention: deleted {} clip(s) over {} GB cap: {:?}",
                            deleted.len(),
                            cap_gb,
                            deleted
                        );
                        // Repopulate the GridView so the deleted clips
                        // disappear from the browser. Safe to call
                        // unconditionally — `refresh_clips_model` is a
                        // reconcile against the disk state.
                        clips_page.refresh_clips_model();
                    }
                    glib::ControlFlow::Break
                }
                Err(mpsc::TryRecvError::Empty) => glib::ControlFlow::Continue,
                Err(mpsc::TryRecvError::Disconnected) => {
                    log::warn!("dispatch_saved_clip: retention worker disconnected");
                    glib::ControlFlow::Break
                }
            }
        });
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
             differ from the actual capture target. To verify the real \
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
