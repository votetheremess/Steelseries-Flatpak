use std::cell::Cell;
use std::rc::Rc;

use adw::prelude::*;
use gtk::pango;

use crate::audio::sinks;

pub struct MixerWidgets {
    pub updating: Rc<Cell<bool>>,
    pub game_scale: gtk::Scale,
    pub chat_scale: gtk::Scale,
    pub music_scale: gtk::Scale,
    pub aux_scale: gtk::Scale,
    pub mic_scale: gtk::Scale,
    pub master_scale: gtk::Scale,
}

struct ChannelDef {
    label: &'static str,
    icon: &'static str,
    /// PipeWire node name for volume control, or None for Master (uses headset_sink)
    sink_name: Option<&'static str>,
    is_source: bool,
}

const CHANNELS: &[ChannelDef] = &[
    ChannelDef { label: "Game", icon: "lucide-gamepad-symbolic", sink_name: Some(sinks::GAME_SINK_NAME), is_source: false },
    ChannelDef { label: "Chat", icon: "lucide-message-square-symbolic", sink_name: Some(sinks::CHAT_SINK_NAME), is_source: false },
    ChannelDef { label: "Music", icon: "lucide-audio-lines-symbolic", sink_name: Some(sinks::MUSIC_SINK_NAME), is_source: false },
    ChannelDef { label: "Aux", icon: "lucide-plug-zap-symbolic", sink_name: Some(sinks::AUX_SINK_NAME), is_source: false },
];

const MIC_CHANNEL: ChannelDef = ChannelDef {
    label: "Mic",
    icon: "lucide-headset-symbolic",
    sink_name: Some(sinks::MIC_SOURCE_NAME),
    is_source: true,
};

pub fn build_mixer_content(
    on_reroute: Option<Rc<dyn Fn(&str, &str)>>,
    on_mic_reroute: Option<Rc<dyn Fn(&str)>>,
    headset_sink: Option<String>,
) -> (gtk::Widget, MixerWidgets) {
    let updating = Rc::new(Cell::new(false));
    let dropdown_sg = gtk::SizeGroup::new(gtk::SizeGroupMode::Vertical);

    let root = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(12)
        .halign(gtk::Align::Center)
        .valign(gtk::Align::Fill)
        .vexpand(true)
        .margin_top(12)
        .margin_bottom(12)
        .margin_start(12)
        .margin_end(12)
        .build();

    // Master channel
    let (master_strip, master_scale) = build_channel_strip(
        "Master",
        "lucide-headphones-symbolic",
        headset_sink.as_deref(),
        false,
        &updating,
        None,
        "master",
        &dropdown_sg,
    );
    root.append(&master_strip);

    // Output sink channels
    let mut game_scale = None;
    let mut chat_scale = None;
    let mut music_scale = None;
    let mut aux_scale = None;

    for ch in CHANNELS {
        let device_dropdown = build_sink_dropdown(
            ch.sink_name.unwrap(),
            headset_sink.as_deref(),
            on_reroute.clone(),
        );
        let (strip, scale) = build_channel_strip(
            ch.label,
            ch.icon,
            ch.sink_name,
            ch.is_source,
            &updating,
            Some(device_dropdown),
            &ch.label.to_ascii_lowercase(),
            &dropdown_sg,
        );
        root.append(&strip);

        match ch.label {
            "Game" => game_scale = Some(scale),
            "Chat" => chat_scale = Some(scale),
            "Music" => music_scale = Some(scale),
            "Aux" => aux_scale = Some(scale),
            _ => {}
        }
    }

    // Mic channel
    let mic_dropdown = build_source_dropdown(
        headset_sink.as_deref(),
        on_mic_reroute,
    );
    let (mic_strip, mic_scale) = build_channel_strip(
        MIC_CHANNEL.label,
        MIC_CHANNEL.icon,
        MIC_CHANNEL.sink_name,
        MIC_CHANNEL.is_source,
        &updating,
        Some(mic_dropdown),
        "mic",
        &dropdown_sg,
    );
    root.append(&mic_strip);

    let widgets = MixerWidgets {
        updating,
        game_scale: game_scale.unwrap(),
        chat_scale: chat_scale.unwrap(),
        music_scale: music_scale.unwrap(),
        aux_scale: aux_scale.unwrap(),
        mic_scale,
        master_scale,
    };

    (root.upcast(), widgets)
}

fn build_channel_strip(
    label: &str,
    icon_name: &str,
    sink_name: Option<&str>,
    is_source: bool,
    updating: &Rc<Cell<bool>>,
    device_dropdown: Option<gtk::DropDown>,
    channel_css_suffix: &str,
    dropdown_size_group: &gtk::SizeGroup,
) -> (gtk::Box, gtk::Scale) {
    let strip = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(6)
        .width_request(150)
        .halign(gtk::Align::Fill)
        .hexpand(false)
        .build();
    strip.add_css_class("mixer-channel");
    strip.add_css_class(&format!("mixer-ch-{channel_css_suffix}"));

    // Channel name
    let name_label = gtk::Label::builder()
        .label(label)
        .halign(gtk::Align::Center)
        .build();
    name_label.add_css_class("heading");
    strip.append(&name_label);

    // Icon
    let icon = gtk::Image::from_icon_name(icon_name);
    icon.set_pixel_size(22);
    icon.set_halign(gtk::Align::Center);
    strip.append(&icon);

    // Volume scale (vertical, inverted so 100% is at top)
    let scale = gtk::Scale::with_range(gtk::Orientation::Vertical, 0.0, 100.0, 1.0);
    scale.set_inverted(true);
    scale.set_draw_value(false);
    scale.set_vexpand(true);
    scale.set_height_request(250);
    scale.set_halign(gtk::Align::Center);
    scale.set_value(read_initial_volume(sink_name, is_source));
    strip.append(&scale);

    // Percentage label
    let pct_label = gtk::Label::builder()
        .label(&format!("{}%", scale.value() as u32))
        .halign(gtk::Align::Center)
        .build();
    pct_label.add_css_class("numeric");
    strip.append(&pct_label);

    // Wire volume change
    {
        let updating = updating.clone();
        let pct_label = pct_label.clone();
        let sink_owned = sink_name.map(|s| s.to_string());
        scale.connect_value_changed(move |s| {
            let pct = s.value() as u32;
            pct_label.set_label(&format!("{pct}%"));
            if updating.get() {
                return;
            }
            if let Some(ref name) = sink_owned {
                if is_source {
                    sinks::set_source_volume(name, pct).ok();
                } else {
                    sinks::set_sink_volume(name, pct).ok();
                }
            }
        });
    }

    // Dropdown wrapper (or placeholder for Master) — kept in a SizeGroup for equal height
    let dropdown_wrapper = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .margin_start(8)
        .margin_end(8)
        .build();

    if let Some(dropdown) = device_dropdown {
        dropdown.set_hexpand(true);
        dropdown_wrapper.append(&dropdown);
    }
    // If no dropdown (Master), the wrapper stays empty as a height placeholder

    dropdown_size_group.add_widget(&dropdown_wrapper);
    strip.append(&dropdown_wrapper);

    (strip, scale)
}

/// Read the current volume for initial slider position.
fn read_initial_volume(sink_name: Option<&str>, is_source: bool) -> f64 {
    let Some(name) = sink_name else {
        return 100.0;
    };
    let result = if is_source {
        sinks::get_source_volume(name)
    } else {
        sinks::get_sink_volume(name)
    };
    result.unwrap_or(100) as f64
}

/// Factory for the dropdown button label: truncates long device names with ellipsis.
fn truncating_label_factory() -> gtk::SignalListItemFactory {
    let factory = gtk::SignalListItemFactory::new();
    factory.connect_setup(|_factory, list_item| {
        let list_item = list_item.downcast_ref::<gtk::ListItem>().unwrap();
        let label = gtk::Label::builder()
            .xalign(0.0)
            .ellipsize(pango::EllipsizeMode::End)
            .max_width_chars(1)
            .hexpand(true)
            .build();
        list_item.set_child(Some(&label));
    });
    factory.connect_bind(|_factory, list_item| {
        let list_item = list_item.downcast_ref::<gtk::ListItem>().unwrap();
        let item = list_item.item().and_downcast::<gtk::StringObject>().unwrap();
        let label = list_item.child().and_downcast::<gtk::Label>().unwrap();
        label.set_label(&item.string());
        label.set_tooltip_text(Some(&item.string()));
    });
    factory
}

/// Factory for the dropdown popup list: wraps long device names so they're fully readable.
fn popup_label_factory() -> gtk::SignalListItemFactory {
    let factory = gtk::SignalListItemFactory::new();
    factory.connect_setup(|_factory, list_item| {
        let list_item = list_item.downcast_ref::<gtk::ListItem>().unwrap();
        let label = gtk::Label::builder()
            .xalign(0.0)
            .wrap(true)
            .wrap_mode(pango::WrapMode::WordChar)
            .build();
        list_item.set_child(Some(&label));
    });
    factory.connect_bind(|_factory, list_item| {
        let list_item = list_item.downcast_ref::<gtk::ListItem>().unwrap();
        let item = list_item.item().and_downcast::<gtk::StringObject>().unwrap();
        let label = list_item.child().and_downcast::<gtk::Label>().unwrap();
        label.set_label(&item.string());
    });
    factory
}

/// Build device dropdown for an output sink, populated with physical sinks.
fn build_sink_dropdown(
    sink_name: &str,
    headset_sink: Option<&str>,
    on_reroute: Option<Rc<dyn Fn(&str, &str)>>,
) -> gtk::DropDown {
    let devices = sinks::list_physical_sinks();
    let display_names: Vec<&str> = devices.iter().map(|(_, desc)| desc.as_str()).collect();
    let model = gtk::StringList::new(&display_names);
    let dropdown = gtk::DropDown::builder()
        .model(&model)
        .factory(&truncating_label_factory())
        .list_factory(&popup_label_factory())
        .build();
    dropdown.add_css_class("mixer-device-dropdown");

    // Select the headset sink by default
    if let Some(hs) = headset_sink {
        for (i, (name, _)) in devices.iter().enumerate() {
            if name == hs {
                dropdown.set_selected(i as u32);
                break;
            }
        }
    }

    // Wire change handler
    {
        let sink_name = sink_name.to_string();
        let devices = devices.clone();
        dropdown.connect_selected_notify(move |dd| {
            let idx = dd.selected() as usize;
            if idx >= devices.len() {
                return;
            }
            let (ref device_name, _) = devices[idx];
            if let Some(ref cb) = on_reroute {
                cb(&sink_name, device_name);
            }
        });
    }

    dropdown
}

/// Build device dropdown for the mic, populated with physical sources.
fn build_source_dropdown(
    _headset_sink: Option<&str>,
    on_mic_reroute: Option<Rc<dyn Fn(&str)>>,
) -> gtk::DropDown {
    let devices = sinks::list_physical_sources();
    let display_names: Vec<&str> = devices.iter().map(|(_, desc)| desc.as_str()).collect();
    let model = gtk::StringList::new(&display_names);
    let dropdown = gtk::DropDown::builder()
        .model(&model)
        .factory(&truncating_label_factory())
        .list_factory(&popup_label_factory())
        .build();
    dropdown.add_css_class("mixer-device-dropdown");

    // Select the first source by default (headset mic is typically first)
    // Will be overridden by saved routing later

    // Wire change handler
    {
        let devices = devices.clone();
        dropdown.connect_selected_notify(move |dd| {
            let idx = dd.selected() as usize;
            log::info!("Mic dropdown selected index {idx}");
            if idx >= devices.len() {
                log::warn!("Mic dropdown index {idx} out of range ({})", devices.len());
                return;
            }
            let (ref source_name, ref desc) = devices[idx];
            log::info!("Mic dropdown selected: {desc} ({source_name})");
            if let Some(ref cb) = on_mic_reroute {
                cb(source_name);
            } else {
                log::warn!("on_mic_reroute callback is None");
            }
        });
    }

    dropdown
}
