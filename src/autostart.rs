use std::fs;
use std::path::PathBuf;

const FILE_NAME: &str = "com.github.arctis_chatmix.ArctisNovaEliteChatMix.desktop";

fn desktop_file_path() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))?;
    Some(base.join("autostart").join(FILE_NAME))
}

pub fn is_enabled() -> bool {
    desktop_file_path().is_some_and(|p| p.exists())
}

pub fn enable() -> Result<(), String> {
    let Some(path) = desktop_file_path() else {
        return Err("Could not determine autostart directory".into());
    };
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| format!("Failed to create {parent:?}: {e}"))?;
    }

    let exe = std::env::current_exe()
        .map_err(|e| format!("Failed to get current exe path: {e}"))?;
    let exe_str = exe
        .to_str()
        .ok_or_else(|| "Exe path is not valid UTF-8".to_string())?;

    let content = format!(
        "[Desktop Entry]\n\
Type=Application\n\
Name=Arctis Nova Elite ChatMix\n\
Comment=ChatMix and battery monitoring for SteelSeries Arctis Nova Elite\n\
Exec={exe_str} --hidden\n\
Icon=audio-headphones\n\
Terminal=false\n\
Categories=AudioVideo;\n\
X-GNOME-Autostart-enabled=true\n"
    );

    fs::write(&path, content).map_err(|e| format!("Failed to write {path:?}: {e}"))?;
    log::info!("Autostart enabled at {}", path.display());
    Ok(())
}

pub fn disable() -> Result<(), String> {
    let Some(path) = desktop_file_path() else {
        return Err("Could not determine autostart directory".into());
    };
    if path.exists() {
        fs::remove_file(&path).map_err(|e| format!("Failed to remove {path:?}: {e}"))?;
        log::info!("Autostart disabled (removed {})", path.display());
    }
    Ok(())
}
