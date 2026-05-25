//! BufferController — state machine that maps portal + hotkey events
//! into ClipCommand sends to the backend.

use std::sync::mpsc::Sender;

use crate::clips::{BackendEvent, CaptureConfig, ClipCommand};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BufferState {
    /// No portal pick yet — waiting for user to set up capture source.
    Uninitialized,
    /// Portal pick exists but capture is not active (e.g., onboarding
    /// incomplete or backend transition window).
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
    /// Has the portal been picked? (Set externally after restore_token persists.)
    pub has_portal_pick: bool,
    /// Has the user finished the onboarding wizard? Seeded from
    /// `ClipSettings.onboarding_complete` at app startup; flipped to true
    /// by the wizard "finish" handler. Gates auto-arm so a mid-wizard
    /// user (token saved but never confirmed) doesn't auto-arm on every
    /// relaunch.
    onboarding_complete: bool,
    /// If a config change arrived while saving, hold it and flush when `Saving → Armed`.
    pending_reconfigure: bool,
    /// User pressed Pause; suppresses arming until Resume. Session-only;
    /// not persisted to `clips_settings.txt`. Resets to `false` on every
    /// launch — the user's stated intent is "always armed when the app
    /// opens up."
    user_paused: bool,
}

impl BufferController {
    pub fn new(config: CaptureConfig) -> Self {
        Self {
            state: BufferState::Uninitialized,
            config,
            has_portal_pick: false,
            onboarding_complete: false,
            pending_reconfigure: false,
            user_paused: false,
        }
    }

    pub fn state(&self) -> BufferState {
        self.state
    }

    /// Update the onboarding-complete gate. Called from the app startup
    /// auto-resume block (re-seeding from persisted settings) and from the
    /// wizard "finish" handler so a freshly-completed wizard arms without
    /// requiring an app restart. If the flag is being flipped to true and
    /// we're in Idle, also evaluate arming so the buffer engages
    /// immediately when the user clicks Done on the wizard.
    pub fn set_onboarding_complete(&mut self, v: bool, cmd_tx: &Sender<ClipCommand>) {
        let was = self.onboarding_complete;
        self.onboarding_complete = v;
        if !was && v {
            self.maybe_arm(cmd_tx);
        }
    }

    /// Read-only accessor for tests / introspection.
    pub fn onboarding_complete(&self) -> bool {
        self.onboarding_complete
    }

    /// User completed portal pick. Transition Uninitialized → Idle (and
    /// possibly Arming if the other gates are satisfied).
    pub fn on_portal_pick_complete(&mut self, restore_token: String, cmd_tx: &Sender<ClipCommand>) {
        self.config.portal_restore_token = Some(restore_token);
        self.has_portal_pick = true;
        if matches!(self.state, BufferState::Uninitialized) {
            self.state = BufferState::Idle;
            self.maybe_arm(cmd_tx);
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
                    // Re-check should_arm before transitioning back to Armed:
                    // the user may have clicked Pause while this Saved event
                    // was in flight, and without the re-check every save
                    // would force-rearm even when paused. (Critic Major 3.)
                    if self.should_arm() {
                        self.state = BufferState::Armed;
                        if self.pending_reconfigure {
                            let _ = cmd_tx
                                .send(ClipCommand::Reconfigure { config: self.config.clone() });
                            self.pending_reconfigure = false;
                        }
                    } else {
                        let _ = cmd_tx.send(ClipCommand::StopReplay);
                        // If paused, reflect that intent in the state.
                        self.state = if self.user_paused {
                            BufferState::Paused
                        } else {
                            BufferState::Idle
                        };
                        self.pending_reconfigure = false; // discard — would apply on next arm
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
    /// Always-armed model: arm whenever the portal token is present, the
    /// wizard is complete, and the user hasn't paused. `onboarding_complete`
    /// gates a mid-wizard user with a stale token from auto-arming on
    /// relaunch.
    pub fn should_arm(&self) -> bool {
        self.has_portal_pick && self.onboarding_complete && !self.user_paused
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
    /// conditions: if all gates are satisfied, transition straight to
    /// Arming.
    pub fn resume(&mut self, cmd_tx: &Sender<ClipCommand>) {
        self.user_paused = false;
        self.state = BufferState::Idle;
        self.maybe_arm(cmd_tx);
    }

    /// Read-only accessor for the user-paused flag. The dashboard's Pause
    /// button uses this to flip its label between "Capturing" and
    /// "Start Capturing".
    pub fn user_paused(&self) -> bool {
        self.user_paused
    }

    /// Direct setter for the user-paused flag. Primarily for tests; runtime
    /// callers should use `pause()` / `resume()` for the full transition.
    pub fn user_paused_set(&mut self, v: bool) {
        self.user_paused = v;
    }

    /// Testing seam: force a particular state without driving through the
    /// portal-pick → Armed sequence. Not for production callers.
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

    #[test]
    fn starts_in_uninitialized() {
        let b = BufferController::new(cfg());
        assert_eq!(b.state(), BufferState::Uninitialized);
    }

    #[test]
    fn portal_pick_transitions_to_idle_when_onboarding_incomplete() {
        let (tx, rx) = channel();
        let mut b = BufferController::new(cfg());
        // Default: onboarding_complete = false
        b.on_portal_pick_complete("token".into(), &tx);
        // Transitioned to Idle; should NOT have armed (onboarding not
        // complete → should_arm is false).
        assert_eq!(b.state(), BufferState::Idle);
        assert!(rx.try_recv().is_err(), "no StartReplay before onboarding finishes");
    }

    #[test]
    fn portal_pick_arms_when_onboarding_complete() {
        let (tx, rx) = channel();
        let mut b = BufferController::new(cfg());
        b.set_onboarding_complete(true, &tx);
        b.on_portal_pick_complete("token".into(), &tx);
        assert_eq!(b.state(), BufferState::Arming);
        assert!(matches!(rx.try_recv(), Ok(ClipCommand::StartReplay { .. })));
    }

    #[test]
    fn portal_pick_does_not_arm_when_onboarding_incomplete() {
        let (tx, rx) = channel();
        let mut b = BufferController::new(cfg());
        // onboarding_complete defaults to false — simulating a user
        // mid-wizard with a stale token re-attaching on relaunch.
        b.on_portal_pick_complete("token".into(), &tx);
        assert_eq!(b.state(), BufferState::Idle, "stays Idle until wizard finishes");
        assert!(rx.try_recv().is_err(), "no StartReplay sent");
    }

    #[test]
    fn set_onboarding_complete_arms_idle_buffer() {
        // The wizard "finish" handler flips onboarding_complete on a buffer
        // that's already attached to a portal token. The setter must
        // re-evaluate arming so the buffer engages without an app restart.
        let (tx, rx) = channel();
        let mut b = BufferController::new(cfg());
        b.on_portal_pick_complete("token".into(), &tx);
        assert_eq!(b.state(), BufferState::Idle);
        b.set_onboarding_complete(true, &tx);
        assert_eq!(b.state(), BufferState::Arming);
        assert!(matches!(rx.try_recv(), Ok(ClipCommand::StartReplay { .. })));
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
        b.set_onboarding_complete(true, &tx);
        b.on_portal_pick_complete("t".into(), &tx);
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
        b.set_onboarding_complete(true, &tx);
        b.on_portal_pick_complete("t".into(), &tx);
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
    fn disarmed_during_arming_is_ignored() {
        let (tx, _rx) = channel();
        let mut b = BufferController::new(cfg());
        b.set_onboarding_complete(true, &tx);
        b.on_portal_pick_complete("t".into(), &tx);
        assert_eq!(b.state(), BufferState::Arming);
        // Stale Disarmed from a prior session arrives while we're Arming.
        b.on_backend_event(&BackendEvent::Disarmed, &tx);
        // Should still be Arming — wait for the actual Armed event.
        assert_eq!(b.state(), BufferState::Arming);
        b.on_backend_event(&BackendEvent::Armed, &tx);
        assert_eq!(b.state(), BufferState::Armed);
    }

    #[test]
    fn portal_reset_returns_to_uninitialized() {
        let (tx, rx) = channel();
        let mut b = BufferController::new(cfg());
        b.set_onboarding_complete(true, &tx);
        b.on_portal_pick_complete("t".into(), &tx);
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
        b.set_onboarding_complete(true, &tx);
        b.on_portal_pick_complete("t".into(), &tx);
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
        // State back to Armed (always-armed → should_arm still true).
        assert_eq!(b.state(), BufferState::Armed);
        // Reconfigure should have flushed.
        assert!(matches!(rx.try_recv(), Ok(ClipCommand::Reconfigure { .. })));
    }

    #[test]
    fn save_during_pause_does_not_rearm() {
        // Race: user clicks Pause while a Saved event is in flight. The
        // Saved handler MUST re-check should_arm() before transitioning
        // back to Armed; without this, every save would force-rearm even
        // when the user just paused. (Critic Major 3.)
        let (tx, rx) = channel();
        let mut b = BufferController::new(cfg());
        b.set_onboarding_complete(true, &tx);
        b.on_portal_pick_complete("t".into(), &tx);
        let _ = rx.try_recv(); // consume StartReplay
        b.on_backend_event(&BackendEvent::Armed, &tx);
        b.on_save_hotkey(&tx);
        let _ = rx.try_recv(); // consume SaveClip
        assert_eq!(b.state(), BufferState::Saving);

        // User clicks Pause while the Saved event is in flight. Directly
        // setting the flag here simulates the race (in production
        // BufferController::pause() would force the state to Paused, but
        // here we want to keep the Saving state so we can observe the
        // Saved handler's gate). user_paused is the actual gate that
        // should_arm reads.
        b.user_paused_set(true);

        // Now the Saved event lands.
        b.on_backend_event(
            &BackendEvent::Saved {
                path: std::path::PathBuf::from("/tmp/c.mp4"),
                duration_ms: 60_000,
            },
            &tx,
        );
        // Must NOT rearm; should land in Paused (because user_paused is set).
        assert_eq!(b.state(), BufferState::Paused);
        assert!(matches!(rx.try_recv(), Ok(ClipCommand::StopReplay)));
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
        let (tx, _rx) = channel();
        let mut bc = BufferController::new(CaptureConfig::default());
        bc.has_portal_pick = true;
        bc.set_onboarding_complete(true, &tx);
        bc.user_paused_set(true);
        assert!(!bc.should_arm(), "user_paused must suppress arming");
        bc.user_paused_set(false);
        assert!(bc.should_arm(), "should_arm true when all gates satisfied");
    }

    #[test]
    fn resume_re_arms_immediately() {
        use std::sync::mpsc::channel;
        let (tx, rx) = channel();
        let mut bc = BufferController::new(CaptureConfig::default());
        bc.has_portal_pick = true;
        bc.set_onboarding_complete(true, &tx);
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

    /// Round-4 Bug B: ErrorState must recover via the pause-recording-toggle
    /// click path. The toggle handler in `app.rs` reads `state()` and dispatches
    /// to `retry()` when ErrorState is observed; this test exercises the same
    /// underlying transition that the handler triggers and asserts the buffer
    /// re-arms when the standard gates (portal pick + onboarding complete + not
    /// paused) are still satisfied.
    #[test]
    fn error_state_recovers_via_retry_and_re_arms() {
        let (tx, rx) = channel();
        let mut bc = BufferController::new(CaptureConfig::default());
        bc.has_portal_pick = true;
        bc.set_onboarding_complete(true, &tx);
        // Drain the auto-arm StartReplay from set_onboarding_complete.
        let _ = rx.try_recv();
        // Simulate the backend transitioning to ErrorState (e.g., GSR child
        // died during a save).
        bc.set_state_for_test(BufferState::ErrorState);
        assert_eq!(bc.state(), BufferState::ErrorState);
        // What the toggle handler does on click while in ErrorState.
        bc.retry(&tx);
        assert_eq!(
            bc.state(),
            BufferState::Arming,
            "retry must transition ErrorState → Idle → Arming when gates are satisfied"
        );
        assert!(
            matches!(rx.try_recv(), Ok(ClipCommand::StartReplay { .. })),
            "retry must enqueue StartReplay so the backend re-arms"
        );
    }
}
