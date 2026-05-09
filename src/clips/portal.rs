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
    std::fs::read_to_string(token_path())
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

pub fn save_token(token: &str) -> std::io::Result<()> {
    let path = token_path();
    std::fs::create_dir_all(path.parent().unwrap())?;
    std::fs::write(path, token)
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
    let token = response
        .restore_token()
        .map(|s| s.to_string())
        .unwrap_or_default();
    Ok(token)
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
