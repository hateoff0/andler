

use serde::{Deserialize, Serialize};

use crate::error::FsmError;


#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum InstanceState {

    Created,

    Starting,

    Running,

    Paused,

    Stopping,

    Stopped,

    Error { message: String },
}


#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum InstanceEvent {

    Start,

    StartCompleted,

    Pause,

    Resume,

    Stop,

    StopCompleted,

    Fail(String),
}

impl InstanceState {

    pub fn apply(self, event: InstanceEvent) -> Result<InstanceState, FsmError> {
        use InstanceEvent as E;
        use InstanceState as S;

        let from_for_error = self.clone();
        let event_for_error = event.clone();

        let next = match (self, event) {
            (S::Created, E::Start) => S::Starting,
            (S::Starting, E::StartCompleted) => S::Running,

            (S::Running, E::Pause) => S::Paused,
            (S::Paused, E::Resume) => S::Running,

            (S::Running, E::Stop) => S::Stopping,
            (S::Paused, E::Stop) => S::Stopping,
            (S::Starting, E::Stop) => S::Stopping,
            (S::Stopping, E::StopCompleted) => S::Stopped,

            (S::Stopped, E::Start) => S::Starting,
            (S::Error { .. }, E::Start) => S::Starting,

            (S::Created, E::Fail(message))
            | (S::Starting, E::Fail(message))
            | (S::Running, E::Fail(message))
            | (S::Paused, E::Fail(message))
            | (S::Stopping, E::Fail(message)) => S::Error { message },

            _ => {
                return Err(FsmError::InvalidTransition {
                    from: from_for_error,
                    event: event_for_error,
                })
            }
        };

        Ok(next)
    }


    pub fn is_terminal(&self) -> bool {
        matches!(self, InstanceState::Stopped | InstanceState::Error { .. })
    }


    pub fn is_disk_idle(&self) -> bool {
        matches!(
            self,
            InstanceState::Created | InstanceState::Stopped | InstanceState::Error { .. }
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn happy_path_start_pause_resume_stop() {
        let s = InstanceState::Created;
        let s = s.apply(InstanceEvent::Start).unwrap();
        assert_eq!(s, InstanceState::Starting);

        let s = s.apply(InstanceEvent::StartCompleted).unwrap();
        assert_eq!(s, InstanceState::Running);

        let s = s.apply(InstanceEvent::Pause).unwrap();
        assert_eq!(s, InstanceState::Paused);

        let s = s.apply(InstanceEvent::Resume).unwrap();
        assert_eq!(s, InstanceState::Running);

        let s = s.apply(InstanceEvent::Stop).unwrap();
        assert_eq!(s, InstanceState::Stopping);

        let s = s.apply(InstanceEvent::StopCompleted).unwrap();
        assert_eq!(s, InstanceState::Stopped);
        assert!(s.is_terminal());
    }

    #[test]
    fn cannot_resume_from_running() {
        let s = InstanceState::Running;
        let err = s.apply(InstanceEvent::Resume).unwrap_err();
        matches!(err, FsmError::InvalidTransition { .. });
    }

    #[test]
    fn cannot_pause_from_created() {
        let s = InstanceState::Created;
        assert!(s.apply(InstanceEvent::Pause).is_err());
    }

    #[test]
    fn fail_is_reachable_from_every_active_state() {
        for state in [
            InstanceState::Created,
            InstanceState::Starting,
            InstanceState::Running,
            InstanceState::Paused,
            InstanceState::Stopping,
        ] {
            let result = state.apply(InstanceEvent::Fail("boom".to_string())).unwrap();
            assert_eq!(
                result,
                InstanceState::Error {
                    message: "boom".to_string()
                }
            );
        }
    }

    #[test]
    fn terminal_states_accept_only_start_and_reject_everything_else() {
        let terminal_states = [
            InstanceState::Stopped,
            InstanceState::Error {
                message: "boom".to_string(),
            },
        ];
        for state in terminal_states {
            assert!(state.is_terminal());

            assert_eq!(
                state.clone().apply(InstanceEvent::Start).unwrap(),
                InstanceState::Starting,
                "expected {state:?} to accept Start (restart)"
            );

            let rejected_events = [
                InstanceEvent::StartCompleted,
                InstanceEvent::Pause,
                InstanceEvent::Resume,
                InstanceEvent::Stop,
                InstanceEvent::StopCompleted,
                InstanceEvent::Fail("boom".to_string()),
            ];
            for event in rejected_events {
                assert!(
                    state.clone().apply(event.clone()).is_err(),
                    "expected no transition from {state:?} on {event:?}"
                );
            }
        }
    }

    #[test]
    fn restarting_from_error_discards_the_previous_failure_message() {
        let errored = InstanceState::Error {
            message: "previous crash: out of memory".to_string(),
        };
        assert_eq!(
            errored.apply(InstanceEvent::Start).unwrap(),
            InstanceState::Starting
        );
    }
}
