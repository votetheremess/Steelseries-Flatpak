use std::cell::Cell;
use std::rc::Rc;

use adw::prelude::*;
use gtk::glib;

use super::model::*;

/// Build a compact controls bar: [Freq: 125 Hz] [Gain: +3.0 dB] [Q: 1.00] [Type ▾] [On/Off]
/// Each pill has a dim label prefix and a content-hugging entry.
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

    // Frequency
    let freq_entry = gtk::Entry::builder()
        .tooltip_text("Frequency — press Enter to apply")
        .xalign(0.5)
        .hexpand(false)
        .build();
    freq_entry.add_css_class("eq-spin");

    freq_entry.connect_activate({
        let state = state.clone();
        let on_changed = on_changed.clone();
        let updating = updating.clone();
        move |entry| {
            if updating.get() { return; }
            let text = entry.text();
            let text = text.trim();
            let val = if let Some(khz) = text.strip_suffix("kHz").or_else(|| text.strip_suffix("khz")) {
                khz.trim().parse::<f64>().map(|v| v * 1000.0).ok()
            } else if let Some(hz) = text.strip_suffix("Hz").or_else(|| text.strip_suffix("hz")) {
                hz.trim().parse::<f64>().ok()
            } else {
                text.parse::<f64>().ok()
            };
            if let Some(v) = val {
                let mut st = state.borrow_mut();
                if let Some(band) = st.selected_band_mut() {
                    band.frequency = v.clamp(FREQ_MIN, FREQ_MAX);
                    st.active_sink_eq_mut().preset_name = None;
                }
                drop(st);
                on_changed();
            }
        }
    });

    // Gain
    let gain_entry = gtk::Entry::builder()
        .tooltip_text("Gain — press Enter to apply")
        .xalign(0.5)
        .hexpand(false)
        .build();
    gain_entry.add_css_class("eq-spin");

    gain_entry.connect_activate({
        let state = state.clone();
        let on_changed = on_changed.clone();
        let updating = updating.clone();
        move |entry| {
            if updating.get() { return; }
            let text = entry.text();
            let text = text.trim().trim_end_matches("dB").trim_end_matches("db").trim();
            if let Ok(v) = text.parse::<f64>() {
                let mut st = state.borrow_mut();
                if let Some(band) = st.selected_band_mut() {
                    band.gain_db = v.clamp(GAIN_MIN, GAIN_MAX);
                    st.active_sink_eq_mut().preset_name = None;
                }
                drop(st);
                on_changed();
            }
        }
    });

    // Q
    let q_entry = gtk::Entry::builder()
        .tooltip_text("Q factor — press Enter to apply")
        .xalign(0.5)
        .hexpand(false)
        .build();
    q_entry.add_css_class("eq-spin");

    q_entry.connect_activate({
        let state = state.clone();
        let on_changed = on_changed.clone();
        let updating = updating.clone();
        move |entry| {
            if updating.get() { return; }
            let text = entry.text();
            if let Ok(v) = text.trim().parse::<f64>() {
                let mut st = state.borrow_mut();
                if let Some(band) = st.selected_band_mut() {
                    band.q = v.clamp(Q_MIN, Q_MAX);
                    st.active_sink_eq_mut().preset_name = None;
                }
                drop(st);
                on_changed();
            }
        }
    });

    // Filter type dropdown
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

    // Enable switch
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

    // Assemble: labeled pills
    panel.append(&labeled_pill("Freq", &freq_entry));
    panel.append(&labeled_pill("Gain", &gain_entry));
    panel.append(&labeled_pill("Q", &q_entry));
    panel.append(&labeled_pill("Type", &filter_dropdown));
    panel.append(&labeled_pill("On", &enable_switch));

    // Update function — populates when a band is selected, clears when none
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

    // Start in empty state
    update_fn();

    (panel, update_fn)
}

fn labeled_pill(label: &str, widget: &impl IsA<gtk::Widget>) -> gtk::Box {
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
    pill
}
