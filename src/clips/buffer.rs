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

    /// Backend event arrived from backend thread.
    pub fn on_backend_event(&mut self, evt: &BackendEvent, _cmd_tx: &Sender<ClipCommand>) {
        match evt {
            BackendEvent::Armed => {
                if matches!(self.state, BufferState::Arming) {
                    self.state = BufferState::Armed;
                }
            }
            BackendEvent::Disarmed => {
                if matches!(self.state, BufferState::Armed | BufferState::Arming) {
                    self.state = BufferState::Idle;
                }
            }
            BackendEvent::Saved { .. } => {
                if matches!(self.state, BufferState::Saving) {
                    self.state = BufferState::Armed;
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
        if matches!(self.state, BufferState::Armed | BufferState::Arming) {
            let _ = cmd_tx.send(ClipCommand::Reconfigure { config: self.config.clone() });
        }
    }

    /// User-initiated retry after error.
    pub fn retry(&mut self, cmd_tx: &Sender<ClipCommand>) {
        if matches!(self.state, BufferState::ErrorState) {
            self.state = BufferState::Idle;
            self.maybe_arm(cmd_tx);
        }
    }

    fn maybe_arm(&mut self, cmd_tx: &Sender<ClipCommand>) {
        if !self.has_portal_pick {
            return;
        }
        if !matches!(self.state, BufferState::Idle) {
            return;
        }
        let should_arm = self.always_armed || (self.auto_arm && self.current_game.is_some());
        if should_arm {
            let _ = cmd_tx.send(ClipCommand::StartReplay { config: self.config.clone() });
            self.state = BufferState::Arming;
        }
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
    fn always_armed_arms_on_portal_pick() {
        let (tx, rx) = channel();
        let mut b = BufferController::new(cfg());
        b.always_armed = true;
        b.on_portal_pick_complete("t".into(), &tx);
        assert_eq!(b.state(), BufferState::Arming);
        assert!(matches!(rx.try_recv(), Ok(ClipCommand::StartReplay { .. })));
    }
}
