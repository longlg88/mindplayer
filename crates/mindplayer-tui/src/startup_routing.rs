//! Codex chooses in its native composer for each prompt, never at app startup.

use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::thread;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RoutingMode {
    Native,
    Jev,
    PerPrompt,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum StartupStage {
    Choice,
    Checking,
    Blocked,
    Complete,
}

pub(crate) struct StartupRouting {
    pub selection: RoutingMode,
    pub stage: StartupStage,
    pub error: Option<&'static str>,
    mode: Option<RoutingMode>,
    check: Option<Receiver<Result<(), &'static str>>>,
}

// Explicit legacy choice fixtures do not determine the app's initial mode.
#[cfg(test)]
impl Default for StartupRouting {
    fn default() -> Self {
        Self {
            selection: RoutingMode::Native,
            stage: StartupStage::Choice,
            error: None,
            mode: None,
            check: None,
        }
    }
}

impl StartupRouting {
    pub(crate) fn per_prompt() -> Self {
        Self {
            selection: RoutingMode::PerPrompt,
            stage: StartupStage::Complete,
            error: None,
            mode: Some(RoutingMode::PerPrompt),
            check: None,
        }
    }

    pub fn active(&self) -> bool {
        self.mode.is_none()
    }

    pub fn mode(&self) -> Option<RoutingMode> {
        self.mode
    }

    pub fn toggle(&mut self) {
        if self.stage == StartupStage::Choice {
            self.selection = match self.selection {
                RoutingMode::Native => RoutingMode::Jev,
                RoutingMode::Jev => RoutingMode::Native,
                RoutingMode::PerPrompt => RoutingMode::PerPrompt,
            };
            self.error = None;
        }
    }

    pub fn confirm(&mut self) {
        match self.stage {
            StartupStage::Choice if self.selection == RoutingMode::Native => {
                // In particular, do not inspect a Jev key or launch Node here.
                self.finish(RoutingMode::Native);
            }
            StartupStage::Choice => {
                // The choice sheet discloses external prompt transfer before
                // Enter. This explicit decision supplies consent for this run;
                // a successful local check completes it without another prompt.
                let (tx, rx) = mpsc::channel();
                self.check = Some(rx);
                self.stage = StartupStage::Checking;
                self.error = None;
                thread::spawn(move || {
                    let _ = tx.send(crate::local_router::check_startup());
                });
            }
            _ => {}
        }
    }

    fn finish(&mut self, mode: RoutingMode) {
        self.mode = Some(mode);
        self.stage = StartupStage::Complete;
        self.error = None;
        self.check = None;
    }

    fn finish_check(&mut self, result: Result<(), &'static str>) {
        if self.stage != StartupStage::Checking {
            return;
        }
        self.check = None;
        match result {
            Ok(()) => self.finish(RoutingMode::Jev),
            Err(reason) => {
                self.stage = StartupStage::Blocked;
                self.error = Some(reason);
            }
        }
    }

    /// The event loop remains responsive while local readiness is inspected.
    /// Success means local prerequisites only, never authentication or balance.
    pub fn poll(&mut self) -> bool {
        let Some(rx) = &self.check else {
            return false;
        };
        let result = match rx.try_recv() {
            Ok(result) => result,
            Err(TryRecvError::Empty) => return false,
            Err(TryRecvError::Disconnected) => Err("Jev 준비 확인을 완료하지 못했습니다."),
        };
        self.finish_check(result);
        true
    }

    pub fn back(&mut self) {
        if self.active() {
            // Dropping the receiver also discards a cancelled check's late result.
            self.check = None;
            self.stage = StartupStage::Choice;
            self.error = None;
        }
    }

    #[cfg(test)]
    pub(crate) fn confirmed(mode: RoutingMode) -> Self {
        let mut state = Self {
            selection: mode,
            ..Self::default()
        };
        state.finish(mode);
        state
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn per_prompt_is_ready_without_a_startup_choice_or_readiness_check() {
        let mut state = StartupRouting::per_prompt();
        assert_eq!(state.mode(), Some(RoutingMode::PerPrompt));
        assert_eq!(state.stage, StartupStage::Complete);
        assert!(!state.active());
        assert!(state.error.is_none());
        assert!(state.check.is_none());
        state.toggle();
        state.confirm();
        state.back();
        state.finish_check(Ok(()));
        assert!(!state.poll());
        assert_eq!(state.mode(), Some(RoutingMode::PerPrompt));
        assert!(state.check.is_none());
    }

    #[test]
    fn a_new_process_requires_a_choice_and_does_not_restore_the_last_one() {
        let mut native = StartupRouting::default();
        assert!(native.active());
        assert_eq!(native.mode(), None);
        native.confirm();
        assert_eq!(native.mode(), Some(RoutingMode::Native));
        assert_eq!(native.stage, StartupStage::Complete);
        assert!(native.check.is_none());
        assert!(StartupRouting::default().active());
    }

    #[test]
    fn local_readiness_does_not_authorize_external_transfer() {
        let mut state = StartupRouting::default();
        state.toggle();
        state.finish_check(Ok(()));
        assert!(state.active());
        assert_eq!(state.stage, StartupStage::Choice);
        assert_eq!(state.mode(), None);
    }

    #[test]
    fn one_jev_confirmation_finishes_when_readiness_succeeds_without_a_checkbox() {
        let mut state = StartupRouting::default();
        state.toggle();
        // Checking represents the explicit Jev + Enter decision. Feed a local
        // readiness receipt without reading credentials or launching Node.
        let (tx, rx) = mpsc::channel();
        state.stage = StartupStage::Checking;
        state.check = Some(rx);
        tx.send(Ok(())).unwrap();
        assert!(state.poll());
        assert_eq!(state.mode(), Some(RoutingMode::Jev));
        assert_eq!(state.stage, StartupStage::Complete);
        assert!(state.check.is_none());
        assert!(!state.poll());
        state.toggle();
        state.back();
        state.confirm();
        assert_eq!(state.mode(), Some(RoutingMode::Jev));
    }

    #[test]
    fn disconnected_readiness_remains_blocked_without_a_native_fallback() {
        let mut state = StartupRouting::default();
        state.toggle();
        let (tx, rx) = mpsc::channel();
        state.stage = StartupStage::Checking;
        state.check = Some(rx);
        drop(tx);
        assert!(state.poll());
        assert_eq!(state.stage, StartupStage::Blocked);
        assert_eq!(state.selection, RoutingMode::Jev);
        assert_eq!(state.mode(), None);
        assert!(state.error.is_some());
        state.confirm();
        assert!(state.active());
    }

    #[test]
    fn failure_and_cancel_never_silently_choose_native() {
        let mut state = StartupRouting::default();
        state.toggle();
        state.stage = StartupStage::Checking;
        state.finish_check(Err("Jev 키가 설정되어 있지 않습니다."));
        state.confirm();
        assert!(state.active());
        assert_eq!(state.stage, StartupStage::Blocked);
        state.back();
        assert_eq!(state.selection, RoutingMode::Jev);
        assert!(state.active());
        state.toggle();
        state.confirm();
        assert_eq!(state.mode(), Some(RoutingMode::Native));
    }

    #[test]
    fn cancelled_checks_cannot_change_a_later_native_choice() {
        let mut state = StartupRouting::default();
        state.toggle();
        let (tx, rx) = mpsc::channel();
        state.stage = StartupStage::Checking;
        state.check = Some(rx);
        state.back();
        state.toggle();
        state.confirm();
        assert!(tx.send(Ok(())).is_err());
        assert!(!state.poll());
        assert_eq!(state.mode(), Some(RoutingMode::Native));
    }
}
