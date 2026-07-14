//! Конечный автомат состояний инстанса.
//!
//! Состояния и переходы соответствуют операциям `HypervisorBackend`
//! (см. `backend.rs`): `spawn` -> `Starting`, успешный запуск -> `Running`,
//! `pause`/`resume` переключают `Running`/`Paused`, `stop` ведёт в `Stopping`
//! -> `Stopped`. Любая ошибка backend'а уводит в `Error`.
//!
//! `Stopped`/`Error` — не "конец" в смысле "эту запись больше нельзя
//! трогать": оба принимают `Start` и возвращаются в `Starting` — тот же
//! путь, что и заново созданный инстанс, но с уже существующим
//! `InstanceConfig`/`InstanceId` (см. `Daemon::start_instance`, которая
//! не делает ничего специфичного для `Created` — просто вызывает
//! `HypervisorBackend::spawn` с уже сохранённым конфигом, так что этот
//! путь работал бы для любого состояния с самого начала, если бы FSM это
//! разрешала). `is_terminal()` по-прежнему считает их терминальными — это
//! про "текущий запуск завершён", не про "исходящих переходов больше не
//! существует"; см. её doc-комментарий.
//!
//! Набор состояний соответствует §4.1 docs/architecture/CORE_ARCHITECTURE_PLAN.md.
//!
//! `andler-store` сохраняет текущее `InstanceState`, но сама логика
//! разрешённых переходов живёт здесь, а не в слое персистентности.

use serde::{Deserialize, Serialize};

use crate::error::FsmError;

/// Состояние инстанса в его жизненном цикле.
///
/// Не `Copy` — вариант `Error` несёт `String` с диагностикой backend'а,
/// поэтому переходы возвращают владеющее значение, а не ссылку/копию.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum InstanceState {
    /// `InstanceConfig` сохранён в `andler-store`, процесс ещё не запускался.
    Created,
    /// `HypervisorBackend::spawn` вызван, ждём подтверждения готовности.
    Starting,
    /// Инстанс работает.
    Running,
    /// Инстанс приостановлен (`HypervisorBackend::pause`).
    Paused,
    /// Запрошена остановка, ждём завершения процесса.
    Stopping,
    /// Процесс корректно завершён, ресурсы освобождены.
    Stopped,
    /// Backend вернул ошибку, из которой FSM не знает, как восстановиться
    /// автоматически сама. Можно перезапустить вручную (`Start` ->
    /// `Starting`, тот же путь, что у `Stopped`) — это не тупик,
    /// а сигнал "текущий запуск закончился неудачно", см. doc-комментарий
    /// модуля. `message` — диагностика причины (например, текст из
    /// `BackendError`/`BackendStatus::detail`).
    Error { message: String },
}

/// Событие, которое пытается перевести инстанс в новое состояние.
///
/// Соответствие методам `HypervisorBackend` сделано намеренно прямым:
/// каждый вызов backend'а в `andler-daemon` сопровождается ровно одним
/// событием FSM до и/или после результата.
///
/// Не `Copy` — `Fail` несёт диагностическое сообщение.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum InstanceEvent {
    /// Запрошен запуск (`HypervisorBackend::spawn` вызван).
    Start,
    /// Backend подтвердил, что инстанс запущен.
    StartCompleted,
    /// Запрошена пауза.
    Pause,
    /// Запрошено возобновление.
    Resume,
    /// Запрошена остановка.
    Stop,
    /// Backend подтвердил, что процесс завершён.
    StopCompleted,
    /// Backend вернул ошибку на любом этапе. `message` переносится в
    /// `InstanceState::Error { message }` при успешном переходе.
    Fail(String),
}

impl InstanceState {
    /// Применяет событие к текущему состоянию, возвращая новое состояние
    /// или ошибку, если переход не разрешён.
    ///
    /// Принимает `self` по значению (а не `&self`) и потребляет его —
    /// `InstanceState`/`InstanceEvent` не `Copy` из-за `String`-полей,
    /// поэтому вызывающая сторона явно передаёт владение: это отражает,
    /// что предыдущее состояние действительно перестаёт существовать
    /// после перехода (как и в `andler-store`, где хранится только
    /// текущее состояние, а не история).
    ///
    /// Это чистая функция без побочных эффектов: вызывающая сторона
    /// (`andler-daemon`) сама решает, что делать с результатом — например,
    /// сохранить новое состояние в `andler-store` или вызвать соответствующий
    /// метод `HypervisorBackend`.
    pub fn apply(self, event: InstanceEvent) -> Result<InstanceState, FsmError> {
        use InstanceEvent as E;
        use InstanceState as S;

        // `from`/`event` нужны и для успешного перехода (логирование на
        // стороне вызывающего), и для сообщения об ошибке, поэтому
        // клонируем перед match, а не пытаемся восстановить значение
        // из `FsmError` постфактум.
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

            // Restart: `Stopped`/`Error` accept `Start` the same as a
            // freshly-`Created` instance — see module doc comment for
            // why this doesn't need any change to `Daemon::start_instance`
            // itself (it was already generic over the source state; only
            // the FSM was refusing to let it through). `Error { .. }`
            // matches regardless of its `message` — the diagnostic text
            // of a past failure has no bearing on whether a retry is
            // allowed.
            (S::Stopped, E::Start) => S::Starting,
            (S::Error { .. }, E::Start) => S::Starting,

            // `Fail` разрешён из любого состояния, кроме уже финальных —
            // ошибка backend'а может произойти на любом активном этапе.
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

    /// `true` if the instance's *current run* has ended — `Stopped`
    /// (clean exit) or `Error` (failed). **Not** "no outgoing transitions
    /// exist at all": both accept `Start` and go back to `Starting` (see
    /// module doc comment) — this predicate is about whether the
    /// instance is presently active, e.g. for UI/status purposes, not
    /// about the shape of the transition graph. Doesn't include
    /// `Created`: an instance that has never run isn't "finished"
    /// anything, it just hasn't started yet.
    pub fn is_terminal(&self) -> bool {
        matches!(self, InstanceState::Stopped | InstanceState::Error { .. })
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

            // The one allowed way out: restart into a fresh Starting run.
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
        // Regardless of what the previous failure said, Start always
        // leads to a clean Starting — the old `message` doesn't leak
        // into the new run's state.
        let errored = InstanceState::Error {
            message: "previous crash: out of memory".to_string(),
        };
        assert_eq!(
            errored.apply(InstanceEvent::Start).unwrap(),
            InstanceState::Starting
        );
    }
}
