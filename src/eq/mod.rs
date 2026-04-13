mod biquad;
mod controls;
mod graph;
pub mod model;
mod presets;

use std::rc::Rc;

use adw::prelude::*;
use gtk::glib;

use model::*;

pub fn build_eq_page() -> gtk::Widget {
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

    // We need forward declarations for the callbacks that cross-reference widgets
    let graph_ref: Rc<std::cell::RefCell<Option<graph::EqGraph>>> =
        Rc::new(std::cell::RefCell::new(None));
    let controls_update_ref: Rc<std::cell::RefCell<Option<Rc<dyn Fn()>>>> =
        Rc::new(std::cell::RefCell::new(None));
    let preset_dropdown_ref: Rc<std::cell::RefCell<Option<gtk::DropDown>>> =
        Rc::new(std::cell::RefCell::new(None));
    let preset_updating: Rc<std::cell::Cell<bool>> =
        Rc::new(std::cell::Cell::new(false));

    // Shared on_changed callback: redraws graph, updates controls, saves state
    let on_changed: Rc<dyn Fn()> = {
        let graph_ref = graph_ref.clone();
        let controls_update_ref = controls_update_ref.clone();
        let preset_dropdown_ref = preset_dropdown_ref.clone();
        let preset_updating = preset_updating.clone();
        let state = state.clone();
        Rc::new(move || {
            if let Some(ref g) = *graph_ref.borrow() {
                g.queue_draw();
            }
            if let Some(ref update) = *controls_update_ref.borrow() {
                update();
            }
            // Update preset dropdown — must guard with preset_updating to prevent
            // the dropdown's selected_notify handler from re-borrowing state.
            // Extract preset name while borrowing, then drop the borrow before
            // calling set_selected (which fires signals synchronously).
            if !preset_updating.get() {
                let preset_name = state.borrow().active_sink_eq().preset_name.clone();
                if let Some(ref dd) = *preset_dropdown_ref.borrow() {
                    preset_updating.set(true);
                    update_preset_selection(dd, preset_name.as_deref());
                    preset_updating.set(false);
                }
            }
            // Persist state
            presets::save_eq_state(&state.borrow().sinks);
        })
    };

    // -----------------------------------------------------------------------
    // Top bar: sink tabs + preset dropdown
    // -----------------------------------------------------------------------
    let top_bar = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(8)
        .margin_bottom(8)
        .build();

    // Sink tab buttons
    let tab_box = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(0)
        .build();
    tab_box.add_css_class("linked");

    let mut first_btn: Option<gtk::ToggleButton> = None;
    for &target in EqTarget::ALL {
        let btn = gtk::ToggleButton::builder()
            .label(target.label())
            .build();

        if let Some(ref first) = first_btn {
            btn.set_group(Some(first));
        } else {
            btn.set_active(true);
            first_btn = Some(btn.clone());
        }

        {
            let state = state.clone();
            let on_changed = on_changed.clone();
            btn.connect_toggled(move |b| {
                if b.is_active() {
                    state.borrow_mut().active_target = target;
                    on_changed();
                }
            });
        }

        tab_box.append(&btn);
    }

    top_bar.append(&tab_box);

    // Spacer
    let spacer = gtk::Box::builder().hexpand(true).build();
    top_bar.append(&spacer);

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
    top_bar.append(&show_all_box);

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
            st.selected_band = 0;
            drop(st);
            on_changed();
        });
    }
    top_bar.append(&reset_btn);

    // Preset dropdown
    let preset_dropdown = build_preset_dropdown(state.clone(), on_changed.clone(), preset_updating.clone());
    top_bar.append(&preset_dropdown);

    *preset_dropdown_ref.borrow_mut() = Some(preset_dropdown);

    page.append(&top_bar);

    // -----------------------------------------------------------------------
    // EQ Graph
    // -----------------------------------------------------------------------
    let eq_graph = graph::EqGraph::new(state.clone(), on_changed.clone());
    let frame = gtk::Frame::builder()
        .child(&eq_graph.drawing_area)
        .build();
    frame.add_css_class("eq-graph-frame");
    page.append(&frame);

    // Wire "Show All" switch to the graph
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

    // -----------------------------------------------------------------------
    // Detail controls
    // -----------------------------------------------------------------------
    let (controls_widget, controls_update) = controls::build_controls(state.clone(), on_changed.clone());

    let clamp = adw::Clamp::builder()
        .maximum_size(950)
        .child(&controls_widget)
        .margin_top(4)
        .build();
    page.append(&clamp);

    *controls_update_ref.borrow_mut() = Some(controls_update);

    page.upcast()
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

    // Populate: built-in presets + "Custom"
    refresh_preset_list(&model);

    let dropdown = gtk::DropDown::builder()
        .model(&model)
        .tooltip_text("EQ Preset")
        .build();

    // Set initial selection
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

            if name == "Custom" {
                return; // Don't do anything when "Custom" is re-selected
            }

            // Try loading as built-in preset
            if let Some(bands) = presets::load_built_in(&name) {
                let mut st = state.borrow_mut();
                let sink_eq = st.active_sink_eq_mut();
                sink_eq.bands = bands;
                sink_eq.preset_name = Some(name.clone());
                drop(st);
                on_changed();
                return;
            }

            // Try loading as custom preset
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
    // Clear
    while model.n_items() > 0 {
        model.remove(0);
    }

    // Built-in presets
    for name in presets::built_in_names() {
        model.append(name);
    }

    // Custom presets
    let custom = presets::list_custom_presets();
    if !custom.is_empty() {
        for name in &custom {
            model.append(name);
        }
    }

    // "Custom" entry (for when user modifies bands manually)
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
    // If not found, select "Custom"
    let n = model.n_items();
    if n > 0 {
        dropdown.set_selected(n - 1);
    }
}
