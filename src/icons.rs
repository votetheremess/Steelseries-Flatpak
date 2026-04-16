use std::fs;
use std::path::PathBuf;

use gtk::gdk;

/// (lucide icon name, our `-symbolic` alias, raw SVG bytes)
/// The alias is what we reference in code via `gtk::Image::from_icon_name`.
const ICONS: &[(&str, &[u8])] = &[
    ("lucide-headphones-symbolic", include_bytes!("../data/icons/headphones.svg")),
    ("lucide-headset-symbolic", include_bytes!("../data/icons/headset.svg")),
    ("lucide-gamepad-symbolic", include_bytes!("../data/icons/gamepad-2.svg")),
    ("lucide-battery-symbolic", include_bytes!("../data/icons/battery.svg")),
    ("lucide-battery-low-symbolic", include_bytes!("../data/icons/battery-low.svg")),
    ("lucide-battery-medium-symbolic", include_bytes!("../data/icons/battery-medium.svg")),
    ("lucide-battery-full-symbolic", include_bytes!("../data/icons/battery-full.svg")),
    ("lucide-home-symbolic", include_bytes!("../data/icons/house.svg")),
    ("lucide-sidebar-symbolic", include_bytes!("../data/icons/panel-left.svg")),
    ("lucide-check-symbolic", include_bytes!("../data/icons/badge-check.svg")),
    ("lucide-audio-lines-symbolic", include_bytes!("../data/icons/audio-lines.svg")),
    ("lucide-sliders-horizontal-symbolic", include_bytes!("../data/icons/sliders-horizontal.svg")),
    ("lucide-clapperboard-symbolic", include_bytes!("../data/icons/clapperboard.svg")),
    ("lucide-bolt-symbolic", include_bytes!("../data/icons/zap.svg")),
    ("lucide-settings-symbolic", include_bytes!("../data/icons/settings.svg")),
    ("lucide-plug-zap-symbolic", include_bytes!("../data/icons/plug-zap.svg")),
    ("lucide-message-square-symbolic", include_bytes!("../data/icons/message-square.svg")),
    ("lucide-pencil-symbolic", include_bytes!("../data/icons/pencil.svg")),
];

fn icon_dir() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".cache")))?;
    Some(base.join("arctis-chatmix/icons/hicolor/scalable/actions"))
}

fn search_root() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".cache")))?;
    Some(base.join("arctis-chatmix/icons"))
}

/// Extract embedded Lucide SVGs to the user cache dir and register the path
/// with the GTK icon theme, so that `from_icon_name("lucide-*-symbolic")`
/// resolves to themed, theme-color-adaptive icons.
pub fn install() {
    let Some(dir) = icon_dir() else {
        log::warn!("Could not determine cache dir for icons");
        return;
    };

    if let Err(e) = fs::create_dir_all(&dir) {
        log::warn!("Failed to create icon cache dir {}: {e}", dir.display());
        return;
    }

    for (name, bytes) in ICONS {
        let path = dir.join(format!("{name}.svg"));
        if let Err(e) = fs::write(&path, bytes) {
            log::warn!("Failed to write {}: {e}", path.display());
        }
    }

    // Register the search path with GTK's icon theme
    if let Some(display) = gdk::Display::default() {
        let theme = gtk::IconTheme::for_display(&display);
        if let Some(root) = search_root() {
            theme.add_search_path(&root);
            log::info!("Registered Lucide icon path: {}", root.display());
        }
    } else {
        log::warn!("No gdk::Display available when installing Lucide icons");
    }
}
