//! ScreenCast portal interaction via ashpd.

use ashpd::desktop::{
    screencast::{CursorMode, Screencast, SourceType},
    PersistMode,
};
use std::path::PathBuf;

const PORTAL_TOKEN_FILE: &str = "clips_portal.txt";

fn token_path() -> PathBuf {
    let home = std::env::var_os("HOME").expect("HOME");
    PathBuf::from(home)
        .join(".config/arctis-chatmix")
        .join(PORTAL_TOKEN_FILE)
}

pub fn load_token() -> Option<String> {
    let path = token_path();
    match std::fs::read_to_string(&path) {
        Ok(s) => {
            let trimmed = s.trim().to_string();
            if trimmed.is_empty() {
                log::info!(
                    "portal::load_token: file at {} is empty, returning None",
                    path.display()
                );
                None
            } else {
                log::info!(
                    "portal::load_token: loaded {} bytes from {}",
                    trimmed.len(),
                    path.display()
                );
                Some(trimmed)
            }
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            log::info!(
                "portal::load_token: no token file at {} (first run or after reset)",
                path.display()
            );
            None
        }
        Err(e) => {
            log::warn!(
                "portal::load_token: failed to read {}: {}",
                path.display(),
                e
            );
            None
        }
    }
}

pub fn save_token(token: &str) -> std::io::Result<()> {
    let path = token_path();
    std::fs::create_dir_all(path.parent().unwrap())?;
    std::fs::write(&path, token)?;
    log::info!(
        "portal::save_token: wrote {} bytes to {}",
        token.len(),
        path.display()
    );
    Ok(())
}

pub fn clear_token() -> std::io::Result<()> {
    let path = token_path();
    if path.exists() {
        std::fs::remove_file(path)
    } else {
        Ok(())
    }
}

/// Open the ScreenCast portal picker. Awaits user choice, returns the restore token
/// on success.
pub async fn pick_screencast_source() -> ashpd::Result<String> {
    log::info!(
        "portal::pick_screencast_source: opening ScreenCast portal with \
         PersistMode::ExplicitlyRevoked"
    );
    let proxy = Screencast::new().await?;
    let session = proxy.create_session().await?;
    proxy
        .select_sources(
            &session,
            CursorMode::Embedded,
            SourceType::Monitor.into(),
            false,
            None,
            PersistMode::ExplicitlyRevoked,
        )
        .await?;
    let response = proxy.start(&session, None).await?.response()?;
    match response.restore_token() {
        Some(t) if !t.is_empty() => {
            log::info!(
                "portal::pick_screencast_source: portal returned restore_token \
                 (len={})",
                t.len()
            );
            Ok(t.to_string())
        }
        Some(_) => {
            log::warn!(
                "portal::pick_screencast_source: portal returned an empty \
                 restore_token (Some(\"\")) — desktop may not fully support \
                 PersistMode"
            );
            Ok(String::new())
        }
        None => {
            log::warn!(
                "portal::pick_screencast_source: portal returned no \
                 restore_token (None) — desktop may not support PersistMode \
                 or rejected persistence for this consent"
            );
            Ok(String::new())
        }
    }
}

/// Capture a single frame from the current portal session for the "Test capture"
/// button. Uses the Screenshot portal which shares consent with ScreenCast.
pub async fn screenshot_current_target() -> ashpd::Result<PathBuf> {
    use ashpd::desktop::screenshot::Screenshot;
    let response = Screenshot::request()
        .interactive(false)
        .send()
        .await?
        .response()?;
    // ashpd 0.10 returns &url::Url from .uri(). Use to_file_path() which handles
    // URL-decoding properly; fall back to as_str() trim if to_file_path is unavailable
    // for the URL kind.
    let url = response.uri();
    if let Ok(p) = url.to_file_path() {
        return Ok(p);
    }
    let s = url.as_str();
    let path = s.strip_prefix("file://").unwrap_or(s);
    Ok(PathBuf::from(path))
}

#[cfg(test)]
mod token_tests {
    use super::*;

    #[test]
    fn token_round_trip() {
        let token = "test-token-12345";
        save_token(token).unwrap();
        assert_eq!(load_token().as_deref(), Some(token));
        clear_token().unwrap();
        assert!(load_token().is_none());
    }
}
