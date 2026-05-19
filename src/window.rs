use std::cell::RefCell;
use std::rc::Rc;

use adw::prelude::*;

use std::sync::mpsc::Sender;

use crate::audio::{persistence, sinks};
use crate::autostart;
use crate::clips::settings::ClipSettings;
use crate::clips::{BufferController, ClipCommand, ClipsPage};
use crate::eq::model::{Band, EqTarget, SpatialState, NUM_BANDS};
use crate::hid::protocol::NoiseMode;
use crate::mixer::MixerWidgets;

const PLACEHOLDER: &str = "-";

/// Hooks the clips Settings page needs to live-react to user changes.
///
/// `app.rs` builds the partial form (everything but `clips_page`, which
/// it doesn't have access to) and passes it to `ChatMixWindow::new`,
/// which attaches its own `clips_page` and produces the full context
/// before forwarding to `build_settings_page`. Splitting the type
/// avoids leaking the page-construction order between the two modules.
pub struct ClipsSettingsContextPartial {
    pub clip_settings: Rc<RefCell<ClipSettings>>,
    pub buffer: Rc<RefCell<BufferController>>,
    pub cmd_tx: Sender<ClipCommand>,
    pub headset_sink_monitor: String,
}

/// Full settings hook bundle, with the runtime browser page attached.
/// Consumed by `build_settings_page` and `build_clips_group`.
pub struct ClipsSettingsContext {
    pub clip_settings: Rc<RefCell<ClipSettings>>,
    pub buffer: Rc<RefCell<BufferController>>,
    pub cmd_tx: Sender<ClipCommand>,
    pub headset_sink_monitor: String,
    pub clips_page: Rc<ClipsPage>,
}

pub struct ChatMixWindow {
    pub window: adw::ApplicationWindow,
    inner: Rc<RefCell<Widgets>>,
}

struct Widgets {
    device_row: adw::ActionRow,
    noise_row: adw::ActionRow,
    headset_battery_label: gtk::Label,
    headset_battery_icon: gtk::Image,
    spare_battery_label: gtk::Label,
    spare_battery_icon: gtk::Image,
    balance_scale: gtk::Scale,
    /// Owned by the dashboard row-1 section. `None` only in tests / partial
    /// builds; in normal operation always `Some` after `build_dashboard_page`.
    clips_section: Option<ClipsSectionWidgets>,
    mixer: Option<MixerWidgets>,
    clips: Option<Rc<crate::clips::ClipsPage>>,
    /// Sidebar Clips toggle button. Held so `show_clips_tab()` can flip it
    /// active, which triggers the bound `connect_toggled` handler that
    /// switches the content stack — the same path the user takes when
    /// clicking the sidebar themselves. Set during `ChatMixWindow::new`
    /// after the sidebar is built.
    clips_sidebar_btn: Option<gtk::ToggleButton>,
    /// Sidebar Settings toggle button. Same pattern as `clips_sidebar_btn`:
    /// `show_settings_tab()` flips it active so the new Duration / Hotkey
    /// jump buttons can navigate to Settings via a single GAction.
    settings_sidebar_btn: Option<gtk::ToggleButton>,
}

fn battery_icon(percent: u8) -> (&'static str, bool) {
    match percent {
        0..=9 => ("lucide-battery-symbolic", true),
        10..=19 => ("lucide-battery-low-symbolic", true),
        20..=39 => ("lucide-battery-low-symbolic", false),
        40..=69 => ("lucide-battery-medium-symbolic", false),
        _ => ("lucide-battery-full-symbolic", false),
    }
}

impl ChatMixWindow {
    pub fn new(
        app: &adw::Application,
        on_eq_apply: Option<Rc<dyn Fn(EqTarget, [Band; NUM_BANDS])>>,
        on_spatial_apply: Option<Rc<dyn Fn(EqTarget, SpatialState)>>,
        on_reroute: Option<Rc<dyn Fn(&str, &str)>>,
        on_mic_reroute: Option<Rc<dyn Fn(&str)>>,
        headset_sink: Option<String>,
        clips_settings_ctx_partial: Option<ClipsSettingsContextPartial>,
    ) -> Self {
        let window = adw::ApplicationWindow::builder()
            .application(app)
            .title("Arctis Nova Elite ChatMix")
            .default_width(1200)
            .default_height(725)
            .build();

        // Top-level horizontal box: sidebar | content
        let root = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .build();

        // Content area: header bar + stack
        let content_area = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .hexpand(true)
            .build();

        let header_bar = adw::HeaderBar::new();
        content_area.append(&header_bar);

        let stack = gtk::Stack::builder()
            .vexpand(true)
            .transition_type(gtk::StackTransitionType::Crossfade)
            .transition_duration(150)
            .build();

        // Build all pages
        let (dashboard_page, mut widgets) = build_dashboard_page();
        stack.add_named(&dashboard_page, Some("home"));
        let (eq_page, mixer_widgets) = crate::eq::build_eq_page(
            on_eq_apply, on_spatial_apply, on_reroute, on_mic_reroute, headset_sink,
        );
        widgets.mixer = mixer_widgets;
        stack.add_named(&eq_page, Some("eq"));
        let clips_page = Rc::new(crate::clips::build_clips_page());
        stack.add_named(clips_page.widget(), Some("clips"));
        widgets.clips = Some(clips_page.clone());
        stack.add_named(
            &build_placeholder_page("Engine", "lucide-sliders-horizontal-symbolic", "Coming soon"),
            Some("engine"),
        );
        // Promote the partial context to the full context by attaching the
        // runtime browser page we just built. Building inside ChatMixWindow
        // (rather than in app.rs) keeps the page-construction order
        // encapsulated: app.rs never touches `clips_page` directly.
        let clips_settings_ctx = clips_settings_ctx_partial.map(|p| ClipsSettingsContext {
            clip_settings: p.clip_settings,
            buffer: p.buffer,
            cmd_tx: p.cmd_tx,
            headset_sink_monitor: p.headset_sink_monitor,
            clips_page: clips_page.clone(),
        });
        stack.add_named(&build_settings_page(app, clips_settings_ctx), Some("settings"));

        content_area.append(&stack);

        // Build sidebar and wire to stack. The clips + settings toggle
        // buttons are held on `Widgets` so `show_clips_tab()` (from the
        // `app.show-clip` GAction handler) and `show_settings_tab()` (from
        // the new `app.show-clips-settings` GAction, fired by the home
        // page's Duration / Hotkey buttons) can flip them active
        // programmatically.
        let sidebar = build_sidebar(&stack);
        widgets.clips_sidebar_btn = Some(sidebar.clips_btn.clone());
        widgets.settings_sidebar_btn = Some(sidebar.settings_btn.clone());
        root.append(&sidebar.container);
        root.append(&content_area);

        // Wrap the entire root in an AdwToastOverlay so saved-clip toasts
        // (Phase 7 Task 7.1) can be presented from `clips::notifications`.
        // Structure becomes: Window → ToastOverlay → root: Box → [sidebar | content].
        // `find_toast_overlay` in `clips::notifications` walks the descendant
        // chain to locate the overlay at notification time.
        let toast_overlay = adw::ToastOverlay::new();
        toast_overlay.set_child(Some(&root));
        window.set_content(Some(&toast_overlay));

        let css = gtk::CssProvider::new();
        css.load_from_string(
            "window { font-size: 125%; } \
             row { padding-top: 4px; padding-bottom: 4px; } \
             .eq-graph-frame { background-color: rgba(0,0,0,0.05); border-radius: 8px; } \
             .eq-top-bar button { font-size: 80%; } \
             .eq-spin { font-size: 75%; font-weight: bold; } \
             .eq-filter-dropdown > button { font-size: 75%; min-height: 0; padding-top: 4px; padding-bottom: 4px; } \
             .eq-floating-panel { background-color: alpha(@window_bg_color, 0.92); border-radius: 8px; \
               padding: 8px 12px; margin-top: 4px; margin-bottom: 4px; border: 1px solid alpha(white, 0.12); } \
             .spatial-mix-pill { background-color: alpha(currentColor, 0.1); border-radius: 8px; \
               padding: 4px 12px; min-height: 30px; } \
             .spatial-mix-pill label { font-weight: bold; } \
             .eq-enable-switch { min-width: 36px; min-height: 18px; } \
             .mixer-channel { background-color: alpha(@window_bg_color, 0.92); border-radius: 8px; \
               padding: 8px 6px; border: 1px solid alpha(white, 0.12); } \
             .mixer-ch-master { box-shadow: inset 0 2px 0 rgba(255,255,255,0.35); } \
             .mixer-ch-game   { box-shadow: inset 0 2px 0 rgba(230,77,77,0.6); } \
             .mixer-ch-chat   { box-shadow: inset 0 2px 0 rgba(77,153,230,0.6); } \
             .mixer-ch-music  { box-shadow: inset 0 2px 0 rgba(77,204,179,0.6); } \
             .mixer-ch-aux    { box-shadow: inset 0 2px 0 rgba(242,140,64,0.6); } \
             .mixer-ch-mic    { box-shadow: inset 0 2px 0 rgba(179,102,230,0.6); } \
             .mixer-ch-master image { color: rgba(255,255,255,0.7); } \
             .mixer-ch-game image   { color: rgb(230,77,77); } \
             .mixer-ch-chat image   { color: rgb(77,153,230); } \
             .mixer-ch-music image  { color: rgb(77,204,179); } \
             .mixer-ch-aux image    { color: rgb(242,140,64); } \
             .mixer-ch-mic image    { color: rgb(179,102,230); } \
             .mixer-ch-master scale trough highlight { background-color: rgba(255,255,255,0.65); } \
             .mixer-ch-game scale trough highlight   { background-color: rgba(230,77,77,0.85); } \
             .mixer-ch-chat scale trough highlight   { background-color: rgba(77,153,230,0.85); } \
             .mixer-ch-music scale trough highlight  { background-color: rgba(77,204,179,0.85); } \
             .mixer-ch-aux scale trough highlight    { background-color: rgba(242,140,64,0.85); } \
             .mixer-ch-mic scale trough highlight    { background-color: rgba(179,102,230,0.85); } \
             .mixer-device-dropdown { font-size: 75%; } \
             .mixer-device-dropdown > button { min-height: 0; padding-top: 4px; padding-bottom: 4px; } \
             .clip-dot { min-width: 10px; min-height: 10px; border-radius: 5px; \
               transition: background-color 200ms; } \
             .clip-dot.dot-paused    { background-color: rgb(160,160,160); } \
             .clip-dot.dot-capturing { background-color: rgb(77,204,179); \
               animation: clip-pulse 1.6s infinite; } \
             .clip-dot.dot-saving    { background-color: rgb(242,205,64); } \
             .clip-dot.dot-error     { background-color: rgb(230,77,77); } \
             @keyframes clip-pulse { \
               0%   { opacity: 0.55; } \
               50%  { opacity: 1.0;  } \
               100% { opacity: 0.55; } \
             } \
             .clips-row-card { padding: 14px 18px; }"
        );
        gtk::style_context_add_provider_for_display(
            &gtk::prelude::WidgetExt::display(&window),
            &css,
            gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
        );

        let inner = Rc::new(RefCell::new(widgets));
        Self { window, inner }
    }

    pub fn set_connected(&self, connected: bool, device_name: Option<&str>) {
        let w = self.inner.borrow();
        if connected {
            w.device_row.set_title("Connected");
            w.device_row
                .set_subtitle(device_name.unwrap_or("Arctis Nova Elite"));
        } else {
            w.device_row.set_title("Disconnected");
            w.device_row.set_subtitle("Waiting for device…");
        }
    }

    pub fn set_noise_mode(&self, mode: NoiseMode) {
        let w = self.inner.borrow();
        w.noise_row.set_subtitle(&mode.to_string());
    }

    pub fn set_chatmix(&self, game: u8, chat: u8) {
        let w = self.inner.borrow();
        let balance = game as f64 - chat as f64;
        w.balance_scale.set_value(balance);
    }

    pub fn set_sink_volume(&self, sink_name: &str, pct: u8) {
        let w = self.inner.borrow();
        if let Some(ref m) = w.mixer {
            m.updating.set(true);
            let scale = match sink_name {
                sinks::GAME_SINK_NAME => &m.game_scale,
                sinks::CHAT_SINK_NAME => &m.chat_scale,
                sinks::MUSIC_SINK_NAME => &m.music_scale,
                sinks::AUX_SINK_NAME => &m.aux_scale,
                sinks::MIC_SOURCE_NAME => &m.mic_scale,
                _ => {
                    m.updating.set(false);
                    return;
                }
            };
            scale.set_value(pct as f64);
            m.updating.set(false);
        }
    }

    /// Returns the `ClipsPage` for this window, panicking if the window was
    /// constructed without one (which never happens in normal use — the field
    /// is always populated during `ChatMixWindow::new`).
    pub fn clips_page(&self) -> Rc<crate::clips::ClipsPage> {
        self.inner
            .borrow()
            .clips
            .clone()
            .expect("clips page is always set during window construction")
    }

    /// Switch the content stack to the Clips tab.
    ///
    /// Called from the `app.show-clip` GAction handler when the user clicks
    /// "Show" on a clip-saved toast/notification. Activating the sidebar
    /// toggle button triggers the same `connect_toggled` handler the user's
    /// own click goes through, so the stack switches and the radio-group
    /// state is consistent with the visible page.
    pub fn show_clips_tab(&self) {
        if let Some(btn) = self.inner.borrow().clips_sidebar_btn.as_ref() {
            btn.set_active(true);
        }
    }

    /// Switch the content stack to the Settings tab.
    ///
    /// Called from the `app.show-clips-settings` GAction handler, which is
    /// bound to the home-page Clips card's Duration / Hotkey buttons. Same
    /// mechanism as `show_clips_tab`: flip the sidebar toggle, let the
    /// existing `connect_toggled` handler do the stack switch.
    pub fn show_settings_tab(&self) {
        if let Some(btn) = self.inner.borrow().settings_sidebar_btn.as_ref() {
            btn.set_active(true);
        }
    }

    /// Refresh the row-1 Clips section to reflect the new `BufferState`.
    /// Driven from the backend event poll in `app.rs` after each
    /// `BufferController::on_backend_event` + after the auto-resume block at
    /// startup so the dot/label reflect the initial buffer state without
    /// flicker. `user_paused` is derived from the state because `Paused` is
    /// the only state where the buffer's `user_paused` flag is true, and
    /// this method's callers don't have the buffer ref handy.
    pub fn set_clips_state(&self, state: crate::clips::BufferState) {
        let w = self.inner.borrow();
        if let Some(section) = &w.clips_section {
            section.refresh_state(state, matches!(state, crate::clips::BufferState::Paused));
        }
    }

    /// Refresh the dashboard's row-1 Clips section: pause-button label /
    /// Full refresh of the row-1 Clips section: dot color, capture-toggle
    /// label / sensitivity, Duration button text, Quick Capture button
    /// text. Called by the `app.pause-recording-toggle` GAction handler
    /// and any other site that wants the section's controls to reflect a
    /// new buffer state / settings snapshot. The faster state-only
    /// refresh (no settings needed) goes through `set_clips_state`.
    pub fn refresh_clips_section(
        &self,
        state: crate::clips::BufferState,
        paused: bool,
        settings: &crate::clips::settings::ClipSettings,
    ) {
        if let Some(section) = &self.inner.borrow().clips_section {
            section.refresh(state, paused, settings);
        }
    }

    pub fn set_battery(&self, headset: u8, spare: u8) {
        let w = self.inner.borrow();

        let (headset_icon, headset_critical) = battery_icon(headset);
        w.headset_battery_label.set_label(&format!("{headset}%"));
        w.headset_battery_icon.set_icon_name(Some(headset_icon));
        apply_critical_class(&w.headset_battery_icon, headset_critical);

        let (spare_icon, spare_critical) = battery_icon(spare);
        w.spare_battery_label.set_label(&format!("{spare}%"));
        w.spare_battery_icon.set_icon_name(Some(spare_icon));
        apply_critical_class(&w.spare_battery_icon, spare_critical);
    }
}

fn apply_critical_class(image: &gtk::Image, critical: bool) {
    if critical {
        image.add_css_class("error");
    } else {
        image.remove_css_class("error");
    }
}

// ---------------------------------------------------------------------------
// Sidebar rail
// ---------------------------------------------------------------------------

struct SidebarResult {
    container: gtk::Widget,
    clips_btn: gtk::ToggleButton,
    settings_btn: gtk::ToggleButton,
}

fn build_sidebar(stack: &gtk::Stack) -> SidebarResult {
    let rail = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(8)
        .width_request(70)
        .margin_top(15)
        .margin_bottom(15)
        .margin_start(5)
        .margin_end(5)
        .build();
    rail.add_css_class("background");

    let home_btn = sidebar_button("lucide-home-symbolic", "Home");
    home_btn.set_active(true);

    let eq_btn = sidebar_button("lucide-audio-lines-symbolic", "Equalizer");
    eq_btn.set_group(Some(&home_btn));

    let clips_btn = sidebar_button("lucide-clapperboard-symbolic", "Clips");
    clips_btn.set_group(Some(&home_btn));

    let engine_btn = sidebar_button("lucide-sliders-horizontal-symbolic", "Engine (coming soon)");
    engine_btn.set_sensitive(false);
    engine_btn.set_group(Some(&home_btn));

    let settings_btn = sidebar_button("lucide-settings-symbolic", "Settings");
    settings_btn.set_group(Some(&home_btn));

    rail.append(&home_btn);
    rail.append(&eq_btn);
    rail.append(&clips_btn);
    rail.append(&engine_btn);

    // Spacer pushes settings to bottom
    let spacer = gtk::Box::builder().vexpand(true).build();
    rail.append(&spacer);
    rail.append(&settings_btn);

    // Wire navigation
    wire_sidebar_button(&home_btn, stack, "home");
    wire_sidebar_button(&eq_btn, stack, "eq");
    wire_sidebar_button(&clips_btn, stack, "clips");
    wire_sidebar_button(&engine_btn, stack, "engine");
    wire_sidebar_button(&settings_btn, stack, "settings");

    // Separator between sidebar and content
    let container = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .build();
    container.append(&rail);
    container.append(&gtk::Separator::new(gtk::Orientation::Vertical));

    SidebarResult {
        container: container.upcast(),
        clips_btn,
        settings_btn,
    }
}

fn sidebar_button(icon_name: &str, tooltip: &str) -> gtk::ToggleButton {
    let icon = gtk::Image::from_icon_name(icon_name);
    icon.set_pixel_size(22);
    let btn = gtk::ToggleButton::builder()
        .child(&icon)
        .tooltip_text(tooltip)
        .height_request(60)
        .width_request(60)
        .build();
    btn.add_css_class("flat");
    btn
}

fn wire_sidebar_button(btn: &gtk::ToggleButton, stack: &gtk::Stack, page: &str) {
    let stack = stack.clone();
    let page = page.to_string();
    btn.connect_toggled(move |b| {
        if b.is_active() {
            stack.set_visible_child_name(&page);
        }
    });
}

// ---------------------------------------------------------------------------
// Dashboard page
// ---------------------------------------------------------------------------

fn build_dashboard_page() -> (gtk::Widget, Widgets) {
    let scroll = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vexpand(true)
        .build();

    let clamp = adw::Clamp::builder()
        .maximum_size(1125)
        .margin_top(25)
        .margin_bottom(25)
        .margin_start(25)
        .margin_end(25)
        .build();

    let grid = gtk::Grid::builder()
        .column_spacing(15)
        .row_spacing(15)
        .column_homogeneous(true)
        .build();

    // Row 0: Status card (battery + chatmix) + Device card
    let (status_card, status_result) = build_status_card();
    let (device_card, dev_widgets) = build_device_card();

    // SizeGroup forces all rows to match the tallest (Device card's ActionRows).
    let row_height = gtk::SizeGroup::new(gtk::SizeGroupMode::Vertical);
    row_height.add_widget(&dev_widgets.0);
    row_height.add_widget(&dev_widgets.1);
    row_height.add_widget(&status_result.battery_row);
    row_height.add_widget(&status_result.chatmix_row);

    // Also match the cards themselves for any sub-pixel rounding
    let card_height = gtk::SizeGroup::new(gtk::SizeGroupMode::Vertical);
    card_height.add_widget(&status_card);
    card_height.add_widget(&device_card);

    grid.attach(&status_card, 0, 0, 1, 1);
    grid.attach(&device_card, 1, 0, 1, 1);

    // Row 1: full-width Clips section. Holds the indicator (cloned from the
    // section widgets so `set_clips_state` continues to update one shared
    // dot/label) plus the Save/Pause buttons + duration/hotkey hints.
    let (clips_section, clips_section_widgets) = build_clips_section();
    grid.attach(&clips_section, 0, 1, 2, 1);

    let footer = gtk::Label::builder()
        .label("Assign apps to SteelSeries sinks in your system sound settings. Assignments are remembered between sessions.")
        .wrap(true)
        .halign(gtk::Align::Center)
        .margin_top(12)
        .margin_bottom(24)
        .build();
    footer.add_css_class("dim-label");

    let content = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .build();
    content.append(&grid);
    let spacer = gtk::Box::builder().vexpand(true).build();
    content.append(&spacer);
    content.append(&footer);

    clamp.set_child(Some(&content));
    scroll.set_child(Some(&clamp));

    let widgets = Widgets {
        device_row: dev_widgets.0,
        noise_row: dev_widgets.1,
        headset_battery_label: status_result.headset_battery_label,
        headset_battery_icon: status_result.headset_battery_icon,
        spare_battery_label: status_result.spare_battery_label,
        spare_battery_icon: status_result.spare_battery_icon,
        balance_scale: status_result.balance_scale,
        clips_section: Some(clips_section_widgets),
        mixer: None,
        clips: None,
        clips_sidebar_btn: None,
        settings_sidebar_btn: None,
    };

    (scroll.upcast(), widgets)
}

// -- Clips section (row 1, full-width) --
//
// Single horizontal row inside an `adw::PreferencesGroup` titled "Clips".
// Layout (left → right):
//   [Save Clip] [● Capturing | Start Capturing] <spacer> [Duration: 60s] [Quick Capture: Alt + S]
//
// The capture-toggle button carries its own pulsing-green dot when the
// buffer is armed; when paused the dot is muted gray. Duration / hotkey
// labels are buttons themselves — clicking either jumps to the Settings
// tab via `app.show-clips-settings`.

pub struct ClipsSectionWidgets {
    pub save_button: gtk::Button,
    pub capture_toggle: gtk::Button,
    pub capture_dot: gtk::Widget,
    pub capture_label: gtk::Label,
    pub duration_btn: gtk::Button,
    pub hotkey_btn: gtk::Button,
}

impl ClipsSectionWidgets {
    /// Refresh both the high-frequency state (dot color, toggle label /
    /// sensitivity) and the low-frequency settings-derived labels
    /// (duration, hotkey). Called from the `app.pause-recording-toggle`
    /// handler and from any callsite that has a fresh `ClipSettings`
    /// snapshot to apply.
    pub fn refresh(
        &self,
        buffer_state: crate::clips::buffer::BufferState,
        user_paused: bool,
        settings: &crate::clips::settings::ClipSettings,
    ) {
        self.refresh_state(buffer_state, user_paused);

        // Duration button — literal seconds, no minute formatting, per
        // the redesign spec.
        self.duration_btn
            .set_label(&format!("Duration: {}s", settings.buffer_length));

        // Hotkey button — falls back to "Alt + S" when the portal hasn't
        // filled in the display string yet.
        let hk = if settings.save_hotkey_display.is_empty() {
            "Alt + S".to_string()
        } else {
            settings.save_hotkey_display.clone()
        };
        self.hotkey_btn
            .set_label(&format!("Quick Capture: {hk}"));
    }

    /// Refresh just the dot / capture-toggle pair (cheap, called on every
    /// backend event). Doesn't require a settings ref.
    ///
    /// State → visual table:
    ///
    /// | BufferState     | Dot class      | Toggle label       | Sensitive |
    /// |-----------------|----------------|--------------------|-----------|
    /// | Uninitialized   | dot-paused     | Start Capturing    | false     |
    /// | Idle            | dot-paused     | Start Capturing    | true      |
    /// | Arming / Armed  | dot-capturing  | Capturing          | true      |
    /// | Saving          | dot-saving     | Saving…            | false     |
    /// | ErrorState      | dot-error      | Start Capturing    | false     |
    /// | Paused          | dot-paused     | Start Capturing    | true      |
    ///
    /// `user_paused` overrides the label for the Pause → Resume race
    /// (state briefly reads Idle while the buffer transitions): if intent
    /// is "paused", the label sticks to "Start Capturing" until the next
    /// Armed event lands.
    pub fn refresh_state(
        &self,
        buffer_state: crate::clips::buffer::BufferState,
        user_paused: bool,
    ) {
        use crate::clips::buffer::BufferState as S;

        // Reset all state classes; reapply per the state below.
        for cls in ["dot-paused", "dot-capturing", "dot-saving", "dot-error"] {
            self.capture_dot.remove_css_class(cls);
        }

        let (dot_class, base_label, sensitive) = match buffer_state {
            S::Uninitialized => ("dot-paused", "Start Capturing", false),
            S::Idle => ("dot-paused", "Start Capturing", true),
            S::Arming | S::Armed => ("dot-capturing", "Capturing", true),
            S::Saving => ("dot-saving", "Saving…", false),
            S::ErrorState => ("dot-error", "Start Capturing", false),
            S::Paused => ("dot-paused", "Start Capturing", true),
        };
        self.capture_dot.add_css_class(dot_class);

        // user_paused override — see doc comment.
        let label = if user_paused && !matches!(buffer_state, S::Saving) {
            "Start Capturing"
        } else {
            base_label
        };
        self.capture_label.set_label(label);
        self.capture_toggle.set_sensitive(sensitive);

        // Tooltip flips with state so hover hints match the action the
        // click would perform.
        let tip = match (buffer_state, user_paused) {
            (S::Arming | S::Armed, false) => "Click to pause recording. The current rolling clip is lost.",
            (S::Paused, _) | (_, true) => "Click to resume recording.",
            (S::Idle, _) => "Click to start recording.",
            (S::Saving, _) => "Saving the last clip…",
            (S::ErrorState, _) => "Capture stopped. Open Settings to retry.",
            (S::Uninitialized, _) => "Set up Clips first (Clips tab).",
        };
        self.capture_toggle.set_tooltip_text(Some(tip));
    }
}

fn build_clips_section() -> (adw::PreferencesGroup, ClipsSectionWidgets) {
    let group = adw::PreferencesGroup::builder().title("Clips").build();

    // Single horizontal row. Margins match `clips-row-card` (15/15/8/8 in
    // the spec — we drive vertical via the action_box margins so the
    // ListBoxRow doesn't add extra padding above/below).
    let action_box = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(12)
        .margin_start(15)
        .margin_end(15)
        .margin_top(8)
        .margin_bottom(8)
        .build();

    // Save Clip — suggested-action keeps it visually primary.
    let save_button = gtk::Button::builder().label("Save Clip").build();
    save_button.add_css_class("suggested-action");
    save_button.set_action_name(Some("app.save-clip"));
    action_box.append(&save_button);

    // Capture toggle — plain Button (not ToggleButton) so we can manage
    // the visual state ourselves. The action is intentionally still
    // `app.pause-recording-toggle` even though the UI verb flipped to
    // "Capturing" / "Start Capturing" — renaming the action would break
    // any external D-Bus scripts the user has wired up. (Critic Major 5.)
    let dot = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .valign(gtk::Align::Center)
        .build();
    dot.add_css_class("clip-dot");
    dot.add_css_class("dot-paused"); // initial — refresh_state will repaint

    let capture_label = gtk::Label::builder().label("Start Capturing").build();

    let toggle_content = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(8)
        .valign(gtk::Align::Center)
        .build();
    toggle_content.append(&dot);
    toggle_content.append(&capture_label);

    let capture_toggle = gtk::Button::builder().child(&toggle_content).build();
    capture_toggle.set_action_name(Some("app.pause-recording-toggle"));
    action_box.append(&capture_toggle);

    // Spacer pushes the duration / hotkey buttons to the right.
    let spacer = gtk::Box::builder().hexpand(true).build();
    action_box.append(&spacer);

    // Duration jump button — flat so it reads as a hint, not a primary
    // action. Action fires `app.show-clips-settings` (registered in
    // `app.rs`) which navigates to the Settings tab.
    let duration_btn = gtk::Button::builder().label("Duration: 60s").build();
    duration_btn.add_css_class("flat");
    duration_btn.set_action_name(Some("app.show-clips-settings"));
    duration_btn.set_tooltip_text(Some("Open Clips settings to change the recording length"));
    action_box.append(&duration_btn);

    let hotkey_btn = gtk::Button::builder().label("Quick Capture: Alt + S").build();
    hotkey_btn.add_css_class("flat");
    hotkey_btn.set_action_name(Some("app.show-clips-settings"));
    hotkey_btn.set_tooltip_text(Some("Open Clips settings to change the save-clip hotkey"));
    action_box.append(&hotkey_btn);

    let row_action = gtk::ListBoxRow::builder()
        .child(&action_box)
        .activatable(false)
        .selectable(false)
        .build();
    group.add(&row_action);

    let widgets = ClipsSectionWidgets {
        save_button,
        capture_toggle,
        capture_dot: dot.upcast(),
        capture_label,
        duration_btn,
        hotkey_btn,
    };
    (group, widgets)
}

// -- Status card (battery + chatmix) --

struct StatusResult {
    headset_battery_label: gtk::Label,
    headset_battery_icon: gtk::Image,
    spare_battery_label: gtk::Label,
    spare_battery_icon: gtk::Image,
    balance_scale: gtk::Scale,
    battery_row: adw::ActionRow,
    chatmix_row: gtk::ListBoxRow,
}

fn build_status_card() -> (adw::PreferencesGroup, StatusResult) {
    let group = adw::PreferencesGroup::builder().title("Status").build();

    let battery_row = adw::ActionRow::new();
    battery_row.set_activatable(false);

    let headset_box = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(8)
        .build();
    let headset_icon = gtk::Image::from_icon_name("lucide-headset-symbolic");
    headset_icon.set_pixel_size(22);
    let headset_battery_icon = gtk::Image::from_icon_name("lucide-battery-symbolic");
    headset_battery_icon.set_pixel_size(22);
    let headset_battery_label = gtk::Label::builder().label(PLACEHOLDER).build();
    headset_battery_label.add_css_class("numeric");
    headset_box.append(&headset_icon);
    headset_box.append(&headset_battery_icon);
    headset_box.append(&headset_battery_label);
    battery_row.add_prefix(&headset_box);

    let spare_box = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(8)
        .build();
    let spare_prefix_icon = gtk::Image::from_icon_name("lucide-bolt-symbolic");
    spare_prefix_icon.set_pixel_size(22);
    let spare_battery_icon = gtk::Image::from_icon_name("lucide-battery-symbolic");
    spare_battery_icon.set_pixel_size(22);
    let spare_battery_label = gtk::Label::builder().label(PLACEHOLDER).build();
    spare_battery_label.add_css_class("numeric");
    spare_box.append(&spare_prefix_icon);
    spare_box.append(&spare_battery_icon);
    spare_box.append(&spare_battery_label);
    battery_row.add_suffix(&spare_box);

    group.add(&battery_row);

    let chatmix_box = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(15)
        .margin_start(15)
        .margin_end(15)
        .build();

    let game_icon = gtk::Image::from_icon_name("lucide-gamepad-symbolic");
    game_icon.set_pixel_size(22);
    let balance_scale = gtk::Scale::with_range(gtk::Orientation::Horizontal, -100.0, 100.0, 1.0);
    balance_scale.set_value(0.0);
    balance_scale.set_draw_value(false);
    balance_scale.set_sensitive(false);
    balance_scale.set_hexpand(true);
    balance_scale.set_inverted(true);
    let chat_icon = gtk::Image::from_icon_name("lucide-message-square-symbolic");
    chat_icon.set_pixel_size(22);

    chatmix_box.append(&game_icon);
    chatmix_box.append(&balance_scale);
    chatmix_box.append(&chat_icon);

    let chatmix_row = gtk::ListBoxRow::builder()
        .child(&chatmix_box)
        .activatable(false)
        .selectable(false)
        .build();
    group.add(&chatmix_row);

    let result = StatusResult {
        headset_battery_label,
        headset_battery_icon,
        spare_battery_label,
        spare_battery_icon,
        balance_scale,
        battery_row,
        chatmix_row,
    };
    (group, result)
}

// -- Device card --

fn build_device_card() -> (adw::PreferencesGroup, (adw::ActionRow, adw::ActionRow)) {
    let group = adw::PreferencesGroup::builder().title("Device").build();

    let device_row = adw::ActionRow::builder()
        .title("Connected")
        .subtitle("Arctis Nova Elite")
        .build();
    let device_icon = gtk::Image::from_icon_name("lucide-check-symbolic");
    device_icon.set_pixel_size(22);
    device_icon.add_css_class("success");
    device_row.add_prefix(&device_icon);
    group.add(&device_row);

    let noise_row = adw::ActionRow::builder()
        .title("Noise Control")
        .subtitle(PLACEHOLDER)
        .build();
    let noise_icon = gtk::Image::from_icon_name("lucide-headphones-symbolic");
    noise_icon.set_pixel_size(22);
    noise_row.add_prefix(&noise_icon);
    group.add(&noise_row);

    (group, (device_row, noise_row))
}

// ---------------------------------------------------------------------------
// Settings page
// ---------------------------------------------------------------------------

fn build_settings_page(
    app: &adw::Application,
    clips_settings_ctx: Option<ClipsSettingsContext>,
) -> gtk::Widget {
    let page = adw::PreferencesPage::new();

    // General
    let general_group = adw::PreferencesGroup::builder().title("General").build();
    let autostart_row = adw::SwitchRow::builder()
        .title("Start at Login")
        .subtitle("Launch hidden when you log in")
        .active(autostart::is_enabled())
        .build();
    autostart_row.connect_active_notify(|row| {
        let result = if row.is_active() {
            autostart::enable()
        } else {
            autostart::disable()
        };
        if let Err(e) = result {
            log::warn!("Failed to toggle autostart: {e}");
            row.set_active(!row.is_active());
        }
    });
    general_group.add(&autostart_row);
    page.add(&general_group);

    // Clips — only buildable if we have the runtime hooks. The pipeline
    // can fail to initialize on first launch (no headset, missing
    // permissions, etc.); in that case we skip the Clips section rather
    // than show settings the user can't actually act on.
    if let Some(ctx) = clips_settings_ctx {
        page.add(&crate::clips::settings::build_clips_group(
            ctx.clip_settings,
            ctx.buffer,
            ctx.cmd_tx,
            ctx.headset_sink_monitor,
            ctx.clips_page,
        ));
    }

    // Data
    let data_group = adw::PreferencesGroup::builder().title("Data").build();
    let clear_row = adw::ActionRow::builder()
        .title("Saved Assignments")
        .subtitle("App-to-sink routing remembered between sessions")
        .build();
    let clear_button = gtk::Button::builder()
        .label("Clear")
        .valign(gtk::Align::Center)
        .build();
    clear_button.connect_clicked(|_| {
        if let Err(e) = persistence::clear_saved() {
            log::warn!("Failed to clear saved assignments: {e}");
        }
    });
    clear_row.add_suffix(&clear_button);
    data_group.add(&clear_row);
    page.add(&data_group);

    // Application
    let app_group = adw::PreferencesGroup::builder().title("Application").build();
    let quit_row = adw::ActionRow::builder()
        .title("Quit")
        .subtitle("Stop the background service and destroy virtual sinks")
        .build();
    let quit_button = gtk::Button::builder()
        .label("Quit")
        .valign(gtk::Align::Center)
        .build();
    quit_button.add_css_class("destructive-action");
    {
        let app = app.clone();
        quit_button.connect_clicked(move |_| {
            app.quit();
        });
    }
    quit_row.add_suffix(&quit_button);
    app_group.add(&quit_row);
    page.add(&app_group);

    page.upcast()
}

// ---------------------------------------------------------------------------
// Placeholder pages
// ---------------------------------------------------------------------------

fn build_placeholder_page(title: &str, icon_name: &str, description: &str) -> gtk::Widget {
    let page = adw::StatusPage::builder()
        .icon_name(icon_name)
        .title(title)
        .description(description)
        .build();
    page.upcast()
}
