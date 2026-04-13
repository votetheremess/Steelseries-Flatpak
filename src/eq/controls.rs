use std::cell::Cell;
use std::rc::Rc;

use adw::prelude::*;
use gtk::glib;

use super::model::*;

/// Build the detail control bar for the currently selected band.
/// Returns the container widget and an update function that refreshes all controls
/// from the current state.
pub fn build_controls(
    state: SharedEqState,
    on_changed: Rc<dyn Fn()>,
) -> (gtk::Widget, Rc<dyn Fn()>) {
    let container = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(12)
        .margin_top(8)
        .margin_bottom(8)
        .margin_start(12)
        .margin_end(12)
        .halign(gtk::Align::Center)
        .build();

    // Suppress feedback loops when we programmatically update controls
    let updating = Rc::new(Cell::new(false));

    // Band label (colored)
    let band_label = gtk::Label::builder()
        .width_chars(8)
        .xalign(0.5)
        .build();
    band_label.add_css_class("heading");

    // Frequency SpinButton
    let freq_adj = gtk::Adjustment::new(
        1000.0, FREQ_MIN, FREQ_MAX, 1.0, 100.0, 0.0,
    );
    let freq_spin = gtk::SpinButton::builder()
        .adjustment(&freq_adj)
        .digits(0)
        .width_chars(7)
        .tooltip_text("Frequency (Hz)")
        .build();
    freq_spin.add_css_class("eq-spin");

    // Custom output to format as "1.0 kHz" or "125 Hz"
    freq_spin.connect_output(|spin| {
        let val = spin.value();
        let text = if val >= 1000.0 {
            format!("{:.1} kHz", val / 1000.0)
        } else {
            format!("{:.0} Hz", val)
        };
        spin.set_text(&text);
        glib::Propagation::Stop
    });

    // Custom input to parse "1.0 kHz" or "125 Hz" back
    freq_spin.connect_input(|spin| {
        let text = spin.text();
        let text = text.trim();
        if let Some(khz) = text.strip_suffix("kHz").or_else(|| text.strip_suffix("khz")) {
            if let Ok(v) = khz.trim().parse::<f64>() {
                return Some(Ok(v * 1000.0));
            }
        }
        if let Some(hz) = text.strip_suffix("Hz").or_else(|| text.strip_suffix("hz")) {
            if let Ok(v) = hz.trim().parse::<f64>() {
                return Some(Ok(v));
            }
        }
        if let Ok(v) = text.parse::<f64>() {
            return Some(Ok(v));
        }
        None
    });

    // Gain SpinButton
    let gain_adj = gtk::Adjustment::new(0.0, GAIN_MIN, GAIN_MAX, 0.5, 1.0, 0.0);
    let gain_spin = gtk::SpinButton::builder()
        .adjustment(&gain_adj)
        .digits(1)
        .width_chars(6)
        .tooltip_text("Gain (dB)")
        .build();
    gain_spin.add_css_class("eq-spin");

    gain_spin.connect_output(|spin| {
        let val = spin.value();
        spin.set_text(&format!("{:+.1} dB", val));
        glib::Propagation::Stop
    });

    gain_spin.connect_input(|spin| {
        let text = spin.text();
        let text = text.trim().trim_end_matches("dB").trim_end_matches("db").trim();
        if let Ok(v) = text.parse::<f64>() {
            Some(Ok(v))
        } else {
            None
        }
    });

    // Q SpinButton
    let q_adj = gtk::Adjustment::new(1.0, Q_MIN, Q_MAX, 0.1, 0.5, 0.0);
    let q_spin = gtk::SpinButton::builder()
        .adjustment(&q_adj)
        .digits(2)
        .width_chars(5)
        .tooltip_text("Q Factor (bandwidth)")
        .build();
    q_spin.add_css_class("eq-spin");

    // Filter type dropdown
    let filter_labels: Vec<&str> = FilterType::ALL.iter().map(|ft| ft.label()).collect();
    let filter_model = gtk::StringList::new(&filter_labels);
    let filter_dropdown = gtk::DropDown::builder()
        .model(&filter_model)
        .tooltip_text("Filter Type")
        .build();
    filter_dropdown.add_css_class("eq-filter-dropdown");

    // Enable switch
    let enable_switch = gtk::Switch::builder()
        .tooltip_text("Enable/disable this band")
        .valign(gtk::Align::Center)
        .build();

    // Layout with labels
    let freq_box = labeled_control("Freq", &freq_spin);
    let gain_box = labeled_control("Gain", &gain_spin);
    let q_box = labeled_control("Q", &q_spin);
    let type_box = labeled_control("Type", &filter_dropdown);
    let enable_box = labeled_control("On", &enable_switch);

    container.append(&band_label);
    container.append(&gtk::Separator::new(gtk::Orientation::Vertical));
    container.append(&freq_box);
    container.append(&gain_box);
    container.append(&q_box);
    container.append(&type_box);
    container.append(&enable_box);

    // Wire up change handlers
    {
        let state = state.clone();
        let on_changed = on_changed.clone();
        let updating = updating.clone();
        freq_spin.connect_value_changed(move |spin| {
            if updating.get() { return; }
            let mut st = state.borrow_mut();
            st.selected_band_mut().frequency = spin.value().clamp(FREQ_MIN, FREQ_MAX);
            st.active_sink_eq_mut().preset_name = None;
            drop(st);
            on_changed();
        });
    }
    {
        let state = state.clone();
        let on_changed = on_changed.clone();
        let updating = updating.clone();
        gain_spin.connect_value_changed(move |spin| {
            if updating.get() { return; }
            let mut st = state.borrow_mut();
            st.selected_band_mut().gain_db = spin.value().clamp(GAIN_MIN, GAIN_MAX);
            st.active_sink_eq_mut().preset_name = None;
            drop(st);
            on_changed();
        });
    }
    {
        let state = state.clone();
        let on_changed = on_changed.clone();
        let updating = updating.clone();
        q_spin.connect_value_changed(move |spin| {
            if updating.get() { return; }
            let mut st = state.borrow_mut();
            st.selected_band_mut().q = spin.value().clamp(Q_MIN, Q_MAX);
            st.active_sink_eq_mut().preset_name = None;
            drop(st);
            on_changed();
        });
    }
    {
        let state = state.clone();
        let on_changed = on_changed.clone();
        let updating = updating.clone();
        let gain_spin_ref = gain_spin.clone();
        filter_dropdown.connect_selected_notify(move |dd| {
            if updating.get() { return; }
            let idx = dd.selected() as usize;
            if idx < FilterType::ALL.len() {
                let ft = FilterType::ALL[idx];
                let mut st = state.borrow_mut();
                st.selected_band_mut().filter_type = ft;
                st.active_sink_eq_mut().preset_name = None;
                gain_spin_ref.set_sensitive(ft.uses_gain());
                drop(st);
                on_changed();
            }
        });
    }
    {
        let state = state.clone();
        let on_changed = on_changed.clone();
        let updating = updating.clone();
        enable_switch.connect_state_set(move |_switch, active| {
            if updating.get() { return glib::Propagation::Proceed; }
            let mut st = state.borrow_mut();
            st.selected_band_mut().enabled = active;
            st.active_sink_eq_mut().preset_name = None;
            drop(st);
            on_changed();
            glib::Propagation::Proceed
        });
    }

    // Update function: refresh all controls from current state
    let update_fn: Rc<dyn Fn()> = {
        let state = state.clone();
        let updating = updating.clone();
        Rc::new(move || {
            updating.set(true);

            let st = state.borrow();
            let idx = st.selected_band;
            let band = &st.active_sink_eq().bands[idx];
            let (r, g, b) = BAND_COLORS[idx];

            band_label.set_label(&format!("Band {}", idx + 1));
            // Apply band color using Pango markup
            let markup = format!(
                "<span foreground=\"#{:02x}{:02x}{:02x}\"><b>Band {}</b></span>",
                (r * 255.0) as u32,
                (g * 255.0) as u32,
                (b * 255.0) as u32,
                idx + 1,
            );
            band_label.set_markup(&markup);

            freq_spin.set_value(band.frequency);
            gain_spin.set_value(band.gain_db);
            q_spin.set_value(band.q);
            gain_spin.set_sensitive(band.filter_type.uses_gain());
            enable_switch.set_active(band.enabled);

            let ft_idx = FilterType::ALL
                .iter()
                .position(|&ft| ft == band.filter_type)
                .unwrap_or(0);
            filter_dropdown.set_selected(ft_idx as u32);

            drop(st);
            updating.set(false);
        })
    };

    // Initial update
    update_fn();

    (container.upcast(), update_fn)
}

fn labeled_control(label: &str, widget: &impl IsA<gtk::Widget>) -> gtk::Box {
    let bx = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(2)
        .halign(gtk::Align::Center)
        .build();
    let lbl = gtk::Label::builder()
        .label(label)
        .build();
    lbl.add_css_class("dim-label");
    lbl.add_css_class("caption");
    bx.append(&lbl);
    bx.append(widget);
    bx
}
