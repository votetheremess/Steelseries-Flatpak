use std::cell::RefCell;
use std::rc::Rc;

use adw::prelude::*;

use crate::autostart;
use crate::audio::persistence;
use crate::hid::protocol::NoiseMode;

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
    game_level: gtk::LevelBar,
    game_label: gtk::Label,
    chat_level: gtk::LevelBar,
    chat_label: gtk::Label,
}

/// Returns (icon name, is_critical) — is_critical means the icon should be
/// rendered in the error (red) color.
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
    pub fn new(app: &adw::Application) -> Self {
        let window = adw::ApplicationWindow::builder()
            .application(app)
            .title("Arctis Nova Elite ChatMix")
            .default_width(480)
            .default_height(640)
            .build();

        let toolbar = adw::ToolbarView::new();
        let header_bar = adw::HeaderBar::new();

        // Sidebar toggle button on the left of the header
        let sidebar_toggle = gtk::ToggleButton::builder()
            .icon_name("lucide-sidebar-symbolic")
            .tooltip_text("Toggle sidebar")
            .build();
        header_bar.pack_start(&sidebar_toggle);

        toolbar.add_top_bar(&header_bar);

        // Battery bar (right under the header, with breathing room)
        let battery_bar = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(0)
            .margin_top(16)
            .margin_bottom(8)
            .margin_start(20)
            .margin_end(20)
            .build();

        // Headset (left)
        let headset_bat_box = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(10)
            .halign(gtk::Align::Start)
            .hexpand(true)
            .build();
        let headset_prefix_icon = gtk::Image::from_icon_name("lucide-headset-symbolic");
        headset_prefix_icon.set_pixel_size(24);
        let headset_battery_icon = gtk::Image::from_icon_name("lucide-battery-symbolic");
        headset_battery_icon.set_pixel_size(24);
        let headset_battery_label = gtk::Label::builder().label(PLACEHOLDER).build();
        headset_battery_label.add_css_class("numeric");
        headset_battery_label.add_css_class("heading");
        headset_bat_box.append(&headset_prefix_icon);
        headset_bat_box.append(&headset_battery_icon);
        headset_bat_box.append(&headset_battery_label);
        battery_bar.append(&headset_bat_box);

        // Spare (right)
        let spare_bat_box = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(10)
            .halign(gtk::Align::End)
            .build();
        let spare_label_prefix = gtk::Label::builder().label("Spare").build();
        spare_label_prefix.add_css_class("dim-label");
        spare_label_prefix.add_css_class("heading");
        let spare_battery_icon = gtk::Image::from_icon_name("lucide-battery-symbolic");
        spare_battery_icon.set_pixel_size(24);
        let spare_battery_label = gtk::Label::builder().label(PLACEHOLDER).build();
        spare_battery_label.add_css_class("numeric");
        spare_battery_label.add_css_class("heading");
        spare_bat_box.append(&spare_label_prefix);
        spare_bat_box.append(&spare_battery_icon);
        spare_bat_box.append(&spare_battery_label);
        battery_bar.append(&spare_bat_box);

        toolbar.add_top_bar(&battery_bar);

        let page = adw::PreferencesPage::new();

        // Device group
        let device_group = adw::PreferencesGroup::builder().title("Device").build();

        let device_row = adw::ActionRow::builder()
            .title("Connected")
            .subtitle("Arctis Nova Elite")
            .build();
        let device_icon = gtk::Image::from_icon_name("lucide-check-symbolic");
        device_icon.add_css_class("success");
        device_row.add_prefix(&device_icon);
        device_group.add(&device_row);

        let noise_row = adw::ActionRow::builder()
            .title("Noise Control")
            .subtitle(PLACEHOLDER)
            .build();
        let noise_icon = gtk::Image::from_icon_name("lucide-headphones-symbolic");
        noise_row.add_prefix(&noise_icon);
        device_group.add(&noise_row);

        page.add(&device_group);

        // Balance group with a "Details" switch in the header
        let balance_group = adw::PreferencesGroup::builder()
            .title("ChatMix Balance")
            .build();

        let details_switch = gtk::Switch::builder()
            .valign(gtk::Align::Center)
            .tooltip_text("Show Game and Chat levels individually")
            .build();
        balance_group.set_header_suffix(Some(&details_switch));

        let balance_box = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(12)
            .margin_top(12)
            .margin_bottom(12)
            .margin_start(12)
            .margin_end(12)
            .build();

        // Balance view (shown when switch is OFF): labels + scale
        let balance_view = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(12)
            .build();

        let labels_box = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(12)
            .build();
        let chat_end_label = gtk::Label::builder()
            .label("Chat")
            .halign(gtk::Align::Start)
            .hexpand(true)
            .build();
        chat_end_label.add_css_class("dim-label");
        let game_end_label = gtk::Label::builder()
            .label("Game")
            .halign(gtk::Align::End)
            .build();
        game_end_label.add_css_class("dim-label");
        labels_box.append(&chat_end_label);
        labels_box.append(&game_end_label);
        balance_view.append(&labels_box);

        let balance_scale = gtk::Scale::with_range(
            gtk::Orientation::Horizontal,
            -100.0,
            100.0,
            1.0,
        );
        balance_scale.set_value(0.0);
        balance_scale.set_draw_value(false);
        balance_scale.set_sensitive(false);
        balance_view.append(&balance_scale);

        balance_box.append(&balance_view);

        // Details view (shown when switch is ON): separate Game and Chat level bars
        let details_view = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(8)
            .build();
        details_view.set_visible(false);

        let game_row = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(12)
            .build();
        let game_title = gtk::Label::builder()
            .label("Game")
            .width_chars(5)
            .xalign(0.0)
            .build();
        let game_level = gtk::LevelBar::builder()
            .min_value(0.0)
            .max_value(100.0)
            .value(0.0)
            .hexpand(true)
            .valign(gtk::Align::Center)
            .build();
        let game_label = gtk::Label::builder()
            .label(PLACEHOLDER)
            .width_chars(5)
            .xalign(1.0)
            .build();
        game_label.add_css_class("numeric");
        game_row.append(&game_title);
        game_row.append(&game_level);
        game_row.append(&game_label);
        details_view.append(&game_row);

        let chat_row = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(12)
            .build();
        let chat_title = gtk::Label::builder()
            .label("Chat")
            .width_chars(5)
            .xalign(0.0)
            .build();
        let chat_level = gtk::LevelBar::builder()
            .min_value(0.0)
            .max_value(100.0)
            .value(0.0)
            .hexpand(true)
            .valign(gtk::Align::Center)
            .build();
        let chat_label = gtk::Label::builder()
            .label(PLACEHOLDER)
            .width_chars(5)
            .xalign(1.0)
            .build();
        chat_label.add_css_class("numeric");
        chat_row.append(&chat_title);
        chat_row.append(&chat_level);
        chat_row.append(&chat_label);
        details_view.append(&chat_row);

        balance_box.append(&details_view);

        // Wire the switch to toggle between the two views
        let balance_view_clone = balance_view.clone();
        let details_view_clone = details_view.clone();
        details_switch.connect_active_notify(move |sw| {
            let on = sw.is_active();
            balance_view_clone.set_visible(!on);
            details_view_clone.set_visible(on);
        });

        balance_group.add(&balance_box);
        page.add(&balance_group);

        // Settings group — autostart switch sits inline on the main page
        let settings_group = adw::PreferencesGroup::new();
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
        settings_group.add(&autostart_row);
        page.add(&settings_group);

        // Bottom: Quit + Clear Config buttons (horizontal) above the info label
        let bottom_group = adw::PreferencesGroup::new();

        let buttons_box = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(12)
            .homogeneous(true)
            .margin_top(8)
            .margin_bottom(4)
            .margin_start(12)
            .margin_end(12)
            .build();

        let clear_button = gtk::Button::builder()
            .label("Clear Config")
            .build();
        clear_button.connect_clicked(|_| {
            if let Err(e) = persistence::clear_saved() {
                log::warn!("Failed to clear saved assignments: {e}");
            }
        });
        buttons_box.append(&clear_button);

        let quit_button = gtk::Button::builder()
            .label("Quit")
            .build();
        quit_button.add_css_class("destructive-action");
        {
            let app = app.clone();
            quit_button.connect_clicked(move |_| {
                app.quit();
            });
        }
        buttons_box.append(&quit_button);

        bottom_group.add(&buttons_box);

        // Info text below the buttons
        let info_label = gtk::Label::builder()
            .label(
                "Assign apps to \"ChatMix Game\" or \"ChatMix Chat\" in your system sound settings. \
                 Assignments are remembered between sessions by PipeWire.",
            )
            .wrap(true)
            .justify(gtk::Justification::Center)
            .margin_top(12)
            .margin_bottom(8)
            .margin_start(12)
            .margin_end(12)
            .build();
        info_label.add_css_class("dim-label");
        bottom_group.add(&info_label);

        page.add(&bottom_group);

        toolbar.set_content(Some(&page));

        // Sidebar — vertical list of page icons
        let sidebar = build_sidebar();

        // OverlaySplitView wraps everything: sidebar on the left, toolbar+content on the right
        let split_view = adw::OverlaySplitView::builder()
            .sidebar(&sidebar)
            .content(&toolbar)
            .show_sidebar(false)
            .collapsed(true)
            .min_sidebar_width(70.0)
            .max_sidebar_width(70.0)
            .sidebar_width_fraction(0.0)
            .build();

        // Bind the toggle button to the split view's show_sidebar property
        sidebar_toggle
            .bind_property("active", &split_view, "show-sidebar")
            .bidirectional()
            .sync_create()
            .build();

        window.set_content(Some(&split_view));

        let inner = Rc::new(RefCell::new(Widgets {
            device_row,
            noise_row,
            headset_battery_label,
            headset_battery_icon,
            spare_battery_label,
            spare_battery_icon,
            balance_scale,
            game_level,
            game_label,
            chat_level,
            chat_label,
        }));

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
        w.game_level.set_value(game as f64);
        w.game_label.set_label(&format!("{game}%"));
        w.chat_level.set_value(chat as f64);
        w.chat_label.set_label(&format!("{chat}%"));

        // Balance scale: -100 = all chat, +100 = all game
        let balance = game as f64 - chat as f64;
        w.balance_scale.set_value(balance);
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

fn build_sidebar() -> gtk::Widget {
    let sidebar_box = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(8)
        .margin_top(12)
        .margin_bottom(12)
        .margin_start(8)
        .margin_end(8)
        .build();
    sidebar_box.add_css_class("background");

    let home_button = gtk::ToggleButton::builder()
        .icon_name("lucide-home-symbolic")
        .tooltip_text("Home")
        .active(true)
        .height_request(48)
        .width_request(48)
        .build();
    home_button.add_css_class("flat");
    sidebar_box.append(&home_button);

    sidebar_box.upcast()
}

