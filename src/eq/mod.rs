mod biquad;
mod controls;
mod graph;
pub mod model;
pub mod presets;
pub mod sonar_import;

use std::rc::Rc;

use adw::prelude::*;
use gtk::glib;

use model::*;

pub fn build_eq_page(
    on_eq_apply: Option<Rc<dyn Fn(EqTarget, [Band; NUM_BANDS])>>,
    on_reroute: Option<Rc<dyn Fn(&str, &str)>>,
    on_mic_reroute: Option<Rc<dyn Fn(&str)>>,
    headset_sink: Option<String>,
) -> (gtk::Widget, Option<crate::mixer::MixerWidgets>) {
    let state = new_shared_state();

    // Load persisted EQ state if available
    {
        let saved = presets::load_eq_state();
        if !saved.is_empty() {
            let mut st = state.borrow_mut();
            for (target, sink_eq) in saved {
                st.sinks.insert(target, sink_eq);
            }
        }
    }

    let page = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(0)
        .margin_top(8)
        .margin_bottom(8)
        .margin_start(12)
        .margin_end(12)
        .build();

    // Forward-declared refs
    let graph_ref: Rc<std::cell::RefCell<Option<graph::EqGraph>>> =
        Rc::new(std::cell::RefCell::new(None));
    let controls_update_ref: Rc<std::cell::RefCell<Option<Rc<dyn Fn()>>>> =
        Rc::new(std::cell::RefCell::new(None));
    let preset_dropdown_ref: Rc<std::cell::RefCell<Option<gtk::DropDown>>> =
        Rc::new(std::cell::RefCell::new(None));
    let preset_updating: Rc<std::cell::Cell<bool>> =
        Rc::new(std::cell::Cell::new(false));
    let content_stack_ref: Rc<std::cell::RefCell<Option<gtk::Stack>>> =
        Rc::new(std::cell::RefCell::new(None));
    let eq_controls_ref: Rc<std::cell::RefCell<Option<gtk::Box>>> =
        Rc::new(std::cell::RefCell::new(None));

    // Shared on_changed callback
    let on_changed: Rc<dyn Fn()> = {
        let graph_ref = graph_ref.clone();
        let controls_update_ref = controls_update_ref.clone();
        let preset_dropdown_ref = preset_dropdown_ref.clone();
        let preset_updating = preset_updating.clone();
        let state = state.clone();
        let on_eq_apply = on_eq_apply.clone();
        Rc::new(move || {
            if let Some(ref g) = *graph_ref.borrow() {
                g.queue_draw();
            }
            // Controls bar is always visible; update fn handles empty state
            if let Some(ref update) = *controls_update_ref.borrow() {
                update();
            }
            if !preset_updating.get() {
                let preset_name = state.borrow().active_sink_eq().preset_name.clone();
                if let Some(ref dd) = *preset_dropdown_ref.borrow() {
                    preset_updating.set(true);
                    update_preset_selection(dd, preset_name.as_deref());
                    preset_updating.set(false);
                }
            }
            presets::save_eq_state(&state.borrow().sinks);

            // Notify the audio pipeline (debounced by the caller)
            if let Some(ref apply) = on_eq_apply {
                let s = state.borrow();
                apply(s.active_target, s.active_sink_eq().bands);
            }
        })
    };

    // -----------------------------------------------------------------------
    // Top bar: sink tabs + show all + reset + preset
    // -----------------------------------------------------------------------
    let top_bar = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(8)
        .margin_bottom(8)
        .build();
    top_bar.add_css_class("eq-top-bar");

    // Mixer button — separate pill, same radio group but not visually linked
    let mixer_btn = gtk::ToggleButton::builder()
        .label("Mixer")
        .build();

    let tab_box = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(0)
        .build();
    tab_box.add_css_class("linked");

    let mut first_sink_tab = true;
    for &target in EqTarget::ALL {
        let btn = gtk::ToggleButton::builder()
            .label(target.label())
            .build();

        btn.set_group(Some(&mixer_btn));
        if first_sink_tab {
            btn.set_active(true);
            first_sink_tab = false;
        }

        {
            let state = state.clone();
            let on_changed = on_changed.clone();
            let content_stack_ref = content_stack_ref.clone();
            let eq_controls_ref = eq_controls_ref.clone();
            btn.connect_toggled(move |b| {
                if b.is_active() {
                    state.borrow_mut().active_target = target;
                    on_changed();
                    // Switch back to EQ view
                    if let Some(ref stack) = *content_stack_ref.borrow() {
                        stack.set_visible_child_name("eq");
                    }
                    if let Some(ref controls) = *eq_controls_ref.borrow() {
                        controls.set_visible(true);
                    }
                }
            });
        }

        tab_box.append(&btn);
    }

    // Wire mixer button to switch to mixer view
    {
        let content_stack_ref = content_stack_ref.clone();
        let eq_controls_ref = eq_controls_ref.clone();
        mixer_btn.connect_toggled(move |b| {
            if b.is_active() {
                if let Some(ref stack) = *content_stack_ref.borrow() {
                    stack.set_visible_child_name("mixer");
                }
                if let Some(ref controls) = *eq_controls_ref.borrow() {
                    controls.set_visible(false);
                }
            }
        });
    }

    top_bar.append(&mixer_btn);
    top_bar.append(&tab_box);

    let spacer = gtk::Box::builder().hexpand(true).build();
    top_bar.append(&spacer);

    // EQ-only controls — hidden when mixer view is active
    let eq_controls_box = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(8)
        .build();

    // "Show All" toggle
    let show_all_box = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(6)
        .valign(gtk::Align::Center)
        .build();
    let show_all_label = gtk::Label::new(Some("Show All"));
    show_all_label.add_css_class("dim-label");
    let show_all_switch = gtk::Switch::builder()
        .valign(gtk::Align::Center)
        .active(false)
        .tooltip_text("Show all band curves and colors")
        .build();
    show_all_box.append(&show_all_label);
    show_all_box.append(&show_all_switch);
    eq_controls_box.append(&show_all_box);

    // Reset button
    let reset_btn = gtk::Button::builder()
        .label("Reset")
        .tooltip_text("Reset EQ to flat")
        .build();
    reset_btn.add_css_class("destructive-action");
    {
        let state = state.clone();
        let on_changed = on_changed.clone();
        reset_btn.connect_clicked(move |_| {
            let mut st = state.borrow_mut();
            let sink_eq = st.active_sink_eq_mut();
            sink_eq.bands = model::default_bands();
            sink_eq.preset_name = Some("Flat".to_string());
            st.selected_band = None;
            drop(st);
            on_changed();
        });
    }
    eq_controls_box.append(&reset_btn);

    // Preset dropdown
    let preset_dropdown = build_preset_dropdown(state.clone(), on_changed.clone(), preset_updating.clone());
    eq_controls_box.append(&preset_dropdown);
    *preset_dropdown_ref.borrow_mut() = Some(preset_dropdown.clone());

    // Import from Sonar button — paste a share URL to pull a Sonar preset
    let import_btn = gtk::Button::builder()
        .label("Import Sonar")
        .tooltip_text("Import a preset from a SteelSeries Sonar share URL")
        .build();
    {
        let state = state.clone();
        let on_changed = on_changed.clone();
        let preset_dropdown = preset_dropdown.clone();
        import_btn.connect_clicked(move |btn| {
            let window = btn
                .root()
                .and_then(|r| r.downcast::<gtk::Window>().ok());
            show_import_dialog(window.as_ref(), state.clone(), on_changed.clone(), preset_dropdown.clone());
        });
    }
    eq_controls_box.append(&import_btn);

    top_bar.append(&eq_controls_box);
    *eq_controls_ref.borrow_mut() = Some(eq_controls_box);

    page.append(&top_bar);

    // -----------------------------------------------------------------------
    // Content stack: EQ view and Mixer view
    // -----------------------------------------------------------------------
    let content_stack = gtk::Stack::builder()
        .vexpand(true)
        .transition_type(gtk::StackTransitionType::Crossfade)
        .transition_duration(150)
        .build();

    // -- EQ view --
    let eq_view = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(0)
        .build();

    let eq_graph = graph::EqGraph::new(state.clone(), on_changed.clone());
    let frame = gtk::Frame::builder()
        .child(&eq_graph.drawing_area)
        .build();
    frame.add_css_class("eq-graph-frame");
    eq_view.append(&frame);

    // Wire "Show All" switch
    {
        let show_all = eq_graph.show_all.clone();
        let graph_area = eq_graph.drawing_area.clone();
        show_all_switch.connect_state_set(move |_switch, active| {
            show_all.set(active);
            graph_area.queue_draw();
            glib::Propagation::Proceed
        });
    }

    *graph_ref.borrow_mut() = Some(eq_graph);

    let (controls_panel, controls_update) =
        controls::build_floating_panel(state.clone(), on_changed.clone());
    controls_panel.set_halign(gtk::Align::Center);
    controls_panel.set_margin_top(6);
    eq_view.append(&controls_panel);

    *controls_update_ref.borrow_mut() = Some(controls_update);

    content_stack.add_named(&eq_view, Some("eq"));

    // -- Mixer view --
    let (mixer_widget, mixer_widgets) = crate::mixer::build_mixer_content(
        on_reroute,
        on_mic_reroute,
        headset_sink,
    );
    content_stack.add_named(&mixer_widget, Some("mixer"));

    *content_stack_ref.borrow_mut() = Some(content_stack.clone());

    page.append(&content_stack);

    (page.upcast(), Some(mixer_widgets))
}

// ---------------------------------------------------------------------------
// Preset dropdown
// ---------------------------------------------------------------------------

fn build_preset_dropdown(
    state: SharedEqState,
    on_changed: Rc<dyn Fn()>,
    preset_updating: Rc<std::cell::Cell<bool>>,
) -> gtk::DropDown {
    let model = gtk::StringList::new(&[]);
    refresh_preset_list(&model);

    let dropdown = gtk::DropDown::builder()
        .model(&model)
        .tooltip_text("EQ Preset")
        .build();

    {
        let st = state.borrow();
        let preset_name = st.active_sink_eq().preset_name.as_deref();
        update_preset_selection(&dropdown, preset_name);
    }

    {
        let state = state.clone();
        let on_changed = on_changed.clone();
        let model_ref = model.clone();
        let preset_updating = preset_updating.clone();
        dropdown.connect_selected_notify(move |dd| {
            if preset_updating.get() { return; }

            let idx = dd.selected() as usize;
            let n = model_ref.n_items() as usize;
            if idx >= n { return; }

            let name = model_ref
                .string(idx as u32)
                .map(|s| s.to_string())
                .unwrap_or_default();

            if name == "Custom" { return; }

            if let Some(bands) = presets::load_built_in(&name) {
                let mut st = state.borrow_mut();
                let sink_eq = st.active_sink_eq_mut();
                sink_eq.bands = bands;
                sink_eq.preset_name = Some(name.clone());
                drop(st);
                on_changed();
                return;
            }

            if let Some(bands) = presets::load_custom_preset(&name) {
                let mut st = state.borrow_mut();
                let sink_eq = st.active_sink_eq_mut();
                sink_eq.bands = bands;
                sink_eq.preset_name = Some(name.clone());
                drop(st);
                on_changed();
            }
        });
    }

    dropdown
}

fn refresh_preset_list(model: &gtk::StringList) {
    while model.n_items() > 0 {
        model.remove(0);
    }
    for name in presets::built_in_names() {
        model.append(name);
    }
    let custom = presets::list_custom_presets();
    for name in &custom {
        model.append(name);
    }
    model.append("Custom");
}

fn update_preset_selection(dropdown: &gtk::DropDown, preset_name: Option<&str>) {
    let model = dropdown
        .model()
        .and_downcast::<gtk::StringList>()
        .unwrap();
    let target = preset_name.unwrap_or("Custom");
    for i in 0..model.n_items() {
        if model.string(i).is_some_and(|s| s == target) {
            dropdown.set_selected(i);
            return;
        }
    }
    let n = model.n_items();
    if n > 0 {
        dropdown.set_selected(n - 1);
    }
}

// ---------------------------------------------------------------------------
// Import from Sonar share URL
// ---------------------------------------------------------------------------

fn show_import_dialog(
    parent: Option<&gtk::Window>,
    state: SharedEqState,
    on_changed: Rc<dyn Fn()>,
    preset_dropdown: gtk::DropDown,
) {
    let dialog = adw::Window::builder()
        .title("Import from SteelSeries Sonar")
        .modal(true)
        .resizable(false)
        .default_width(480)
        .build();
    if let Some(p) = parent {
        dialog.set_transient_for(Some(p));
    }

    let root = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(0)
        .build();

    let header = adw::HeaderBar::new();
    root.append(&header);

    let body = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(12)
        .margin_top(16)
        .margin_bottom(16)
        .margin_start(16)
        .margin_end(16)
        .build();

    let hint = gtk::Label::builder()
        .label("Paste a Sonar share URL (steelseries.com/deeplink/...) or a community-configs CDN link.")
        .wrap(true)
        .xalign(0.0)
        .build();
    hint.add_css_class("dim-label");
    body.append(&hint);

    let entry = gtk::Entry::builder()
        .placeholder_text("https://www.steelseries.com/deeplink/gg/sonar/config/v1/import?url=...")
        .hexpand(true)
        .build();
    body.append(&entry);

    let status = gtk::Label::builder()
        .label("")
        .wrap(true)
        .xalign(0.0)
        .visible(false)
        .build();
    body.append(&status);

    let button_row = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(8)
        .halign(gtk::Align::End)
        .margin_top(8)
        .build();
    let cancel_btn = gtk::Button::with_label("Cancel");
    let import_btn = gtk::Button::with_label("Import");
    import_btn.add_css_class("suggested-action");
    button_row.append(&cancel_btn);
    button_row.append(&import_btn);
    body.append(&button_row);

    root.append(&body);
    dialog.set_content(Some(&root));

    {
        let dialog = dialog.clone();
        cancel_btn.connect_clicked(move |_| dialog.close());
    }

    {
        let dialog = dialog.clone();
        let entry = entry.clone();
        let status = status.clone();
        let import_btn_inner = import_btn.clone();
        let cancel_btn = cancel_btn.clone();
        let state = state.clone();
        let on_changed = on_changed.clone();
        let preset_dropdown = preset_dropdown.clone();
        import_btn.connect_clicked(move |_| {
            let url = entry.text().to_string();
            if url.trim().is_empty() {
                status.set_label("Please paste a URL.");
                status.set_visible(true);
                return;
            }

            entry.set_sensitive(false);
            import_btn_inner.set_sensitive(false);
            cancel_btn.set_sensitive(false);
            status.set_label("Fetching preset...");
            status.set_visible(true);

            let (tx, rx) = std::sync::mpsc::channel();
            std::thread::Builder::new()
                .name("sonar-import".into())
                .spawn(move || {
                    let _ = tx.send(sonar_import::import_from_url(&url));
                })
                .expect("spawn sonar-import thread");

            let dialog = dialog.clone();
            let entry = entry.clone();
            let import_btn = import_btn_inner.clone();
            let cancel_btn = cancel_btn.clone();
            let status = status.clone();
            let state = state.clone();
            let on_changed = on_changed.clone();
            let preset_dropdown = preset_dropdown.clone();
            glib::timeout_add_local(std::time::Duration::from_millis(100), move || {
                match rx.try_recv() {
                    Ok(result) => {
                        match result {
                            Ok(preset) => {
                                presets::save_custom_preset(&preset.name, &preset.bands);
                                {
                                    let mut st = state.borrow_mut();
                                    let sink_eq = st.active_sink_eq_mut();
                                    sink_eq.bands = preset.bands;
                                    sink_eq.preset_name = Some(preset.name.clone());
                                }
                                if let Some(model) = preset_dropdown
                                    .model()
                                    .and_downcast::<gtk::StringList>()
                                {
                                    refresh_preset_list(&model);
                                    update_preset_selection(&preset_dropdown, Some(&preset.name));
                                }
                                on_changed();
                                dialog.close();
                            }
                            Err(e) => {
                                status.set_label(&format!("Import failed: {e}"));
                                entry.set_sensitive(true);
                                import_btn.set_sensitive(true);
                                cancel_btn.set_sensitive(true);
                            }
                        }
                        glib::ControlFlow::Break
                    }
                    Err(std::sync::mpsc::TryRecvError::Empty) => glib::ControlFlow::Continue,
                    Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                        status.set_label("Import failed: worker disconnected");
                        entry.set_sensitive(true);
                        import_btn.set_sensitive(true);
                        cancel_btn.set_sensitive(true);
                        glib::ControlFlow::Break
                    }
                }
            });
        });
    }

    dialog.present();
}
