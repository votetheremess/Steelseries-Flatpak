use std::cell::Cell;
use std::rc::Rc;

use gtk::prelude::*;
use gtk::glib;
use gtk::cairo;

use super::biquad;
use super::model::*;

const PAD_LEFT: f64 = 55.0;
const PAD_RIGHT: f64 = 20.0;
const PAD_TOP: f64 = 20.0;
const PAD_BOTTOM: f64 = 35.0;

const POINT_RADIUS: f64 = 8.0;
const POINT_RADIUS_SELECTED: f64 = 10.0;
const HIT_RADIUS: f64 = 18.0;

const CURVE_SAMPLES: usize = 300;

const GRID_FREQS: &[f64] = &[
    20.0, 50.0, 100.0, 200.0, 500.0, 1000.0, 2000.0, 5000.0, 10000.0, 20000.0,
];

const GRID_DBS: &[f64] = &[-12.0, -9.0, -6.0, -3.0, 0.0, 3.0, 6.0, 9.0, 12.0];

// ---------------------------------------------------------------------------
// Coordinate transforms
// ---------------------------------------------------------------------------

fn freq_to_x(freq: f64, graph_w: f64) -> f64 {
    let log_min = FREQ_MIN.log10();
    let log_max = FREQ_MAX.log10();
    PAD_LEFT + (freq.log10() - log_min) / (log_max - log_min) * graph_w
}

fn x_to_freq(x: f64, graph_w: f64) -> f64 {
    let log_min = FREQ_MIN.log10();
    let log_max = FREQ_MAX.log10();
    let t = (x - PAD_LEFT) / graph_w;
    10.0_f64.powf(log_min + t * (log_max - log_min)).clamp(FREQ_MIN, FREQ_MAX)
}

fn db_to_y(db: f64, graph_h: f64) -> f64 {
    PAD_TOP + (GAIN_MAX - db) / (GAIN_MAX - GAIN_MIN) * graph_h
}

fn y_to_db(y: f64, graph_h: f64) -> f64 {
    (GAIN_MAX - (y - PAD_TOP) / graph_h * (GAIN_MAX - GAIN_MIN)).clamp(GAIN_MIN, GAIN_MAX)
}

fn graph_dims(width: f64, height: f64) -> (f64, f64) {
    (
        (width - PAD_LEFT - PAD_RIGHT).max(1.0),
        (height - PAD_TOP - PAD_BOTTOM).max(1.0),
    )
}

// ---------------------------------------------------------------------------
// Hit testing
// ---------------------------------------------------------------------------

fn hit_test(x: f64, y: f64, state: &EqState, width: f64, height: f64) -> Option<usize> {
    let (gw, gh) = graph_dims(width, height);
    let bands = &state.active_sink_eq().bands;
    let mut best = None;
    let mut best_dist = f64::MAX;
    for (i, band) in bands.iter().enumerate() {
        let bx = freq_to_x(band.frequency, gw);
        let by = db_to_y(band.gain_db, gh);
        let dist = ((x - bx).powi(2) + (y - by).powi(2)).sqrt();
        if dist < HIT_RADIUS && dist < best_dist {
            best = Some(i);
            best_dist = dist;
        }
    }
    best
}

// ---------------------------------------------------------------------------
// Drawing
// ---------------------------------------------------------------------------

fn format_freq(f: f64) -> String {
    if f >= 1000.0 {
        format!("{}k", (f / 1000.0) as u32)
    } else {
        format!("{}", f.round() as u32)
    }
}

fn draw_eq_graph(
    cr: &cairo::Context,
    width: i32,
    height: i32,
    state: &EqState,
    hovered_band: Option<usize>,
    show_all: bool,
) {
    let w = width as f64;
    let h = height as f64;
    let (gw, gh) = graph_dims(w, h);
    let selected = state.selected_band;

    // Background
    cr.set_source_rgba(0.10, 0.10, 0.12, 1.0);
    cr.rectangle(PAD_LEFT, PAD_TOP, gw, gh);
    let _ = cr.fill();

    // Grid lines
    cr.set_line_width(1.0);

    for &freq in GRID_FREQS {
        let x = freq_to_x(freq, gw);
        cr.set_source_rgba(1.0, 1.0, 1.0, 0.08);
        cr.move_to(x, PAD_TOP);
        cr.line_to(x, PAD_TOP + gh);
        let _ = cr.stroke();

        cr.set_source_rgba(1.0, 1.0, 1.0, 0.45);
        cr.set_font_size(10.0);
        let label = format_freq(freq);
        let extents = cr.text_extents(&label).unwrap();
        cr.move_to(x - extents.width() / 2.0, PAD_TOP + gh + 15.0);
        let _ = cr.show_text(&label);
    }

    for &db in GRID_DBS {
        let y = db_to_y(db, gh);
        let alpha = if db == 0.0 { 0.25 } else { 0.08 };
        let line_w = if db == 0.0 { 1.5 } else { 1.0 };
        cr.set_source_rgba(1.0, 1.0, 1.0, alpha);
        cr.set_line_width(line_w);
        cr.move_to(PAD_LEFT, y);
        cr.line_to(PAD_LEFT + gw, y);
        let _ = cr.stroke();

        cr.set_source_rgba(1.0, 1.0, 1.0, 0.45);
        cr.set_font_size(10.0);
        let label = if db == 0.0 {
            "0 dB".to_string()
        } else {
            format!("{:+.0}", db)
        };
        let extents = cr.text_extents(&label).unwrap();
        cr.move_to(PAD_LEFT - extents.width() - 6.0, y + extents.height() / 2.0);
        let _ = cr.show_text(&label);
    }

    let bands = &state.active_sink_eq().bands;
    let freqs = biquad::log_frequencies(CURVE_SAMPLES);

    // Clip curves to the graph area so they naturally disappear behind borders
    let _ = cr.save();
    cr.rectangle(PAD_LEFT, PAD_TOP, gw, gh);
    cr.clip();

    // Individual band curves
    for (i, band) in bands.iter().enumerate() {
        if !band.enabled {
            continue;
        }
        let is_active = selected == Some(i);
        if !show_all && !is_active {
            continue;
        }

        let (r, g, b) = BAND_COLORS[i];
        let responses: Vec<f64> = freqs.iter().map(|&f| biquad::magnitude_db(band, f)).collect();
        let zero_y = db_to_y(0.0, gh);

        let fill_alpha = if is_active { 0.18 } else { 0.08 };
        cr.set_source_rgba(r, g, b, fill_alpha);
        cr.move_to(freq_to_x(freqs[0], gw), zero_y);
        for (j, &freq) in freqs.iter().enumerate() {
            let x = freq_to_x(freq, gw);
            let y = db_to_y(responses[j], gh);
            cr.line_to(x, y);
        }
        cr.line_to(freq_to_x(freqs[CURVE_SAMPLES - 1], gw), zero_y);
        cr.close_path();
        let _ = cr.fill();

        let line_alpha = if is_active { 0.75 } else { 0.35 };
        cr.set_source_rgba(r, g, b, line_alpha);
        cr.set_line_width(if is_active { 2.0 } else { 1.5 });
        for (j, &freq) in freqs.iter().enumerate() {
            let x = freq_to_x(freq, gw);
            let y = db_to_y(responses[j], gh);
            if j == 0 { cr.move_to(x, y); } else { cr.line_to(x, y); }
        }
        let _ = cr.stroke();
    }

    // Combined response curve
    let combined: Vec<f64> = freqs
        .iter()
        .map(|&f| biquad::combined_magnitude_db(bands, f))
        .collect();
    cr.set_source_rgba(1.0, 1.0, 1.0, 0.9);
    cr.set_line_width(2.0);
    for (j, &freq) in freqs.iter().enumerate() {
        let x = freq_to_x(freq, gw);
        let y = db_to_y(combined[j], gh);
        if j == 0 { cr.move_to(x, y); } else { cr.line_to(x, y); }
    }
    let _ = cr.stroke();

    // End curve clipping — control points should draw outside the graph area too
    let _ = cr.restore();

    // Control points
    for (i, band) in bands.iter().enumerate() {
        let (r, g, b) = BAND_COLORS[i];
        let x = freq_to_x(band.frequency, gw);
        let y = db_to_y(band.gain_db, gh);

        let is_selected = selected == Some(i);
        let is_hovered = hovered_band == Some(i);
        let radius = if is_selected {
            POINT_RADIUS_SELECTED
        } else if is_hovered {
            POINT_RADIUS + 2.0
        } else {
            POINT_RADIUS
        };

        let alpha = if !band.enabled {
            0.20
        } else if is_selected {
            1.0
        } else if show_all {
            0.75
        } else {
            0.55
        };

        cr.arc(x, y, radius, 0.0, 2.0 * std::f64::consts::PI);
        cr.set_source_rgba(r, g, b, alpha);
        let _ = cr.fill();

        if !band.enabled {
            cr.set_source_rgba(1.0, 1.0, 1.0, 0.5);
            cr.set_line_width(1.5);
            cr.move_to(x - radius * 0.6, y + radius * 0.6);
            cr.line_to(x + radius * 0.6, y - radius * 0.6);
            let _ = cr.stroke();
        }

        if is_selected {
            cr.arc(x, y, radius + 2.0, 0.0, 2.0 * std::f64::consts::PI);
            cr.set_source_rgba(1.0, 1.0, 1.0, 0.8);
            cr.set_line_width(2.0);
            let _ = cr.stroke();
        }

        let text_alpha = if !band.enabled { 0.3 } else { alpha.min(0.95) };
        cr.set_source_rgba(1.0, 1.0, 1.0, text_alpha);
        cr.set_font_size(10.0);
        let label = format!("{}", i + 1);
        let extents = cr.text_extents(&label).unwrap();
        cr.move_to(
            x - (extents.width() / 2.0 + extents.x_bearing()),
            y - (extents.height() / 2.0 + extents.y_bearing()),
        );
        let _ = cr.show_text(&label);
    }

    cr.set_source_rgba(1.0, 1.0, 1.0, 0.15);
    cr.set_line_width(1.0);
    cr.rectangle(PAD_LEFT, PAD_TOP, gw, gh);
    let _ = cr.stroke();
}

// ---------------------------------------------------------------------------
// Public
// ---------------------------------------------------------------------------

pub struct EqGraph {
    pub drawing_area: gtk::DrawingArea,
    pub show_all: Rc<Cell<bool>>,
}

impl EqGraph {
    pub fn new(state: SharedEqState, on_changed: Rc<dyn Fn()>) -> Self {
        let area = gtk::DrawingArea::new();
        area.set_content_height(300);
        area.set_content_width(800);
        area.set_vexpand(true);
        area.set_hexpand(true);

        let hovered_band: Rc<Cell<Option<usize>>> = Rc::new(Cell::new(None));
        let show_all: Rc<Cell<bool>> = Rc::new(Cell::new(false));

        // Track drag state: which dot was hit on mouse-down, whether actual movement happened
        let drag_hit: Rc<Cell<Option<usize>>> = Rc::new(Cell::new(None));
        let drag_moved: Rc<Cell<bool>> = Rc::new(Cell::new(false));

        // Draw
        {
            let state = state.clone();
            let hovered = hovered_band.clone();
            let show_all = show_all.clone();
            area.set_draw_func(move |_area, cr, w, h| {
                draw_eq_graph(cr, w, h, &state.borrow(), hovered.get(), show_all.get());
            });
        }

        // Single GestureDrag handles both clicks and drags — no GestureClick.
        // - drag_begin: record which dot was hit
        // - drag_update: move the dot if it's the selected one
        // - drag_end: if no movement, handle as a click (select/deselect)
        {
            let state = state.clone();
            let area_ref = area.clone();
            let on_changed = on_changed.clone();

            let drag = gtk::GestureDrag::new();

            {
                let state = state.clone();
                let area_ref = area_ref.clone();
                let drag_hit = drag_hit.clone();
                let drag_moved = drag_moved.clone();
                drag.connect_drag_begin(move |_gesture, x, y| {
                    drag_moved.set(false);
                    let w = area_ref.width() as f64;
                    let h = area_ref.height() as f64;
                    let st = state.borrow();
                    drag_hit.set(hit_test(x, y, &st, w, h));
                });
            }

            {
                let state = state.clone();
                let area_ref = area_ref.clone();
                let on_changed = on_changed.clone();
                let drag_hit = drag_hit.clone();
                let drag_moved = drag_moved.clone();
                drag.connect_drag_update(move |gesture, offset_x, offset_y| {
                    // Only drag the dot if it's already selected
                    let Some(hit_idx) = drag_hit.get() else { return };
                    let st = state.borrow();
                    let is_selected = st.selected_band == Some(hit_idx);
                    drop(st);
                    if !is_selected { return; }

                    drag_moved.set(true);
                    let (start_x, start_y) = gesture.start_point().unwrap();
                    let cur_x = start_x + offset_x;
                    let cur_y = start_y + offset_y;

                    let w = area_ref.width() as f64;
                    let h = area_ref.height() as f64;
                    let (gw, gh) = graph_dims(w, h);

                    let new_freq = x_to_freq(cur_x, gw);
                    let new_gain = y_to_db(cur_y, gh);

                    let mut st = state.borrow_mut();
                    let band = &mut st.active_sink_eq_mut().bands[hit_idx];
                    band.frequency = new_freq;
                    if band.filter_type.uses_gain() {
                        band.gain_db = new_gain;
                    }
                    drop(st);

                    on_changed();
                    area_ref.queue_draw();
                });
            }

            {
                let state = state.clone();
                let area_ref = area_ref.clone();
                let on_changed = on_changed.clone();
                let drag_hit = drag_hit.clone();
                let drag_moved = drag_moved.clone();
                drag.connect_drag_end(move |_gesture, _x, _y| {
                    if drag_moved.get() {
                        // Real drag happened — dot stays selected, do nothing
                        drag_hit.set(None);
                        return;
                    }

                    // No movement — treat as a click
                    let hit = drag_hit.get();
                    drag_hit.set(None);

                    let current = state.borrow().selected_band;

                    if hit == current && hit.is_some() {
                        // Click on already-selected dot → deselect
                        state.borrow_mut().selected_band = None;
                    } else if let Some(idx) = hit {
                        // Click on a different dot → select it
                        state.borrow_mut().selected_band = Some(idx);
                    } else {
                        // Click on empty space → deselect
                        state.borrow_mut().selected_band = None;
                    }
                    on_changed();
                    area_ref.queue_draw();
                });
            }

            area.add_controller(drag);
        }

        // Scroll — adjust Q on selected band
        {
            let state = state.clone();
            let area_ref = area.clone();
            let on_changed = on_changed.clone();
            let scroll = gtk::EventControllerScroll::new(
                gtk::EventControllerScrollFlags::VERTICAL,
            );
            scroll.connect_scroll(move |_controller, _dx, dy| {
                let mut st = state.borrow_mut();
                let Some(idx) = st.selected_band else {
                    return glib::Propagation::Proceed;
                };
                let band = &mut st.active_sink_eq_mut().bands[idx];
                let factor = 1.15_f64.powf(-dy);
                band.q = (band.q * factor).clamp(Q_MIN, Q_MAX);
                drop(st);

                on_changed();
                area_ref.queue_draw();
                glib::Propagation::Stop
            });
            area.add_controller(scroll);
        }

        // Motion — hover
        {
            let motion = gtk::EventControllerMotion::new();
            {
                let state = state.clone();
                let area_ref = area.clone();
                let hovered = hovered_band.clone();
                motion.connect_motion(move |_controller, x, y| {
                    let w = area_ref.width() as f64;
                    let h = area_ref.height() as f64;
                    let st = state.borrow();
                    let new_hover = hit_test(x, y, &st, w, h);
                    if new_hover != hovered.get() {
                        hovered.set(new_hover);
                        drop(st);
                        area_ref.queue_draw();
                    }
                });
            }
            {
                let area_ref = area.clone();
                let hovered = hovered_band.clone();
                motion.connect_leave(move |_controller| {
                    if hovered.get().is_some() {
                        hovered.set(None);
                        area_ref.queue_draw();
                    }
                });
            }
            area.add_controller(motion);
        }

        EqGraph { drawing_area: area, show_all }
    }

    pub fn queue_draw(&self) {
        self.drawing_area.queue_draw();
    }

    /// Returns the pixel (x, y) of the selected band's dot, or None.
    pub fn dot_position(&self, state: &EqState) -> Option<(f64, f64)> {
        let idx = state.selected_band?;
        let w = self.drawing_area.width() as f64;
        let h = self.drawing_area.height() as f64;
        let (gw, gh) = graph_dims(w, h);
        let band = &state.active_sink_eq().bands[idx];
        Some((freq_to_x(band.frequency, gw), db_to_y(band.gain_db, gh)))
    }
}
