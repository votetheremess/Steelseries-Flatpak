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

fn battery_icon_name(percent: u8) -> &'static str {
    match percent {
        0..=10 => "battery-empty-symbolic",
        11..=30 => "battery-caution-symbolic",
        31..=70 => "battery-good-symbolic",
        _ => "battery-full-symbolic",
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

        // Gear button on the left opens the Settings dialog
        let settings_button = gtk::Button::builder()
            .icon_name("emblem-system-symbolic")
            .tooltip_text("Settings")
            .build();
        {
            let app = app.clone();
            let window_weak = window.downgrade();
            settings_button.connect_clicked(move |_| {
                if let Some(parent) = window_weak.upgrade() {
                    present_settings_dialog(&app, &parent);
                }
            });
        }
        header_bar.pack_start(&settings_button);

        toolbar.add_top_bar(&header_bar);

        // Battery bar (right under the header)
        let battery_bar = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(0)
            .margin_top(8)
            .margin_bottom(8)
            .margin_start(16)
            .margin_end(16)
            .build();

        // Headset (left)
        let headset_bat_box = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(8)
            .halign(gtk::Align::Start)
            .hexpand(true)
            .build();
        let headphones_icon = gtk::Image::from_icon_name("audio-headphones-symbolic");
        headphones_icon.set_pixel_size(20);
        let headset_battery_icon = gtk::Image::from_icon_name("battery-symbolic");
        headset_battery_icon.set_pixel_size(20);
        let headset_battery_label = gtk::Label::builder().label(PLACEHOLDER).build();
        headset_battery_label.add_css_class("numeric");
        headset_bat_box.append(&headphones_icon);
        headset_bat_box.append(&headset_battery_icon);
        headset_bat_box.append(&headset_battery_label);
        battery_bar.append(&headset_bat_box);

        // Spare (right)
        let spare_bat_box = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(8)
            .halign(gtk::Align::End)
            .build();
        let spare_label_prefix = gtk::Label::builder().label("Spare").build();
        spare_label_prefix.add_css_class("dim-label");
        let spare_battery_icon = gtk::Image::from_icon_name("battery-symbolic");
        spare_battery_icon.set_pixel_size(20);
        let spare_battery_label = gtk::Label::builder().label(PLACEHOLDER).build();
        spare_battery_label.add_css_class("numeric");
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
        let device_icon = gtk::Image::from_icon_name("emblem-ok-symbolic");
        device_icon.add_css_class("success");
        device_row.add_prefix(&device_icon);
        device_group.add(&device_row);

        let noise_row = adw::ActionRow::builder()
            .title("Noise Control")
            .subtitle(PLACEHOLDER)
            .build();
        let noise_icon = gtk::Image::from_icon_name("audio-headphones-symbolic");
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

        // Info group
        let info_group = adw::PreferencesGroup::new();
        let info_label = gtk::Label::builder()
            .label(
                "Assign apps to \"ChatMix Game\" or \"ChatMix Chat\" in your system sound settings. \
                 Assignments are remembered between sessions by PipeWire.",
            )
            .wrap(true)
            .justify(gtk::Justification::Center)
            .margin_top(12)
            .margin_bottom(12)
            .margin_start(12)
            .margin_end(12)
            .build();
        info_label.add_css_class("dim-label");
        info_group.add(&info_label);
        page.add(&info_group);

        toolbar.set_content(Some(&page));
        window.set_content(Some(&toolbar));

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
        w.headset_battery_label.set_label(&format!("{headset}%"));
        w.headset_battery_icon
            .set_icon_name(Some(battery_icon_name(headset)));
        w.spare_battery_label.set_label(&format!("{spare}%"));
        w.spare_battery_icon
            .set_icon_name(Some(battery_icon_name(spare)));
    }
}

fn present_settings_dialog(app: &adw::Application, parent: &adw::ApplicationWindow) {
    let dialog = adw::PreferencesDialog::builder()
        .title("Settings")
        .build();

    let page = adw::PreferencesPage::new();

    // General section — Start at Login switch
    let general_group = adw::PreferencesGroup::builder().title("General").build();
    let autostart_row = adw::SwitchRow::builder()
        .title("Start at Login")
        .subtitle("Launch Arctis ChatMix hidden when you log in")
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
            // Revert switch on error
            row.set_active(!row.is_active());
        }
    });
    general_group.add(&autostart_row);
    page.add(&general_group);

    // Actions section — Clear and Quit buttons
    let actions_group = adw::PreferencesGroup::builder().title("Actions").build();

    let clear_row = adw::ActionRow::builder()
        .title("Clear Saved Assignments")
        .subtitle("Forget which apps were assigned to ChatMix Game/Chat")
        .build();
    let clear_button = gtk::Button::builder()
        .label("Clear")
        .valign(gtk::Align::Center)
        .build();
    clear_button.add_css_class("destructive-action");
    clear_button.connect_clicked(|_| {
        if let Err(e) = persistence::clear_saved() {
            log::warn!("Failed to clear saved assignments: {e}");
        }
    });
    clear_row.add_suffix(&clear_button);
    actions_group.add(&clear_row);

    let quit_row = adw::ActionRow::builder()
        .title("Quit")
        .subtitle("Stop ChatMix and release virtual sinks")
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
    actions_group.add(&quit_row);

    page.add(&actions_group);

    dialog.add(&page);
    dialog.present(Some(parent));
}
