use std::cell::Cell;
use std::rc::Rc;

use adw::prelude::*;
use gtk::glib;
use gtk::gdk;

use super::model::*;

/// Build the controls bar with Blender-style scrub entries.
/// Hover shows a left-right cursor. Drag to adjust. Click to type.
pub fn build_floating_panel(
    state: SharedEqState,
    on_changed: Rc<dyn Fn()>,
) -> (gtk::Box, Rc<dyn Fn()>) {
    let panel = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(6)
        .halign(gtk::Align::Center)
        .valign(gtk::Align::Center)
        .build();
    panel.add_css_class("eq-floating-panel");

    let updating = Rc::new(Cell::new(false));

    // --- Scrub entries ---

    let freq_entry = scrub_entry();
    let gain_entry = scrub_entry();
    let q_entry = scrub_entry();

    // Freq: logarithmic drag — each pixel multiplies by ~1.005
    wire_scrub_drag(&freq_entry, state.clone(), on_changed.clone(), updating.clone(), {
        let state = state.clone();
        move |dx| {
            let mut st = state.borrow_mut();
            if let Some(band) = st.selected_band_mut() {
                band.frequency = (band.frequency * 1.005_f64.powf(dx)).clamp(FREQ_MIN, FREQ_MAX);
                st.active_sink_eq_mut().preset_name = None;
            }
        }
    });

    // Gain: linear drag — 0.05 dB per pixel
    wire_scrub_drag(&gain_entry, state.clone(), on_changed.clone(), updating.clone(), {
        let state = state.clone();
        move |dx| {
            let mut st = state.borrow_mut();
            if let Some(band) = st.selected_band_mut() {
                band.gain_db = (band.gain_db + dx * 0.05).clamp(GAIN_MIN, GAIN_MAX);
                st.active_sink_eq_mut().preset_name = None;
            }
        }
    });

    // Q: logarithmic drag
    wire_scrub_drag(&q_entry, state.clone(), on_changed.clone(), updating.clone(), {
        let state = state.clone();
        move |dx| {
            let mut st = state.borrow_mut();
            if let Some(band) = st.selected_band_mut() {
                band.q = (band.q * 1.01_f64.powf(dx)).clamp(Q_MIN, Q_MAX);
                st.active_sink_eq_mut().preset_name = None;
            }
        }
    });

    // Wire Enter key to commit typed values
    wire_entry_commit(&freq_entry, state.clone(), on_changed.clone(), updating.clone(),
        |text| {
            let text = text.trim();
            if let Some(khz) = text.strip_suffix("kHz").or_else(|| text.strip_suffix("khz")) {
                return khz.trim().parse::<f64>().map(|v| v * 1000.0).ok();
            }
            if let Some(hz) = text.strip_suffix("Hz").or_else(|| text.strip_suffix("hz")) {
                return hz.trim().parse::<f64>().ok();
            }
            text.parse::<f64>().ok()
        },
        {
            let state = state.clone();
            move |val| {
                let mut st = state.borrow_mut();
                if let Some(band) = st.selected_band_mut() {
                    band.frequency = val.clamp(FREQ_MIN, FREQ_MAX);
                    st.active_sink_eq_mut().preset_name = None;
                }
            }
        },
    );

    wire_entry_commit(&gain_entry, state.clone(), on_changed.clone(), updating.clone(),
        |text| text.trim().trim_end_matches("dB").trim_end_matches("db").trim().parse::<f64>().ok(),
        {
            let state = state.clone();
            move |val| {
                let mut st = state.borrow_mut();
                if let Some(band) = st.selected_band_mut() {
                    band.gain_db = val.clamp(GAIN_MIN, GAIN_MAX);
                    st.active_sink_eq_mut().preset_name = None;
                }
            }
        },
    );

    wire_entry_commit(&q_entry, state.clone(), on_changed.clone(), updating.clone(),
        |text| text.trim().parse::<f64>().ok(),
        {
            let state = state.clone();
            move |val| {
                let mut st = state.borrow_mut();
                if let Some(band) = st.selected_band_mut() {
                    band.q = val.clamp(Q_MIN, Q_MAX);
                    st.active_sink_eq_mut().preset_name = None;
                }
            }
        },
    );

    // --- Filter type dropdown ---
    let filter_labels: Vec<&str> = FilterType::ALL.iter().map(|ft| ft.label()).collect();
    let filter_model = gtk::StringList::new(&filter_labels);
    let filter_dropdown = gtk::DropDown::builder()
        .model(&filter_model)
        .tooltip_text("Filter Type")
        .build();
    filter_dropdown.add_css_class("eq-filter-dropdown");

    filter_dropdown.connect_selected_notify({
        let state = state.clone();
        let on_changed = on_changed.clone();
        let updating = updating.clone();
        let gain_entry_ref = gain_entry.clone();
        move |dd| {
            if updating.get() { return; }
            let idx = dd.selected() as usize;
            if idx < FilterType::ALL.len() {
                let ft = FilterType::ALL[idx];
                let mut st = state.borrow_mut();
                if let Some(band) = st.selected_band_mut() {
                    band.filter_type = ft;
                    gain_entry_ref.set_sensitive(ft.uses_gain());
                    st.active_sink_eq_mut().preset_name = None;
                }
                drop(st);
                on_changed();
            }
        }
    });

    // --- Enable switch ---
    let enable_switch = gtk::Switch::builder()
        .tooltip_text("Enable/disable band")
        .valign(gtk::Align::Center)
        .build();
    enable_switch.add_css_class("eq-enable-switch");

    enable_switch.connect_state_set({
        let state = state.clone();
        let on_changed = on_changed.clone();
        let updating = updating.clone();
        move |_switch, active| {
            if updating.get() { return glib::Propagation::Proceed; }
            let mut st = state.borrow_mut();
            if let Some(band) = st.selected_band_mut() {
                band.enabled = active;
                st.active_sink_eq_mut().preset_name = None;
            }
            drop(st);
            on_changed();
            glib::Propagation::Proceed
        }
    });

    // Assemble labeled pills
    panel.append(&labeled_pill("Freq", &freq_entry, true));
    panel.append(&labeled_pill("Gain", &gain_entry, true));
    panel.append(&labeled_pill("Q", &q_entry, true));
    panel.append(&labeled_pill("Type", &filter_dropdown, false));
    panel.append(&labeled_pill("On", &enable_switch, false));

    // --- Update function ---
    let update_fn: Rc<dyn Fn()> = {
        let state = state.clone();
        let updating = updating.clone();
        Rc::new(move || {
            updating.set(true);

            let st = state.borrow();
            let selected = st.selected_band;

            if let Some(idx) = selected {
                let band = st.active_sink_eq().bands[idx].clone();
                drop(st);

                let freq_text = if band.frequency >= 1000.0 {
                    format!("{:.1} kHz", band.frequency / 1000.0)
                } else {
                    format!("{:.0} Hz", band.frequency)
                };
                freq_entry.set_text(&freq_text);
                freq_entry.set_width_chars(freq_text.len() as i32);
                freq_entry.set_sensitive(true);

                let gain_text = format!("{:+.1} dB", band.gain_db);
                gain_entry.set_text(&gain_text);
                gain_entry.set_width_chars(gain_text.len() as i32);
                gain_entry.set_sensitive(band.filter_type.uses_gain());

                let q_text = format!("{:.2}", band.q);
                q_entry.set_text(&q_text);
                q_entry.set_width_chars(q_text.len() as i32);
                q_entry.set_sensitive(true);

                filter_dropdown.set_sensitive(true);
                enable_switch.set_sensitive(true);
                enable_switch.set_active(band.enabled);

                let ft_idx = FilterType::ALL
                    .iter()
                    .position(|&ft| ft == band.filter_type)
                    .unwrap_or(0);
                filter_dropdown.set_selected(ft_idx as u32);
            } else {
                drop(st);

                freq_entry.set_text("");
                freq_entry.set_width_chars(4);
                freq_entry.set_sensitive(false);

                gain_entry.set_text("");
                gain_entry.set_width_chars(4);
                gain_entry.set_sensitive(false);

                q_entry.set_text("");
                q_entry.set_width_chars(3);
                q_entry.set_sensitive(false);

                filter_dropdown.set_sensitive(false);
                enable_switch.set_sensitive(false);
            }

            updating.set(false);
        })
    };

    update_fn();

    (panel, update_fn)
}

// ---------------------------------------------------------------------------
// Scrub entry: non-editable by default, drag to adjust, click to type
// ---------------------------------------------------------------------------

fn set_scrub_cursor(widget: &impl IsA<gtk::Widget>) {
    let cursor = gdk::Cursor::from_name("ew-resize", None);
    if let Some(ref c) = cursor {
        widget.set_cursor(Some(c));
        let mut child = widget.as_ref().first_child();
        while let Some(ch) = child {
            ch.set_cursor(Some(c));
            child = ch.next_sibling();
        }
    }
}

fn scrub_entry() -> gtk::Entry {
    let entry = gtk::Entry::builder()
        .xalign(0.5)
        .hexpand(false)
        .editable(false)
        .can_focus(false)
        .build();
    entry.add_css_class("eq-spin");

    // Remove GTK's built-in text DragSource. Our GestureDrag (Capture phase)
    // intercepts drags for scrub-to-adjust, but GTK's internal DnD controller
    // can still fire and crash with:
    //   gtk_text_util_create_drag_icon: assertion 'text != NULL' failed
    let controllers = entry.observe_controllers();
    for i in (0..controllers.n_items()).rev() {
        if let Some(obj) = controllers.item(i) {
            if obj.is::<gtk::DragSource>() {
                let ctrl = obj.downcast::<gtk::EventController>().unwrap();
                entry.remove_controller(&ctrl);
            }
        }
    }

    // Force scrub cursor on every mouse movement when not in edit mode.
    // GTK's Entry continuously resets its cursor internally, so we must
    // continuously override it.
    let motion = gtk::EventControllerMotion::new();
    {
        let entry = entry.clone();
        motion.connect_motion(move |_ctrl, _x, _y| {
            if !entry.is_editable() {
                set_scrub_cursor(&entry);
            }
        });
    }
    entry.add_controller(motion);

    entry
}

/// Wire drag-to-adjust on a scrub entry. `apply_delta` receives the horizontal pixel delta.
fn wire_scrub_drag(
    entry: &gtk::Entry,
    _state: SharedEqState,
    on_changed: Rc<dyn Fn()>,
    updating: Rc<Cell<bool>>,
    apply_delta: impl Fn(f64) + 'static,
) {
    let did_move = Rc::new(Cell::new(false));

    let drag = gtk::GestureDrag::new();
    // Capture phase: intercept events BEFORE the Entry's internal text handling
    drag.set_propagation_phase(gtk::PropagationPhase::Capture);

    {
        let did_move = did_move.clone();
        drag.connect_drag_begin(move |_gesture, _x, _y| {
            did_move.set(false);
        });
    }

    {
        let did_move = did_move.clone();
        let on_changed = on_changed.clone();
        let updating = updating.clone();
        let entry = entry.clone();
        let prev_x = Rc::new(Cell::new(0.0_f64));
        drag.connect_drag_update(move |_gesture, offset_x, _offset_y| {
            if updating.get() { return; }
            // Keep scrub cursor during drag
            set_scrub_cursor(&entry);
            let dx = offset_x - prev_x.get();
            prev_x.set(offset_x);
            if dx.abs() > 0.5 {
                did_move.set(true);
                apply_delta(dx);
                on_changed();
            }
        });
    }

    {
        let did_move = did_move.clone();
        let entry = entry.clone();
        drag.connect_drag_end(move |_gesture, _x, _y| {
            if !did_move.get() {
                // Click without drag → enter edit mode
                entry.set_can_focus(true);
                entry.set_editable(true);
                entry.grab_focus();
                entry.select_region(0, -1);
            }
        });
    }

    entry.add_controller(drag);
}

/// Wire Enter key to commit a typed value, then return to non-editable scrub mode.
fn wire_entry_commit(
    entry: &gtk::Entry,
    _state: SharedEqState,
    on_changed: Rc<dyn Fn()>,
    updating: Rc<Cell<bool>>,
    parse: impl Fn(&str) -> Option<f64> + 'static,
    apply: impl Fn(f64) + 'static,
) {
    let entry_ref = entry.clone();
    entry.connect_activate(move |entry| {
        if updating.get() { return; }
        let text = entry.text();
        if let Some(val) = parse(&text) {
            apply(val);
            on_changed();
        }
        // Return to scrub mode (notify::editable handler restores cursor automatically)
        entry_ref.set_editable(false);
        entry_ref.set_can_focus(false);
    });
}

fn labeled_pill(label: &str, widget: &impl IsA<gtk::Widget>, scrub: bool) -> gtk::Box {
    let pill = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(8)
        .valign(gtk::Align::Center)
        .build();
    let lbl = gtk::Label::new(Some(label));
    lbl.add_css_class("dim-label");
    lbl.add_css_class("caption");
    pill.append(&lbl);
    pill.append(widget);

    if scrub {
        let widget_clone = widget.as_ref().clone();
        let lbl_clone = lbl.clone();
        pill.connect_realize(move |pill| {
            set_scrub_cursor(pill);
            set_scrub_cursor(&lbl_clone);
            set_scrub_cursor(&widget_clone);
        });
    }

    pill
}
