mod biquad;
mod controls;
mod graph;
pub mod model;
pub mod presets;
pub mod sonar_import;
pub mod spatial;

use std::rc::Rc;

use adw::prelude::*;
use gtk::glib;

use model::*;

pub fn build_eq_page(
    on_eq_apply: Option<Rc<dyn Fn(EqTarget, [Band; NUM_BANDS])>>,
    on_spatial_apply: Option<Rc<dyn Fn(EqTarget, SpatialState)>>,
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
    let spatial_panel_ref: Rc<std::cell::RefCell<Option<gtk::Box>>> =
        Rc::new(std::cell::RefCell::new(None));
    let spatial_refresh_ref: Rc<std::cell::RefCell<Option<Rc<dyn Fn()>>>> =
        Rc::new(std::cell::RefCell::new(None));

    // Shared on_changed callback
    let on_changed: Rc<dyn Fn()> = {
        let graph_ref = graph_ref.clone();
        let controls_update_ref = controls_update_ref.clone();
        let preset_dropdown_ref = preset_dropdown_ref.clone();
        let preset_updating = preset_updating.clone();
        let spatial_refresh_ref = spatial_refresh_ref.clone();
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
            // Pull the spatial panel's controls in sync with the active sink's
            // spatial state (e.g. on tab switch).
            if let Some(ref refresh) = *spatial_refresh_ref.borrow() {
                refresh();
            }
            presets::save_eq_state(&state.borrow().sinks);

            // Notify the audio pipeline (debounced by the caller)
            if let Some(ref apply) = on_eq_apply {
                let s = state.borrow();
                apply(s.active_target, s.active_sink_eq().bands);
            }
        })
    };

    // Dedicated callback for spatial-only changes: save state + notify router
    // via on_spatial_apply. Does NOT go through on_eq_apply (that would
    // needlessly rebuild the EQ filter-chain on every slider drag).
    let on_spatial_changed: Rc<dyn Fn()> = {
        let state = state.clone();
        let on_spatial_apply = on_spatial_apply.clone();
        Rc::new(move || {
            presets::save_eq_state(&state.borrow().sinks);
            if let Some(ref apply) = on_spatial_apply {
                let s = state.borrow();
                apply(s.active_target, s.active_sink_eq().spatial);
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
            let spatial_panel_ref = spatial_panel_ref.clone();
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
                    if let Some(ref panel) = *spatial_panel_ref.borrow() {
                        panel.set_visible(target.supports_spatial());
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
        let spatial_panel_ref = spatial_panel_ref.clone();
        mixer_btn.connect_toggled(move |b| {
            if b.is_active() {
                if let Some(ref stack) = *content_stack_ref.borrow() {
                    stack.set_visible_child_name("mixer");
                }
                if let Some(ref controls) = *eq_controls_ref.borrow() {
                    controls.set_visible(false);
                }
                if let Some(ref panel) = *spatial_panel_ref.borrow() {
                    panel.set_visible(false);
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

    // Preset dropdown
    let preset_dropdown = build_preset_dropdown(state.clone(), on_changed.clone(), preset_updating.clone());
    eq_controls_box.append(&preset_dropdown);
    *preset_dropdown_ref.borrow_mut() = Some(preset_dropdown.clone());

    // Edit button — opens popup with name + Reset + Delete
    let edit_btn = gtk::Button::builder()
        .icon_name("lucide-pencil-symbolic")
        .tooltip_text("Edit the current preset (rename, reset, delete)")
        .build();
    {
        let state = state.clone();
        let on_changed = on_changed.clone();
        let preset_dropdown = preset_dropdown.clone();
        edit_btn.connect_clicked(move |btn| {
            let window = btn
                .root()
                .and_then(|r| r.downcast::<gtk::Window>().ok());
            show_edit_dialog(window.as_ref(), state.clone(), on_changed.clone(), preset_dropdown.clone());
        });
    }
    eq_controls_box.append(&edit_btn);

    // Import from Sonar button — paste a share URL to pull a Sonar preset
    let import_btn = gtk::Button::builder()
        .label("Import")
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
            show_import_dialog(
                window.as_ref(),
                state.clone(),
                on_changed.clone(),
                preset_dropdown.clone(),
            );
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

    // Spatial audio panel — hidden on Chat/Mic tabs
    let (spatial_panel, spatial_refresh) =
        build_spatial_panel(state.clone(), on_spatial_changed.clone());
    spatial_panel.set_margin_top(6);
    {
        let s = state.borrow();
        spatial_panel.set_visible(s.active_target.supports_spatial());
    }
    eq_view.append(&spatial_panel);
    *spatial_panel_ref.borrow_mut() = Some(spatial_panel);
    *spatial_refresh_ref.borrow_mut() = Some(spatial_refresh);

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
}

fn update_preset_selection(dropdown: &gtk::DropDown, preset_name: Option<&str>) {
    let model = dropdown
        .model()
        .and_downcast::<gtk::StringList>()
        .unwrap();
    let target = preset_name.unwrap_or("Flat");
    for i in 0..model.n_items() {
        if model.string(i).is_some_and(|s| s == target) {
            dropdown.set_selected(i);
            return;
        }
    }
    if model.n_items() > 0 {
        dropdown.set_selected(0);
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

    let url_entry = gtk::Entry::builder()
        .placeholder_text("https://www.steelseries.com/deeplink/gg/sonar/config/v1/import?url=...")
        .hexpand(true)
        .build();
    body.append(&url_entry);

    let name_label = gtk::Label::builder()
        .label("Preset name")
        .xalign(0.0)
        .margin_top(4)
        .build();
    name_label.add_css_class("dim-label");
    name_label.add_css_class("caption");
    body.append(&name_label);

    let name_entry = gtk::Entry::builder()
        .placeholder_text("Name will appear here after fetching")
        .hexpand(true)
        .sensitive(false)
        .build();
    body.append(&name_entry);

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
    let action_btn = gtk::Button::with_label("Fetch");
    action_btn.add_css_class("suggested-action");
    button_row.append(&cancel_btn);
    button_row.append(&action_btn);
    body.append(&button_row);

    root.append(&body);
    dialog.set_content(Some(&root));

    // Two-phase state: once we've fetched, hold the bands here so Save can
    // commit them with whatever name the user typed.
    let fetched: Rc<std::cell::RefCell<Option<sonar_import::ImportedPreset>>> =
        Rc::new(std::cell::RefCell::new(None));

    {
        let dialog = dialog.clone();
        cancel_btn.connect_clicked(move |_| dialog.close());
    }

    {
        let dialog = dialog.clone();
        let url_entry = url_entry.clone();
        let name_entry = name_entry.clone();
        let status = status.clone();
        let action_btn_inner = action_btn.clone();
        let cancel_btn = cancel_btn.clone();
        let state = state.clone();
        let on_changed = on_changed.clone();
        let preset_dropdown = preset_dropdown.clone();
        let fetched = fetched.clone();
        action_btn.connect_clicked(move |_| {
            // Phase 2: preset already fetched — commit with edited name
            if let Some(preset) = fetched.borrow_mut().take() {
                let name = name_entry.text().trim().to_string();
                if name.is_empty() {
                    status.set_label("Preset name cannot be empty.");
                    status.set_visible(true);
                    *fetched.borrow_mut() = Some(preset);
                    return;
                }
                let built_ins: Vec<&str> = presets::built_in_names();
                if built_ins.contains(&name.as_str()) {
                    status.set_label(&format!("'{name}' is a built-in preset name. Pick another."));
                    status.set_visible(true);
                    *fetched.borrow_mut() = Some(preset);
                    return;
                }
                presets::save_custom_preset(&name, &preset.bands);
                {
                    let mut st = state.borrow_mut();
                    let sink_eq = st.active_sink_eq_mut();
                    sink_eq.bands = preset.bands;
                    sink_eq.preset_name = Some(name.clone());
                }
                if let Some(model) = preset_dropdown
                    .model()
                    .and_downcast::<gtk::StringList>()
                {
                    refresh_preset_list(&model);
                    update_preset_selection(&preset_dropdown, Some(&name));
                }
                on_changed();
                dialog.close();
                return;
            }

            // Phase 1: fetch the preset
            let url = url_entry.text().to_string();
            if url.trim().is_empty() {
                status.set_label("Please paste a URL.");
                status.set_visible(true);
                return;
            }

            url_entry.set_sensitive(false);
            action_btn_inner.set_sensitive(false);
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

            let url_entry = url_entry.clone();
            let name_entry = name_entry.clone();
            let action_btn = action_btn_inner.clone();
            let cancel_btn = cancel_btn.clone();
            let status = status.clone();
            let fetched = fetched.clone();
            glib::timeout_add_local(std::time::Duration::from_millis(100), move || {
                match rx.try_recv() {
                    Ok(result) => {
                        match result {
                            Ok(preset) => {
                                name_entry.set_sensitive(true);
                                name_entry.set_text(&preset.name);
                                name_entry.select_region(0, -1);
                                name_entry.grab_focus();
                                status.set_label("Fetched. Edit the name if you want, then click Save.");
                                status.set_visible(true);
                                action_btn.set_label("Save");
                                action_btn.set_sensitive(true);
                                cancel_btn.set_sensitive(true);
                                *fetched.borrow_mut() = Some(preset);
                            }
                            Err(e) => {
                                status.set_label(&format!("Import failed: {e}"));
                                url_entry.set_sensitive(true);
                                action_btn.set_sensitive(true);
                                cancel_btn.set_sensitive(true);
                            }
                        }
                        glib::ControlFlow::Break
                    }
                    Err(std::sync::mpsc::TryRecvError::Empty) => glib::ControlFlow::Continue,
                    Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                        status.set_label("Import failed: worker disconnected");
                        url_entry.set_sensitive(true);
                        action_btn.set_sensitive(true);
                        cancel_btn.set_sensitive(true);
                        glib::ControlFlow::Break
                    }
                }
            });
        });
    }

    dialog.present();
}

// ---------------------------------------------------------------------------
// Edit preset dialog — rename, reset, delete
// ---------------------------------------------------------------------------

fn show_edit_dialog(
    parent: Option<&gtk::Window>,
    state: SharedEqState,
    on_changed: Rc<dyn Fn()>,
    preset_dropdown: gtk::DropDown,
) {
    let original_name = state
        .borrow()
        .active_sink_eq()
        .preset_name
        .clone()
        .unwrap_or_else(|| "Flat".to_string());
    let is_built_in = presets::built_in_names().contains(&original_name.as_str());

    let dialog = adw::Window::builder()
        .title("Edit preset")
        .modal(true)
        .resizable(false)
        .default_width(420)
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

    let name_label = gtk::Label::builder()
        .label("Preset name")
        .xalign(0.0)
        .build();
    name_label.add_css_class("dim-label");
    name_label.add_css_class("caption");
    body.append(&name_label);

    let name_entry = gtk::Entry::builder()
        .text(&original_name)
        .hexpand(true)
        .sensitive(!is_built_in)
        .tooltip_text(if is_built_in {
            "Built-in presets can't be renamed"
        } else {
            "Rename this preset"
        })
        .build();
    if !is_built_in {
        name_entry.select_region(0, -1);
    }
    body.append(&name_entry);

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
    let reset_btn = gtk::Button::with_label("Reset");
    let delete_btn = gtk::Button::with_label("Delete");
    delete_btn.add_css_class("destructive-action");
    delete_btn.set_sensitive(!is_built_in);
    if is_built_in {
        delete_btn.set_tooltip_text(Some("Built-in presets can't be deleted"));
    }
    let save_btn = gtk::Button::with_label("Save");
    save_btn.add_css_class("suggested-action");
    save_btn.set_sensitive(!is_built_in);
    if is_built_in {
        save_btn.set_tooltip_text(Some("Built-in presets can't be renamed"));
    }
    button_row.append(&cancel_btn);
    button_row.append(&reset_btn);
    button_row.append(&delete_btn);
    button_row.append(&save_btn);
    body.append(&button_row);

    root.append(&body);
    dialog.set_content(Some(&root));

    // Shared: commit any pending rename (from name_entry) before another action.
    // Returns the name we should use going forward. Writes to state + dropdown.
    let commit_rename = {
        let state = state.clone();
        let name_entry = name_entry.clone();
        let preset_dropdown = preset_dropdown.clone();
        let status = status.clone();
        let original_name = original_name.clone();
        Rc::new(move || -> Result<String, ()> {
            if is_built_in {
                return Ok(original_name.clone());
            }
            let new_name = name_entry.text().trim().to_string();
            if new_name.is_empty() {
                status.set_label("Name cannot be empty.");
                status.set_visible(true);
                return Err(());
            }
            if new_name == original_name {
                return Ok(original_name.clone());
            }
            if presets::built_in_names().contains(&new_name.as_str()) {
                status.set_label(&format!("'{new_name}' is a built-in preset name."));
                status.set_visible(true);
                return Err(());
            }
            if presets::list_custom_presets().iter().any(|n| n == &new_name) {
                status.set_label(&format!("A custom preset named '{new_name}' already exists."));
                status.set_visible(true);
                return Err(());
            }
            let bands = state.borrow().active_sink_eq().bands;
            presets::save_custom_preset(&new_name, &bands);
            presets::delete_custom_preset(&original_name);
            state.borrow_mut().active_sink_eq_mut().preset_name = Some(new_name.clone());
            if let Some(model) = preset_dropdown.model().and_downcast::<gtk::StringList>() {
                refresh_preset_list(&model);
                update_preset_selection(&preset_dropdown, Some(&new_name));
            }
            Ok(new_name)
        })
    };

    {
        let dialog = dialog.clone();
        cancel_btn.connect_clicked(move |_| {
            // Cancel discards any unsaved name change — explicit Save commits.
            dialog.close();
        });
    }

    // Save: commits the rename if non-empty/valid; on failure stays open with
    // the status label showing the error.
    let save_action: Rc<dyn Fn()> = {
        let dialog = dialog.clone();
        let commit_rename = commit_rename.clone();
        let on_changed = on_changed.clone();
        Rc::new(move || {
            if commit_rename().is_ok() {
                on_changed();
                dialog.close();
            }
        })
    };
    {
        let save_action = save_action.clone();
        save_btn.connect_clicked(move |_| save_action());
    }
    // Pressing Enter in the name entry triggers Save too.
    {
        let save_action = save_action.clone();
        name_entry.connect_activate(move |_| save_action());
    }

    {
        let dialog = dialog.clone();
        let state = state.clone();
        let on_changed = on_changed.clone();
        let commit_rename = commit_rename.clone();
        reset_btn.connect_clicked(move |_| {
            let Ok(current_name) = commit_rename() else { return };
            // Reset bands to the preset's original values
            let bands = if let Some(b) = presets::load_built_in(&current_name) {
                Some(b)
            } else {
                presets::load_custom_preset(&current_name)
            };
            if let Some(bands) = bands {
                let mut st = state.borrow_mut();
                let sink_eq = st.active_sink_eq_mut();
                sink_eq.bands = bands;
                sink_eq.preset_name = Some(current_name);
                st.selected_band = None;
                drop(st);
                on_changed();
            }
            dialog.close();
        });
    }

    {
        let dialog = dialog.clone();
        let state = state.clone();
        let on_changed = on_changed.clone();
        let preset_dropdown = preset_dropdown.clone();
        let original_name = original_name.clone();
        delete_btn.connect_clicked(move |_| {
            if is_built_in {
                return;
            }
            presets::delete_custom_preset(&original_name);
            // Active sink falls back to Flat
            {
                let mut st = state.borrow_mut();
                let sink_eq = st.active_sink_eq_mut();
                sink_eq.bands = model::default_bands();
                sink_eq.preset_name = Some("Flat".to_string());
                st.selected_band = None;
            }
            if let Some(model) = preset_dropdown.model().and_downcast::<gtk::StringList>() {
                refresh_preset_list(&model);
                update_preset_selection(&preset_dropdown, Some("Flat"));
            }
            on_changed();
            dialog.close();
        });
    }

    dialog.present();
}

// ---------------------------------------------------------------------------
// Spatial audio panel — Perf↔Immersion + Distance sliders + 2D speaker stage
// ---------------------------------------------------------------------------

fn build_spatial_panel(
    state: SharedEqState,
    on_spatial_changed: Rc<dyn Fn()>,
) -> (gtk::Box, Rc<dyn Fn()>) {
    // Suppress change callbacks while we're programmatically refreshing
    // controls on tab-change.
    let updating: Rc<std::cell::Cell<bool>> = Rc::new(std::cell::Cell::new(false));

    let panel = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(12)
        .build();
    panel.add_css_class("eq-floating-panel");
    panel.add_css_class("spatial-panel");

    let title = gtk::Label::builder()
        .label("Spatial Audio")
        .xalign(0.0)
        .build();
    title.add_css_class("heading");

    // Profile dropdown — one entry per SpatialProfile.
    let profile_model = gtk::StringList::new(&[]);
    for &p in SpatialProfile::ALL {
        profile_model.append(p.display_label());
    }
    let profile_dropdown = gtk::DropDown::builder()
        .model(&profile_model)
        .tooltip_text("Pick the HRTF 'virtual room' flavor")
        .build();

    // Dry/Wet mix slider. Lets the user dial back HeSuVi's room reverb
    // without disabling spatial entirely. Labels use default text color
    // (no dim-label / caption) so they match the dropdown's typography.
    let mix_label = gtk::Label::builder().label("Dry").build();
    let mix_label_wet = gtk::Label::builder().label("Wet").build();
    let mix_scale = gtk::Scale::builder()
        .orientation(gtk::Orientation::Horizontal)
        .draw_value(false)
        .width_request(180)
        .hexpand(false)
        .tooltip_text("Blend between unprocessed stereo (Dry) and full HeSuVi processing (Wet)")
        .build();
    mix_scale.set_range(WET_MIN, WET_MAX);
    mix_scale.set_increments(0.02, 0.10);

    // Wrap [Dry, slider, Wet] in a rounded pill matching other inline boxes
    // in the app (border-radius + subtle background tint).
    let mix_pill = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(8)
        .valign(gtk::Align::Center)
        .build();
    mix_pill.add_css_class("spatial-mix-pill");
    mix_pill.append(&mix_label);
    mix_pill.append(&mix_scale);
    mix_pill.append(&mix_label_wet);

    // Spacer that consumes the remaining horizontal slack so the enable
    // switch sits flush against the right edge while the pill stays left.
    let spacer = gtk::Box::builder().hexpand(true).build();

    let enable_switch = gtk::Switch::builder()
        .valign(gtk::Align::Center)
        .tooltip_text("Toggle HRTF spatial audio")
        .build();
    // If none of the bundled HRIRs can be extracted to cache, spatial is a
    // no-op at the audio layer — grey out the controls so the UI doesn't lie.
    if !spatial::hrtf_available() {
        enable_switch.set_sensitive(false);
        profile_dropdown.set_sensitive(false);
        mix_scale.set_sensitive(false);
        enable_switch.set_tooltip_text(Some(
            "Spatial audio unavailable: HRIR files could not be extracted to cache",
        ));
    }

    panel.append(&title);
    panel.append(&profile_dropdown);
    panel.append(&mix_pill);
    panel.append(&spacer);
    panel.append(&enable_switch);

    // Initial sync from state
    let refresh: Rc<dyn Fn()> = {
        let state = state.clone();
        let enable_switch = enable_switch.clone();
        let profile_dropdown = profile_dropdown.clone();
        let mix_scale = mix_scale.clone();
        let updating = updating.clone();
        Rc::new(move || {
            let sp = state.borrow().active_sink_eq().spatial;
            updating.set(true);
            enable_switch.set_active(sp.enabled);
            let idx = SpatialProfile::ALL
                .iter()
                .position(|p| *p == sp.profile)
                .unwrap_or(0) as u32;
            profile_dropdown.set_selected(idx);
            mix_scale.set_value(sp.wet_mix);
            updating.set(false);
        })
    };
    refresh();

    // Wire enable switch
    {
        let state = state.clone();
        let on_spatial_changed = on_spatial_changed.clone();
        let updating = updating.clone();
        enable_switch.connect_state_set(move |_, active| {
            if !updating.get() {
                state.borrow_mut().active_sink_eq_mut().spatial.enabled = active;
                on_spatial_changed();
            }
            glib::Propagation::Proceed
        });
    }

    // Wire profile dropdown
    {
        let state = state.clone();
        let on_spatial_changed = on_spatial_changed.clone();
        let updating = updating.clone();
        profile_dropdown.connect_selected_notify(move |dd| {
            let idx = dd.selected() as usize;
            let Some(&profile) = SpatialProfile::ALL.get(idx) else { return };
            if !updating.get() {
                state.borrow_mut().active_sink_eq_mut().spatial.profile = profile;
                on_spatial_changed();
            }
        });
    }

    // Wire dry/wet slider
    {
        let state = state.clone();
        let on_spatial_changed = on_spatial_changed.clone();
        let updating = updating.clone();
        mix_scale.connect_value_changed(move |s| {
            if updating.get() {
                return;
            }
            state.borrow_mut().active_sink_eq_mut().spatial.wet_mix = s.value();
            on_spatial_changed();
        });
    }

    (panel, refresh)
}
