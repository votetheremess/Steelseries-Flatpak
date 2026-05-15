//! Runtime `.desktop` file installer for portal app-id resolution.
//!
//! `xdg-desktop-portal`'s Registry interface (used by `ashpd::register_host_app`)
//! requires a `.desktop` file matching the app-id at one of the standard XDG
//! search paths (`$XDG_DATA_HOME/applications/`, `$XDG_DATA_DIRS/applications/`).
//! Without it, KDE's portal returns:
//!
//!     App info not found for 'com.github.arctis_chatmix.ArctisNovaEliteChatMix'
//!
//! We previously only installed the autostart copy at
//! `~/.config/autostart/`, which is NOT in the data search path. This module
//! drops a minimal entry into `~/.local/share/applications/` so the portal
//! resolver can find us. `NoDisplay=true` keeps the entry out of the user's
//! launcher (we don't want to clutter their app menu with a dev-build entry).
//!
//! Idempotent: only rewrites the file when the on-disk content drifts from
//! what we'd write now (e.g. binary path changed between dev sessions).

use std::path::{Path, PathBuf};

/// Install (or refresh) the `<app_id>.desktop` file in
/// `~/.local/share/applications/`. Returns the path it landed at on success.
pub fn install_desktop_file(app_id: &str) -> std::io::Result<PathBuf> {
    let home = std::env::var_os("HOME").ok_or_else(|| {
        std::io::Error::new(std::io::ErrorKind::NotFound, "HOME not set")
    })?;
    let exec = std::env::current_exe()?;
    install_desktop_file_at(&PathBuf::from(home), app_id, &exec)
}

/// Internal variant: writes the desktop file under `home_root` for the given
/// `app_id`, with the given exec path. Used by tests against tempdirs to avoid
/// touching the developer's real `~/.local/share/applications/`.
fn install_desktop_file_at(
    home_root: &Path,
    app_id: &str,
    exec: &Path,
) -> std::io::Result<PathBuf> {
    let dir = home_root.join(".local/share/applications");
    std::fs::create_dir_all(&dir)?;
    let path = dir.join(format!("{app_id}.desktop"));
    let body = format!(
        "[Desktop Entry]\n\
         Type=Application\n\
         Name=ChatMix\n\
         Comment=SteelSeries Arctis Nova Elite ChatMix and clipping\n\
         Exec={}\n\
         Icon=audio-headphones\n\
         Categories=AudioVideo;Audio;\n\
         StartupWMClass=arctis-chatmix\n\
         NoDisplay=true\n",
        exec.display()
    );
    let needs_write = match std::fs::read_to_string(&path) {
        Ok(existing) => existing != body,
        Err(_) => true,
    };
    if needs_write {
        std::fs::write(&path, body)?;
    }
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fresh_tempdir(label: &str) -> PathBuf {
        let temp = std::env::temp_dir().join(format!(
            "arctis-desktop-test-{}-{}-{}",
            label,
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        std::fs::create_dir_all(&temp).unwrap();
        temp
    }

    /// Second call with identical inputs must NOT rewrite the file. We confirm
    /// via mtime equality after a small sleep that exceeds typical filesystem
    /// timestamp resolution.
    #[test]
    fn install_desktop_file_is_idempotent() {
        let temp = fresh_tempdir("idem");
        let app_id = "com.example.test.Idem";
        let exec = PathBuf::from("/opt/arctis/arctis-chatmix");

        let path = install_desktop_file_at(&temp, app_id, &exec).unwrap();
        assert!(path.exists());
        let first_mtime = std::fs::metadata(&path).unwrap().modified().unwrap();

        std::thread::sleep(std::time::Duration::from_millis(20));
        let path2 = install_desktop_file_at(&temp, app_id, &exec).unwrap();
        assert_eq!(path, path2);
        let second_mtime = std::fs::metadata(&path2).unwrap().modified().unwrap();
        assert_eq!(
            first_mtime, second_mtime,
            "second install rewrote the file when content was unchanged"
        );

        let _ = std::fs::remove_dir_all(&temp);
    }

    #[test]
    fn install_desktop_file_writes_expected_keys() {
        let temp = fresh_tempdir("keys");
        let app_id = "com.example.test.Keys";
        let exec = PathBuf::from("/usr/local/bin/arctis-chatmix");

        let path = install_desktop_file_at(&temp, app_id, &exec).unwrap();
        let body = std::fs::read_to_string(&path).unwrap();
        assert!(body.starts_with("[Desktop Entry]\n"), "body: {body}");
        assert!(body.contains("Type=Application\n"), "body: {body}");
        assert!(body.contains("Name=ChatMix\n"), "body: {body}");
        assert!(body.contains("NoDisplay=true\n"), "body: {body}");
        assert!(
            body.contains("Exec=/usr/local/bin/arctis-chatmix\n"),
            "body: {body}"
        );

        let _ = std::fs::remove_dir_all(&temp);
    }

    /// When the binary path changes between launches, the next install_desktop_file
    /// call must rewrite the file so the portal sees the current Exec line.
    #[test]
    fn install_desktop_file_rewrites_when_exec_changes() {
        let temp = fresh_tempdir("rewrite");
        let app_id = "com.example.test.Rewrite";

        let path = install_desktop_file_at(
            &temp,
            app_id,
            &PathBuf::from("/old/path/arctis-chatmix"),
        )
        .unwrap();
        let first = std::fs::read_to_string(&path).unwrap();
        assert!(first.contains("Exec=/old/path/arctis-chatmix\n"));

        let path2 = install_desktop_file_at(
            &temp,
            app_id,
            &PathBuf::from("/new/path/arctis-chatmix"),
        )
        .unwrap();
        assert_eq!(path, path2);
        let second = std::fs::read_to_string(&path2).unwrap();
        assert!(second.contains("Exec=/new/path/arctis-chatmix\n"));
        assert!(!second.contains("Exec=/old/path/arctis-chatmix\n"));

        let _ = std::fs::remove_dir_all(&temp);
    }
}
