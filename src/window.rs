use std::cell::RefCell;
use std::rc::Rc;

use adw::prelude::*;

use crate::audio::{persistence, sinks};
use crate::autostart;
use crate::eq::model::{Band, EqTarget, NUM_BANDS};
use crate::hid::protocol::NoiseMode;
use crate::mixer::MixerWidgets;

const PLACEHOLDER: &str = "—";

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
    mixer: Option<MixerWidgets>,
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
        on_reroute: Option<Rc<dyn Fn(&str, &str)>>,
        on_mic_reroute: Option<Rc<dyn Fn(&str)>>,
        headset_sink: Option<String>,
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
            on_eq_apply, on_reroute, on_mic_reroute, headset_sink,
        );
        widgets.mixer = mixer_widgets;
        stack.add_named(&eq_page, Some("eq"));
        stack.add_named(
            &build_placeholder_page("Clips", "lucide-clapperboard-symbolic", "Coming soon"),
            Some("clips"),
        );
        stack.add_named(
            &build_placeholder_page("Engine", "lucide-sliders-horizontal-symbolic", "Coming soon"),
            Some("engine"),
        );
        stack.add_named(&build_settings_page(app), Some("settings"));

        content_area.append(&stack);

        // Build sidebar and wire to stack
        let sidebar = build_sidebar(&stack);
        root.append(&sidebar);
        root.append(&content_area);

        window.set_content(Some(&root));

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
             .eq-enable-switch { min-width: 36px; min-height: 18px; } \
             .mixer-device-dropdown { font-size: 75%; } \
             .mixer-device-dropdown > button { min-height: 0; padding-top: 4px; padding-bottom: 4px; }"
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

fn build_sidebar(stack: &gtk::Stack) -> gtk::Widget {
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

    let clips_btn = sidebar_button("lucide-clapperboard-symbolic", "Clips (coming soon)");
    clips_btn.set_sensitive(false);
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

    container.upcast()
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

    // SizeGroup forces all rows to match the tallest (Device card's ActionRows)
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
        mixer: None,
    };

    (scroll.upcast(), widgets)
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

fn build_settings_page(app: &adw::Application) -> gtk::Widget {
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
