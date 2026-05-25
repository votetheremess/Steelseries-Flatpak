//! src/clips/mod.rs — public entry for the clipping subsystem.

pub mod backend;
pub mod browser;
pub mod buffer;
pub mod gsr_install;
pub mod hotkey;
pub mod library;
pub mod notifications;
pub mod portal;
pub mod remix;
pub mod settings;
pub mod thumbnail;

pub use browser::{build_clips_page, ClipsPage, PageState, WizardStep};
pub use buffer::{BufferController, BufferState};

use std::path::PathBuf;

/// Commands sent from GTK main thread → backend thread.
#[derive(Debug)]
pub enum ClipCommand {
    /// Begin replay-mode capture with the given config.
    StartReplay { config: CaptureConfig },
    /// Stop and tear down the capture.
    StopReplay,
    /// Save the buffer (full duration).
    SaveClip,
    /// Save the buffer with a specific duration (uses GSR's SIGRTMIN+N for fixed durations).
    /// Falls back to SaveClip if duration doesn't match a supported value.
    SaveClipShort,  // 30 s
    SaveClipMedium, // 60 s
    SaveClipLong,   // 120 s
    /// Tear down current capture and reload with new config.
    Reconfigure { config: CaptureConfig },
    /// User pressed Pause: stop the active GSR replay session and clear the
    /// backend's auto-restart limiter. The buffer is lost (GSR SIGINT
    /// discards the in-memory ring). Distinct from StopReplay because Pause
    /// reflects user intent, not a state-machine transition.
    PauseRecording,
    /// User pressed Resume: clear the supervisor's restart-attempts limiter
    /// so a long-paused stretch doesn't count against the rolling window
    /// when the next StartReplay (already queued by BufferController::resume)
    /// arms a fresh session.
    ResumeRecording,
    /// Final shutdown (drives the thread to exit).
    Shutdown,
}

/// Events sent from backend thread → GTK main thread.
#[derive(Debug)]
pub enum BackendEvent {
    Armed,
    Disarmed,
    Saved { path: PathBuf, duration_ms: u64 },
    BackendDied { reason: String },
    Error { context: String, message: String },
}

/// Capture configuration. Built from settings + portal pick at arm time.
#[derive(Debug, Clone)]
pub struct CaptureConfig {
    pub buffer_secs: u32,
    pub bitrate_mbps: u32,
    pub framerate: u32,
    pub portal_restore_token: Option<String>,
    pub headset_sink_monitor: String,
    pub include_per_source_tracks: bool,
    pub include_mic: bool,
    pub output_dir: PathBuf,
}

impl Default for CaptureConfig {
    fn default() -> Self {
        Self {
            buffer_secs: 60,
            bitrate_mbps: 25,
            framerate: 60,
            portal_restore_token: None,
            headset_sink_monitor: String::new(),
            include_per_source_tracks: true,
            include_mic: true,
            output_dir: PathBuf::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capture_config_default_is_60s_25mbps_60fps() {
        let c = CaptureConfig::default();
        assert_eq!(c.buffer_secs, 60);
        assert_eq!(c.bitrate_mbps, 25);
        assert_eq!(c.framerate, 60);
        assert!(c.include_per_source_tracks);
        assert!(c.include_mic);
    }

    #[test]
    fn clip_command_variants_compile() {
        // Smoke test that each variant constructs.
        let _ = ClipCommand::SaveClip;
        let _ = ClipCommand::SaveClipShort;
        let _ = ClipCommand::SaveClipMedium;
        let _ = ClipCommand::SaveClipLong;
        let _ = ClipCommand::StopReplay;
        let _ = ClipCommand::Shutdown;
    }
}
