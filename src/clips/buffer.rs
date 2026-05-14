//! BufferController — state machine that maps detector + portal + hotkey events
//! into ClipCommand sends to the backend.

use std::sync::mpsc::Sender;

use crate::clips::{BackendEvent, CaptureConfig, ClipCommand, DetectedGame};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BufferState {
    /// No portal pick yet — waiting for user to set up capture source.
    Uninitialized,
    /// Portal pick exists but no game running.
    Idle,
    /// Sent StartReplay; awaiting Armed event.
    Arming,
    /// Backend reported Armed.
    Armed,
    /// Saving (sent SaveClip; awaiting Saved event).
    Saving,
    /// Backend reported error/death; user must retry.
    ErrorState,
    /// User-initiated stop; buffer is lost. The current rolling N-second
    /// clip is discarded when GSR receives SIGINT — there is no flush.
    /// Resume returns to Idle and re-evaluates the arm conditions.
    Paused,
}

pub struct BufferController {
    state: BufferState,
    /// Cached config builder. Updated as user changes settings.
    config: CaptureConfig,
    /// Whether auto-arm is enabled (default true).
    pub auto_arm: bool,
    /// Whether always-armed override is on (default false).
    pub always_armed: bool,
    /// Currently-detected game, if any.
    current_game: Option<DetectedGame>,
    /// Has the portal been picked? (Set externally after restore_token persists.)
    pub has_portal_pick: bool,
    /// If a config change arrived while saving, hold it and flush when `Saving → Armed`.
    pending_reconfigure: bool,
    /// User pressed Pause; suppresses auto-arm/always-armed until Resume.
    user_paused: bool,
}

impl BufferController {
    pub fn new(config: CaptureConfig) -> Self {
        Self {
            state: BufferState::Uninitialized,
            config,
            auto_arm: true,
            always_armed: false,
            current_game: None,
            has_portal_pick: false,
            pending_reconfigure: false,
            user_paused: false,
        }
    }

    pub fn state(&self) -> BufferState {
        self.state
    }

    pub fn current_game(&self) -> Option<&DetectedGame> {
        self.current_game.as_ref()
    }

    /// User completed portal pick. Transition Uninitialized → Idle (and possibly
    /// Arming if always_armed).
    pub fn on_portal_pick_complete(&mut self, restore_token: String, cmd_tx: &Sender<ClipCommand>) {
        self.config.portal_restore_token = Some(restore_token);
        self.has_portal_pick = true;
        if matches!(self.state, BufferState::Uninitialized) {
            self.state = BufferState::Idle;
            self.maybe_arm(cmd_tx);
        }
    }

    /// Detector reports a game started.
    pub fn on_game_started(&mut self, game: DetectedGame, cmd_tx: &Sender<ClipCommand>) {
        self.current_game = Some(game);
        self.maybe_arm(cmd_tx);
    }

    /// Detector reports the current game stopped.
    pub fn on_game_stopped(&mut self, pid: u32, cmd_tx: &Sender<ClipCommand>) {
        if self.current_game.as_ref().map(|g| g.pid) == Some(pid) {
            self.current_game = None;
            if !self.always_armed && matches!(self.state, BufferState::Armed) {
                let _ = cmd_tx.send(ClipCommand::StopReplay);
                self.state = BufferState::Idle;
            }
        }
    }

    /// User pressed the save hotkey.
    pub fn on_save_hotkey(&mut self, cmd_tx: &Sender<ClipCommand>) {
        if matches!(self.state, BufferState::Armed) {
            let _ = cmd_tx.send(ClipCommand::SaveClip);
            self.state = BufferState::Saving;
        }
    }

    /// User pressed a fixed-duration save hotkey. `duration_cmd` must be one
    /// of `ClipCommand::SaveClipShort/Medium/Long`. Behaves identically to
    /// `on_save_hotkey` but forwards the explicit variant so the backend can
    /// signal GSR with the matching `SIGRTMIN+N`.
    pub fn on_save_hotkey_duration(
        &mut self,
        duration_cmd: ClipCommand,
        cmd_tx: &Sender<ClipCommand>,
    ) {
        if matches!(self.state, BufferState::Armed) {
            let _ = cmd_tx.send(duration_cmd);
            self.state = BufferState::Saving;
        }
    }

    /// Backend event arrived from backend thread.
    pub fn on_backend_event(&mut self, evt: &BackendEvent, cmd_tx: &Sender<ClipCommand>) {
        match evt {
            BackendEvent::Armed => {
                if matches!(self.state, BufferState::Arming) {
                    self.state = BufferState::Armed;
                }
            }
            BackendEvent::Disarmed => {
                // Only Armed → Idle on disarm. A `Disarmed` arriving while we're `Arming`
                // is from a prior `StopReplay` whose `disarm()` is just now completing —
                // we're already mid-StartReplay for a new session and should ignore it.
                // Otherwise we'd transition Arming → Idle while the backend is actually
                // about to send `Armed` for the new session, leaving the controller stuck.
                if matches!(self.state, BufferState::Armed) {
                    self.state = BufferState::Idle;
                }
            }
            BackendEvent::Saved { .. } => {
                if matches!(self.state, BufferState::Saving) {
                    // If the game exited mid-save (no current_game) and we're not in
                    // always_armed mode, disarm. Otherwise return to Armed normally.
                    if self.current_game.is_none() && !self.always_armed {
                        let _ = cmd_tx.send(ClipCommand::StopReplay);
                        self.state = BufferState::Idle;
                        self.pending_reconfigure = false; // discard — would apply on next arm
                    } else {
                        self.state = BufferState::Armed;
                        if self.pending_reconfigure {
                            let _ = cmd_tx
                                .send(ClipCommand::Reconfigure { config: self.config.clone() });
                            self.pending_reconfigure = false;
                        }
                    }
                }
            }
            BackendEvent::BackendDied { .. } | BackendEvent::Error { .. } => {
                self.state = BufferState::ErrorState;
            }
        }
    }

    /// Settings change — rebuild config and reconfigure backend if armed.
    pub fn on_config_change(&mut self, new_config: CaptureConfig, cmd_tx: &Sender<ClipCommand>) {
        // Preserve restore_token if caller didn't provide one.
        let token = new_config
            .portal_restore_token
            .clone()
            .or_else(|| self.config.portal_restore_token.clone());
        self.config = CaptureConfig { portal_restore_token: token, ..new_config };
        match self.state {
            BufferState::Armed | BufferState::Arming => {
                let _ = cmd_tx.send(ClipCommand::Reconfigure { config: self.config.clone() });
            }
            BufferState::Saving => {
                // Defer until Saved arrives.
                self.pending_reconfigure = true;
            }
            _ => {} // Uninitialized / Idle / ErrorState — config will apply on next arm
        }
    }

    /// User-initiated retry after error.
    pub fn retry(&mut self, cmd_tx: &Sender<ClipCommand>) {
        if matches!(self.state, BufferState::ErrorState) {
            self.state = BufferState::Idle;
            self.maybe_arm(cmd_tx);
        }
    }

    /// Portal pick was reset by the user (Settings → Clips → Reset). Disarm
    /// if currently armed/arming/saving so we don't keep capturing a screen
    /// the user no longer wants, then return to Uninitialized so the next
    /// portal pick re-arms cleanly.
    pub fn on_portal_reset(&mut self, cmd_tx: &Sender<ClipCommand>) {
        if matches!(
            self.state,
            BufferState::Armed | BufferState::Arming | BufferState::Saving
        ) {
            let _ = cmd_tx.send(ClipCommand::StopReplay);
        }
        self.has_portal_pick = false;
        self.config.portal_restore_token = None;
        self.state = BufferState::Uninitialized;
    }

    fn maybe_arm(&mut self, cmd_tx: &Sender<ClipCommand>) {
        if !matches!(self.state, BufferState::Idle) {
            return;
        }
        if !self.should_arm() {
            return;
        }
        let _ = cmd_tx.send(ClipCommand::StartReplay { config: self.config.clone() });
        self.state = BufferState::Arming;
    }

    /// True if the buffer should be armed given the current toggles + portal
    /// state. Used by `maybe_arm` (which gates the transition Idle → Arming)
    /// and exposed for the Pause button's UI logic.
    ///
    /// `user_paused` is a hard gate: even when `always_armed` is on, an
    /// explicit user pause must suppress arming until they resume.
    pub fn should_arm(&self) -> bool {
        if !self.has_portal_pick {
            return false;
        }
        if self.user_paused {
            return false;
        }
        self.always_armed || (self.auto_arm && self.current_game.is_some())
    }

    /// User pressed Pause from the dashboard. Tear down the active replay
    /// session and remember the intent so re-arms are suppressed until the
    /// user explicitly resumes.
    ///
    /// Guard: pausing from Uninitialized / ErrorState would strand the user
    /// (no way back from Paused → those states via Resume). The dashboard
    /// button is insensitive in those states, but the GAction can still be
    /// activated via D-Bus; guard defensively here.
    pub fn pause(&mut self) {
        if matches!(self.state, BufferState::Uninitialized | BufferState::ErrorState) {
            return;
        }
        self.user_paused = true;
        // Drop any pending reconfigure: it would apply on the next arm, but
        // by the time the user resumes the cached config may already be
        // stale. Forcing a fresh reconfigure on resume keeps the path
        // simpler.
        self.pending_reconfigure = false;
        self.state = BufferState::Paused;
    }

    /// User pressed Resume. Clear the pause flag and re-evaluate arm
    /// conditions: if a game is running and auto_arm is on, transition
    /// straight to Arming rather than wait for the next on_game_started
    /// (which only fires on transitions and may never come).
    pub fn resume(&mut self, cmd_tx: &Sender<ClipCommand>) {
        self.user_paused = false;
        self.state = BufferState::Idle;
        self.maybe_arm(cmd_tx);
    }

    /// Read-only accessor for the user-paused flag. The dashboard's Pause
    /// button uses this to flip its label between "Pause Recording" and
    /// "Resume Recording".
    pub fn user_paused(&self) -> bool {
        self.user_paused
    }

    /// Direct setter for the user-paused flag. Primarily for tests; runtime
    /// callers should use `pause()` / `resume()` for the full transition.
    pub fn user_paused_set(&mut self, v: bool) {
        self.user_paused = v;
    }

    /// Testing seam: force a particular state without driving through the
    /// portal-pick → game-start → Armed sequence. Not for production callers.
    #[cfg(test)]
    pub fn set_state_for_test(&mut self, s: BufferState) {
        self.state = s;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc::channel;

    fn cfg() -> CaptureConfig {
        CaptureConfig::default()
    }

    fn dg(pid: u32) -> DetectedGame {
        DetectedGame { pid, name: "Test".into() }
    }

    #[test]
    fn starts_in_uninitialized() {
        let b = BufferController::new(cfg());
        assert_eq!(b.state(), BufferState::Uninitialized);
    }

    #[test]
    fn portal_pick_transitions_to_idle() {
        let (tx, _rx) = channel();
        let mut b = BufferController::new(cfg());
        b.on_portal_pick_complete("token".into(), &tx);
        assert_eq!(b.state(), BufferState::Idle);
    }

    #[test]
    fn game_start_arms_when_idle_and_auto_arm() {
        let (tx, rx) = channel();
        let mut b = BufferController::new(cfg());
        b.on_portal_pick_complete("token".into(), &tx);
        b.on_game_started(dg(42), &tx);
        assert_eq!(b.state(), BufferState::Arming);
        assert!(matches!(rx.try_recv(), Ok(ClipCommand::StartReplay { .. })));
    }

    #[test]
    fn game_start_does_not_arm_without_portal() {
        let (tx, rx) = channel();
        let mut b = BufferController::new(cfg());
        b.on_game_started(dg(42), &tx);
        assert_eq!(b.state(), BufferState::Uninitialized);
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn save_hotkey_only_works_in_armed() {
        let (tx, rx) = channel();
        let mut b = BufferController::new(cfg());
        b.on_save_hotkey(&tx);
        assert!(rx.try_recv().is_err(), "no command in non-Armed state");
    }

    #[test]
    fn save_hotkey_in_armed_sends_save_clip() {
        let (tx, rx) = channel();
        let mut b = BufferController::new(cfg());
        b.on_portal_pick_complete("t".into(), &tx);
        b.on_game_started(dg(42), &tx);
        let _ = rx.try_recv(); // consume StartReplay
        b.on_backend_event(&BackendEvent::Armed, &tx);
        assert_eq!(b.state(), BufferState::Armed);
        b.on_save_hotkey(&tx);
        assert_eq!(b.state(), BufferState::Saving);
        assert!(matches!(rx.try_recv(), Ok(ClipCommand::SaveClip)));
    }

    #[test]
    fn save_hotkey_duration_in_armed_sends_specific_variant() {
        let (tx, rx) = channel();
        let mut b = BufferController::new(cfg());
        b.on_portal_pick_complete("t".into(), &tx);
        b.on_game_started(dg(42), &tx);
        let _ = rx.try_recv(); // consume StartReplay
        b.on_backend_event(&BackendEvent::Armed, &tx);
        assert_eq!(b.state(), BufferState::Armed);
        b.on_save_hotkey_duration(ClipCommand::SaveClipMedium, &tx);
        assert_eq!(b.state(), BufferState::Saving);
        assert!(matches!(rx.try_recv(), Ok(ClipCommand::SaveClipMedium)));

        // Non-armed state ignores the duration variant just like the bare
        // save hotkey does.
        let (tx2, rx2) = channel();
        let mut b2 = BufferController::new(cfg());
        b2.on_save_hotkey_duration(ClipCommand::SaveClipShort, &tx2);
        assert!(rx2.try_recv().is_err());
    }

    #[test]
    fn always_armed_arms_on_portal_pick() {
        let (tx, rx) = channel();
        let mut b = BufferController::new(cfg());
        b.always_armed = true;
        b.on_portal_pick_complete("t".into(), &tx);
        assert_eq!(b.state(), BufferState::Arming);
        assert!(matches!(rx.try_recv(), Ok(ClipCommand::StartReplay { .. })));
    }

    #[test]
    fn disarmed_during_arming_is_ignored() {
        let (tx, _rx) = channel();
        let mut b = BufferController::new(cfg());
        b.on_portal_pick_complete("t".into(), &tx);
        b.on_game_started(dg(42), &tx);
        assert_eq!(b.state(), BufferState::Arming);
        // Stale Disarmed from a prior session arrives while we're Arming.
        b.on_backend_event(&BackendEvent::Disarmed, &tx);
        // Should still be Arming — wait for the actual Armed event.
        assert_eq!(b.state(), BufferState::Arming);
        b.on_backend_event(&BackendEvent::Armed, &tx);
        assert_eq!(b.state(), BufferState::Armed);
    }

    #[test]
    fn saved_after_game_exit_disarms() {
        let (tx, rx) = channel();
        let mut b = BufferController::new(cfg());
        b.on_portal_pick_complete("t".into(), &tx);
        b.on_game_started(dg(42), &tx);
        let _ = rx.try_recv(); // consume StartReplay
        b.on_backend_event(&BackendEvent::Armed, &tx);
        b.on_save_hotkey(&tx);
        let _ = rx.try_recv(); // consume SaveClip
        assert_eq!(b.state(), BufferState::Saving);
        // Game exits while we're saving (debounce window).
        b.on_game_stopped(42, &tx);
        // No StopReplay yet — state isn't Armed, predicate skipped.
        assert!(rx.try_recv().is_err());
        // Saved arrives. Now we should disarm because no current_game.
        b.on_backend_event(
            &BackendEvent::Saved {
                path: std::path::PathBuf::from("/tmp/clip.mp4"),
                duration_ms: 60_000,
            },
            &tx,
        );
        assert_eq!(b.state(), BufferState::Idle);
        assert!(matches!(rx.try_recv(), Ok(ClipCommand::StopReplay)));
    }

    #[test]
    fn portal_reset_returns_to_uninitialized() {
        let (tx, rx) = channel();
        let mut b = BufferController::new(cfg());
        b.on_portal_pick_complete("t".into(), &tx);
        b.on_game_started(dg(42), &tx);
        let _ = rx.try_recv(); // consume StartReplay
        b.on_backend_event(&BackendEvent::Armed, &tx);
        assert_eq!(b.state(), BufferState::Armed);
        b.on_portal_reset(&tx);
        assert_eq!(b.state(), BufferState::Uninitialized);
        assert!(matches!(rx.try_recv(), Ok(ClipCommand::StopReplay)));
    }

    #[test]
    fn config_change_during_saving_is_deferred() {
        let (tx, rx) = channel();
        let mut b = BufferController::new(cfg());
        b.on_portal_pick_complete("t".into(), &tx);
        b.on_game_started(dg(42), &tx);
        let _ = rx.try_recv(); // consume StartReplay
        b.on_backend_event(&BackendEvent::Armed, &tx);
        b.on_save_hotkey(&tx);
        let _ = rx.try_recv(); // consume SaveClip
        assert_eq!(b.state(), BufferState::Saving);
        // Settings change mid-save.
        let mut new_cfg = cfg();
        new_cfg.bitrate_mbps = 40;
        b.on_config_change(new_cfg, &tx);
        // Should NOT have sent Reconfigure yet.
        assert!(rx.try_recv().is_err());
        // Saved arrives.
        b.on_backend_event(
            &BackendEvent::Saved {
                path: std::path::PathBuf::from("/tmp/c.mp4"),
                duration_ms: 60_000,
            },
            &tx,
        );
        // State back to Armed (game still running).
        assert_eq!(b.state(), BufferState::Armed);
        // Reconfigure should have flushed.
        assert!(matches!(rx.try_recv(), Ok(ClipCommand::Reconfigure { .. })));
    }

    #[test]
    fn pause_from_armed_transitions_to_paused() {
        let mut bc = BufferController::new(CaptureConfig::default());
        bc.has_portal_pick = true;
        bc.set_state_for_test(BufferState::Armed);
        assert_eq!(bc.state(), BufferState::Armed);
        bc.pause();
        assert_eq!(bc.state(), BufferState::Paused);
        assert!(bc.user_paused());
    }

    #[test]
    fn resume_from_paused_to_idle() {
        use std::sync::mpsc::channel;
        let (tx, _rx) = channel();
        let mut bc = BufferController::new(CaptureConfig::default());
        bc.has_portal_pick = true;
        bc.set_state_for_test(BufferState::Idle);  // Move out of Uninitialized
        bc.pause();
        assert_eq!(bc.state(), BufferState::Paused);
        bc.resume(&tx);
        assert_eq!(bc.state(), BufferState::Idle);
        assert!(!bc.user_paused());
    }

    #[test]
    fn pause_clears_pending_reconfigure() {
        let mut bc = BufferController::new(CaptureConfig::default());
        bc.has_portal_pick = true;
        bc.set_state_for_test(BufferState::Armed);
        bc.pending_reconfigure = true;
        bc.pause();
        assert!(!bc.pending_reconfigure);
    }

    #[test]
    fn maybe_arm_respects_user_paused() {
        let mut bc = BufferController::new(CaptureConfig::default());
        bc.has_portal_pick = true;
        bc.always_armed = true;
        bc.user_paused_set(true);
        assert!(!bc.should_arm(), "user_paused must suppress always_armed");
        bc.user_paused_set(false);
        assert!(bc.should_arm(), "should_arm reflects always_armed when not paused");
    }

    #[test]
    fn resume_when_always_armed_re_arms_immediately() {
        use std::sync::mpsc::channel;
        let (tx, rx) = channel();
        let mut bc = BufferController::new(CaptureConfig::default());
        bc.has_portal_pick = true;
        bc.always_armed = true;
        bc.set_state_for_test(BufferState::Idle);  // Move out of Uninitialized
        bc.pause();
        assert_eq!(bc.state(), BufferState::Paused);
        bc.resume(&tx);
        assert_eq!(bc.state(), BufferState::Arming, "resume must re-evaluate maybe_arm");
        assert!(matches!(rx.try_recv(), Ok(ClipCommand::StartReplay { .. })));
    }

    #[test]
    fn pause_from_uninitialized_is_a_noop() {
        let mut bc = BufferController::new(CaptureConfig::default());
        // has_portal_pick = false → state stays Uninitialized
        bc.pause();
        assert_eq!(bc.state(), BufferState::Uninitialized, "pause must not strand the user");
        assert!(!bc.user_paused());
    }
}
