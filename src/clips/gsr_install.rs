//! GSR Flatpak detection + install helpers for the onboarding wizard.

use std::process::{Command, Stdio};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::thread;

pub const GSR_APP_ID: &str = "com.dec05eba.gpu_screen_recorder";
pub const GSR_TERMINAL_INSTALL_COMMAND: &str =
    "flatpak install --user flathub com.dec05eba.gpu_screen_recorder";

/// Returns true if `flatpak info <app id>` succeeds (exit 0).
pub fn is_installed() -> bool {
    Command::new("flatpak")
        .args(["info", GSR_APP_ID])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Install progress updates emitted to the receiver.
#[derive(Debug)]
pub enum InstallProgress {
    Started,
    /// Indeterminate progress with a status string parsed from flatpak's stderr.
    Status(String),
    Done,
    Failed { reason: String },
}

/// Spawn a worker thread that runs `flatpak install --user --noninteractive --assumeyes
/// flathub com.dec05eba.gpu_screen_recorder` and streams progress events.
/// Returns immediately; events arrive on the receiver.
pub fn install() -> Receiver<InstallProgress> {
    let (tx, rx) = channel();
    thread::Builder::new()
        .name("gsr-install".into())
        .spawn(move || run_install(tx))
        .expect("spawn gsr-install");
    rx
}

fn run_install(tx: Sender<InstallProgress>) {
    let _ = tx.send(InstallProgress::Started);

    // --noninteractive avoids the y/n prompt; --assumeyes accepts license/etc.
    // --user installs into ~/.local/share/flatpak (no root needed).
    let mut child = match Command::new("flatpak")
        .args([
            "install",
            "--user",
            "--noninteractive",
            "--assumeyes",
            "flathub",
            GSR_APP_ID,
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
    {
        Ok(c) => c,
        Err(e) => {
            let _ = tx.send(InstallProgress::Failed {
                reason: format!("spawn failed: {e}"),
            });
            return;
        }
    };

    // Read stderr for status lines ("Installing...", "Downloading...", etc.).
    if let Some(stderr) = child.stderr.take() {
        let tx = tx.clone();
        thread::spawn(move || {
            use std::io::{BufRead, BufReader};
            let r = BufReader::new(stderr);
            for line in r.lines().map_while(Result::ok) {
                let trimmed = line.trim().to_string();
                if !trimmed.is_empty() {
                    let _ = tx.send(InstallProgress::Status(trimmed));
                }
            }
        });
    }

    let status = match child.wait() {
        Ok(s) => s,
        Err(e) => {
            let _ = tx.send(InstallProgress::Failed {
                reason: format!("wait failed: {e}"),
            });
            return;
        }
    };

    if status.success() {
        let _ = tx.send(InstallProgress::Done);
    } else {
        let _ = tx.send(InstallProgress::Failed {
            reason: format!("flatpak install exited with {status:?}"),
        });
    }
}

/// Open the GSR app page in the system app store via AppStream URI.
/// The actual handler depends on the desktop environment's `appstream://`
/// association: KDE opens Discover, GNOME opens Software, Bazzite-with-Bazaar
/// opens Bazaar, etc. We don't try to detect the handler — the URL is
/// generic and xdg-open routes it appropriately.
pub fn open_in_app_store() -> std::io::Result<()> {
    let url = format!("appstream://{GSR_APP_ID}");
    Command::new("xdg-open")
        .arg(url)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?
        .wait()
        .map(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn install_command_string_is_stable() {
        assert_eq!(
            GSR_TERMINAL_INSTALL_COMMAND,
            "flatpak install --user flathub com.dec05eba.gpu_screen_recorder"
        );
    }

    #[test]
    fn app_id_is_canonical() {
        assert_eq!(GSR_APP_ID, "com.dec05eba.gpu_screen_recorder");
    }
}
