use std::sync::mpsc;

use ksni::{menu, Tray, TrayService};

/// Commands sent from the tray thread to the GTK main thread.
#[derive(Debug, Clone, Copy)]
pub enum TrayCommand {
    Show,
    Quit,
}

struct ChatMixTray {
    tx: mpsc::Sender<TrayCommand>,
}

impl Tray for ChatMixTray {
    fn id(&self) -> String {
        "com.github.arctis_chatmix.ArctisNovaEliteChatMix".into()
    }

    fn title(&self) -> String {
        "Arctis Nova Elite ChatMix".into()
    }

    fn icon_name(&self) -> String {
        // Use a freedesktop system icon name — tray hosts (Plasma, etc.) query
        // their own icon theme for this, so it must be a well-known name.
        "audio-headphones".into()
    }

    fn tool_tip(&self) -> ksni::ToolTip {
        ksni::ToolTip {
            icon_name: "audio-headphones".into(),
            title: "Arctis Nova Elite ChatMix".into(),
            description: "Click to show window".into(),
            icon_pixmap: vec![],
        }
    }

    // Left click — show the window
    fn activate(&mut self, _x: i32, _y: i32) {
        let _ = self.tx.send(TrayCommand::Show);
    }

    fn menu(&self) -> Vec<menu::MenuItem<Self>> {
        use menu::*;
        vec![
            StandardItem {
                label: "Show Window".into(),
                icon_name: "view-restore".into(),
                activate: Box::new(|t: &mut Self| {
                    let _ = t.tx.send(TrayCommand::Show);
                }),
                ..Default::default()
            }
            .into(),
            MenuItem::Separator,
            StandardItem {
                label: "Quit".into(),
                icon_name: "application-exit".into(),
                activate: Box::new(|t: &mut Self| {
                    let _ = t.tx.send(TrayCommand::Quit);
                }),
                ..Default::default()
            }
            .into(),
        ]
    }
}

/// Spawn the tray service on its own thread.
/// Returns the receiver for TrayCommand messages.
pub fn spawn() -> mpsc::Receiver<TrayCommand> {
    let (tx, rx) = mpsc::channel();
    let service = TrayService::new(ChatMixTray { tx });
    service.spawn();
    log::info!("Tray service spawned");
    rx
}
