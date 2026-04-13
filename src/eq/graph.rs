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

// Grid frequencies for vertical lines
const GRID_FREQS: &[f64] = &[
    20.0, 50.0, 100.0, 200.0, 500.0, 1000.0, 2000.0, 5000.0, 10000.0, 20000.0,
];

// Grid dB values for horizontal lines
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

fn hit_test(
    x: f64,
    y: f64,
    state: &EqState,
    width: f64,
    height: f64,
) -> Option<usize> {
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
        format!("{}", f as u32)
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

    // Background
    cr.set_source_rgba(0.10, 0.10, 0.12, 1.0);
    cr.rectangle(PAD_LEFT, PAD_TOP, gw, gh);
    let _ = cr.fill();

    // Grid lines
    cr.set_line_width(1.0);

    // Vertical frequency grid
    for &freq in GRID_FREQS {
        let x = freq_to_x(freq, gw);
        cr.set_source_rgba(1.0, 1.0, 1.0, 0.08);
        cr.move_to(x, PAD_TOP);
        cr.line_to(x, PAD_TOP + gh);
        let _ = cr.stroke();

        // Label
        cr.set_source_rgba(1.0, 1.0, 1.0, 0.45);
        cr.set_font_size(10.0);
        let label = format_freq(freq);
        let extents = cr.text_extents(&label).unwrap();
        cr.move_to(x - extents.width() / 2.0, PAD_TOP + gh + 15.0);
        let _ = cr.show_text(&label);
    }

    // Horizontal dB grid
    for &db in GRID_DBS {
        let y = db_to_y(db, gh);
        let alpha = if db == 0.0 { 0.25 } else { 0.08 };
        let line_w = if db == 0.0 { 1.5 } else { 1.0 };
        cr.set_source_rgba(1.0, 1.0, 1.0, alpha);
        cr.set_line_width(line_w);
        cr.move_to(PAD_LEFT, y);
        cr.line_to(PAD_LEFT + gw, y);
        let _ = cr.stroke();

        // Label
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
    let selected = state.selected_band;

    // Individual band curves (filled area + line)
    for (i, band) in bands.iter().enumerate() {
        if !band.enabled {
            continue;
        }

        let is_active = i == selected;

        // In non-show-all mode, only draw the active band's individual curve
        if !show_all && !is_active {
            continue;
        }

        let (r, g, b) = BAND_COLORS[i];
        let responses: Vec<f64> = freqs.iter().map(|&f| biquad::magnitude_db(band, f)).collect();

        let zero_y = db_to_y(0.0, gh);

        // Filled area from 0dB to curve
        let fill_alpha = if is_active { 0.18 } else { 0.08 };
        cr.set_source_rgba(r, g, b, fill_alpha);
        cr.move_to(freq_to_x(freqs[0], gw), zero_y);
        for (j, &freq) in freqs.iter().enumerate() {
            let x = freq_to_x(freq, gw);
            let y = db_to_y(responses[j].clamp(GAIN_MIN, GAIN_MAX), gh);
            cr.line_to(x, y);
        }
        cr.line_to(freq_to_x(freqs[CURVE_SAMPLES - 1], gw), zero_y);
        cr.close_path();
        let _ = cr.fill();

        // Curve line
        let line_alpha = if is_active { 0.75 } else { 0.35 };
        cr.set_source_rgba(r, g, b, line_alpha);
        cr.set_line_width(if is_active { 2.0 } else { 1.5 });
        for (j, &freq) in freqs.iter().enumerate() {
            let x = freq_to_x(freq, gw);
            let y = db_to_y(responses[j].clamp(GAIN_MIN, GAIN_MAX), gh);
            if j == 0 {
                cr.move_to(x, y);
            } else {
                cr.line_to(x, y);
            }
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
        let y = db_to_y(combined[j].clamp(GAIN_MIN, GAIN_MAX), gh);
        if j == 0 {
            cr.move_to(x, y);
        } else {
            cr.line_to(x, y);
        }
    }
    let _ = cr.stroke();

    // Control points
    for (i, band) in bands.iter().enumerate() {
        let (r, g, b) = BAND_COLORS[i];
        let x = freq_to_x(band.frequency, gw);
        let y = db_to_y(band.gain_db, gh);

        let is_selected = i == selected;
        let is_hovered = hovered_band == Some(i);
        let radius = if is_selected {
            POINT_RADIUS_SELECTED
        } else if is_hovered {
            POINT_RADIUS + 2.0
        } else {
            POINT_RADIUS
        };

        // Distinct alpha so enabled-inactive vs disabled is obvious
        let alpha = if !band.enabled {
            0.20
        } else if is_selected {
            1.0
        } else if show_all {
            0.75
        } else {
            0.55
        };

        // Filled circle
        cr.arc(x, y, radius, 0.0, 2.0 * std::f64::consts::PI);
        cr.set_source_rgba(r, g, b, alpha);
        let _ = cr.fill();

        // Disabled band: draw a diagonal strike-through line
        if !band.enabled {
            cr.set_source_rgba(1.0, 1.0, 1.0, 0.5);
            cr.set_line_width(1.5);
            cr.move_to(x - radius * 0.6, y + radius * 0.6);
            cr.line_to(x + radius * 0.6, y - radius * 0.6);
            let _ = cr.stroke();
        }

        // Selection ring
        if is_selected {
            cr.arc(x, y, radius + 2.0, 0.0, 2.0 * std::f64::consts::PI);
            cr.set_source_rgba(1.0, 1.0, 1.0, 0.8);
            cr.set_line_width(2.0);
            let _ = cr.stroke();
        }

        // Band number label inside point (properly centered)
        let text_alpha = if !band.enabled { 0.3 } else { alpha.min(0.95) };
        cr.set_source_rgba(1.0, 1.0, 1.0, text_alpha);
        cr.set_font_size(10.0);
        let label = format!("{}", i + 1);
        let extents = cr.text_extents(&label).unwrap();
        // Center using x_bearing and y_bearing for true centering
        cr.move_to(
            x - (extents.width() / 2.0 + extents.x_bearing()),
            y - (extents.height() / 2.0 + extents.y_bearing()),
        );
        let _ = cr.show_text(&label);
    }

    // Border around graph area
    cr.set_source_rgba(1.0, 1.0, 1.0, 0.15);
    cr.set_line_width(1.0);
    cr.rectangle(PAD_LEFT, PAD_TOP, gw, gh);
    let _ = cr.stroke();
}

// ---------------------------------------------------------------------------
// Public: build the interactive DrawingArea
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
        let dragging_band: Rc<Cell<Option<usize>>> = Rc::new(Cell::new(None));
        let show_all: Rc<Cell<bool>> = Rc::new(Cell::new(false));

        // Draw function
        {
            let state = state.clone();
            let hovered = hovered_band.clone();
            let show_all = show_all.clone();
            area.set_draw_func(move |_area, cr, w, h| {
                draw_eq_graph(cr, w, h, &state.borrow(), hovered.get(), show_all.get());
            });
        }

        // Click — select band (must click first before dragging)
        {
            let state = state.clone();
            let area_ref = area.clone();
            let on_changed = on_changed.clone();
            let click = gtk::GestureClick::new();
            click.connect_pressed(move |_gesture, _n, x, y| {
                let w = area_ref.width() as f64;
                let h = area_ref.height() as f64;
                let st = state.borrow();
                if let Some(idx) = hit_test(x, y, &st, w, h) {
                    drop(st);
                    state.borrow_mut().selected_band = idx;
                    on_changed();
                    area_ref.queue_draw();
                }
            });
            area.add_controller(click);
        }

        // Drag — only moves the ALREADY-SELECTED band (click first to select)
        {
            let state = state.clone();
            let area_ref = area.clone();
            let on_changed = on_changed.clone();
            let drag_band = dragging_band.clone();

            let drag = gtk::GestureDrag::new();

            {
                let state = state.clone();
                let area_ref = area_ref.clone();
                let drag_band = drag_band.clone();
                drag.connect_drag_begin(move |_gesture, x, y| {
                    let w = area_ref.width() as f64;
                    let h = area_ref.height() as f64;
                    let st = state.borrow();
                    // Only allow dragging the currently selected band
                    let selected = st.selected_band;
                    if let Some(idx) = hit_test(x, y, &st, w, h) {
                        if idx == selected {
                            drag_band.set(Some(idx));
                        } else {
                            // Clicked a different band — just select it, don't drag
                            drag_band.set(None);
                        }
                    } else {
                        drag_band.set(None);
                    }
                });
            }

            {
                let state = state.clone();
                let area_ref = area_ref.clone();
                let on_changed = on_changed.clone();
                drag.connect_drag_update(move |gesture, offset_x, offset_y| {
                    let Some(idx) = drag_band.get() else { return };
                    let (start_x, start_y) = gesture.start_point().unwrap();
                    let cur_x = start_x + offset_x;
                    let cur_y = start_y + offset_y;

                    let w = area_ref.width() as f64;
                    let h = area_ref.height() as f64;
                    let (gw, gh) = graph_dims(w, h);

                    let new_freq = x_to_freq(cur_x, gw);
                    let new_gain = y_to_db(cur_y, gh);

                    let mut st = state.borrow_mut();
                    let band = &mut st.active_sink_eq_mut().bands[idx];
                    band.frequency = new_freq;
                    if band.filter_type.uses_gain() {
                        band.gain_db = new_gain;
                    }
                    st.active_sink_eq_mut().preset_name = None;
                    drop(st);

                    on_changed();
                    area_ref.queue_draw();
                });
            }

            area.add_controller(drag);
        }

        // Scroll — adjust Q
        {
            let state = state.clone();
            let area_ref = area.clone();
            let on_changed = on_changed.clone();
            let scroll = gtk::EventControllerScroll::new(
                gtk::EventControllerScrollFlags::VERTICAL,
            );
            scroll.connect_scroll(move |_controller, _dx, dy| {
                // Adjust Q on the currently selected band
                let mut st = state.borrow_mut();
                let idx = st.selected_band;
                let band = &mut st.active_sink_eq_mut().bands[idx];

                // Scroll up = increase Q (narrower), scroll down = decrease Q (wider)
                let factor = 1.15_f64.powf(-dy);
                band.q = (band.q * factor).clamp(Q_MIN, Q_MAX);
                st.active_sink_eq_mut().preset_name = None;
                drop(st);

                on_changed();
                area_ref.queue_draw();
                glib::Propagation::Stop
            });
            area.add_controller(scroll);
        }

        // Motion — hover effect + leave
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
}
