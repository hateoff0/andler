use std::path::PathBuf;

use andler_core::{
    DaemonEvent, EventKind, InstanceId, OpId, Operation, OperationKind, OperationState,
};
use tokio::sync::{broadcast, watch};

use super::audit;

/// Progress handle handed to a long-running operation. Publishing goes to
/// the daemon-wide event bus (live consumers) and, for phase changes and
/// final states, to the per-instance events.jsonl audit trail — progress
/// ticks alone would drown the audit log without adding history.
pub(crate) struct OpProgress {
    op: Operation,
    phases: Vec<(String, f32)>,
    phase_index: usize,
    phase_progress: f32,
    events: broadcast::Sender<DaemonEvent>,
    audit_dir: PathBuf,
    cancel: watch::Receiver<bool>,
}

impl OpProgress {
    pub(crate) fn new(
        kind: OperationKind,
        instance_id: InstanceId,
        op_id: OpId,
        phases: Vec<(String, f32)>,
        events: broadcast::Sender<DaemonEvent>,
        audit_dir: PathBuf,
        cancel: watch::Receiver<bool>,
    ) -> Self {
        OpProgress {
            op: Operation::new(kind, instance_id, op_id),
            phase_index: 0,
            phase_progress: 0.0,
            phases,
            events,
            audit_dir,
            cancel,
        }
    }

    pub(crate) fn is_cancelled(&self) -> bool {
        *self.cancel.borrow()
    }

    /// Enters the named phase (must exist in `phases`); the phase change is
    /// broadcast and audited.
    pub(crate) fn enter_phase(&mut self, name: &str) {
        if let Some(index) = self.phases.iter().position(|(p, _)| p == name) {
            self.phase_index = index;
        }
        self.phase_progress = 0.0;
        self.op.progress = self.compute_progress();
        self.publish();
    }

    /// Sets progress within the current phase (0.0..=1.0); broadcast only.
    pub(crate) fn set_progress(&mut self, p: f32) {
        self.phase_progress = p.clamp(0.0, 1.0);
        self.op.progress = self.compute_progress();
        self.publish_broadcast();
    }

    /// Marks the operation running; broadcast + audit.
    pub(crate) fn mark_running(&mut self) {
        self.op.state = OperationState::Running;
        self.publish();
    }

    /// Final state from the runner's result; broadcast + audit. The error
    /// is carried as its Display text (the event shape has no typed error).
    pub(crate) fn finish(&mut self, result: Result<(), String>) {
        if self.is_cancelled() {
            self.op.state = OperationState::Cancelled;
        } else {
            match result {
                Ok(()) => {
                    self.op.state = OperationState::Done;
                    self.op.progress = 1.0;
                }
                Err(message) => {
                    self.op.state = OperationState::Failed;
                    self.op.error = Some(message);
                }
            }
        }
        self.publish();
    }

    fn compute_progress(&self) -> f32 {
        let done: f32 = self.phases[..self.phase_index.min(self.phases.len())]
            .iter()
            .map(|(_, weight)| *weight)
            .sum();
        let current = self
            .phases
            .get(self.phase_index)
            .map(|(_, w)| *w)
            .unwrap_or(0.0);
        (done + current * self.phase_progress).clamp(0.0, 1.0)
    }

    fn event(&self) -> DaemonEvent {
        DaemonEvent {
            ts_ms: chrono::Utc::now().timestamp_millis() as u64,
            instance_id: Some(self.op.instance_id),
            kind: EventKind::Operation {
                op: self.op.clone(),
            },
        }
    }

    fn publish(&self) {
        self.publish_broadcast();
        let event = self.event();
        let audit_dir = self.audit_dir.clone();
        tokio::spawn(async move {
            audit::append_event(&audit_dir, &event).await;
        });
    }

    fn publish_broadcast(&self) {
        let _ = self.events.send(self.event());
    }
}
