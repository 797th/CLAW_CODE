//! SAW-style workflow state machine.
//!
//! Pure, in-memory gate enforcement for the Spec -> Implement -> Verify ->
//! Review -> Done lifecycle a single CLI can verify without external
//! orchestration. No I/O happens here: callers own persistence (see
//! `session.rs`) and are responsible for calling [`WorkflowState::record_evidence`]
//! only when they have actually verified the claim (this module does not
//! parse or validate evidence content, e.g. test output, itself).

use serde::{Deserialize, Serialize};

/// The current phase of the workflow gate state machine.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkflowPhase {
    #[default]
    Idle,
    Spec,
    Implement,
    Verify,
    Review,
    Done,
}

/// A single piece of evidence recorded against a gate.
///
/// The recorder is trusted: `record_evidence` does not parse `detail` to
/// confirm a test run actually passed. Callers (e.g. the command layer in
/// Tasks 8/9) must only record `"test_run"` evidence after they have
/// observed a passing result, and `"review"` evidence after an actual human
/// or model review took place.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GateEvidence {
    pub gate: WorkflowPhase,
    pub kind: String,
    pub detail: String,
}

/// Result of attempting to advance the workflow to the next phase.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GateCheck {
    Pass,
    Blocked { reason: String },
}

/// Workflow state tracked per task/session.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct WorkflowState {
    pub phase: WorkflowPhase,
    /// Ticket/branch id the workflow is tracking.
    pub task_ref: Option<String>,
    /// Acceptance criteria; required to leave Spec. Never invented by the
    /// model -- if empty (or containing only whitespace-only entries), the
    /// model must ask the user for them. `try_advance` requires at least one
    /// entry whose trimmed content is non-empty.
    pub acceptance_criteria: Vec<String>,
    /// Evidence recorded against gates (e.g. test-run output refs, review
    /// approvals).
    pub evidence: Vec<GateEvidence>,
}

impl WorkflowState {
    /// Start a new task, moving Idle -> Spec and recording the task
    /// reference. This is the only way to leave `Idle`; `try_advance` from
    /// `Idle` always reports [`GateCheck::Blocked`].
    pub fn start(&mut self, task_ref: impl Into<String>) {
        self.phase = WorkflowPhase::Spec;
        self.task_ref = Some(task_ref.into());
    }

    /// Record evidence against a gate. The caller is responsible for
    /// ensuring the evidence is truthful (this function does not validate
    /// `detail`).
    pub fn record_evidence(&mut self, evidence: GateEvidence) {
        self.evidence.push(evidence);
    }

    /// Attempt to advance to the next phase, enforcing the gate rules.
    /// Returns [`GateCheck::Pass`] and mutates `self.phase` when the gate is
    /// satisfied; otherwise returns [`GateCheck::Blocked`] with a reason and
    /// leaves `self.phase` unchanged.
    pub fn try_advance(&mut self) -> GateCheck {
        match self.phase {
            WorkflowPhase::Idle => GateCheck::Blocked {
                reason: "start a task first (call start(task_ref)) before advancing the workflow"
                    .to_string(),
            },
            WorkflowPhase::Spec => {
                let has_real_ac = self
                    .acceptance_criteria
                    .iter()
                    .any(|ac| !ac.trim().is_empty());
                if !has_real_ac {
                    GateCheck::Blocked {
                        reason: "no acceptance criteria recorded; ask the user for acceptance \
                                 criteria before leaving Spec -- never invent them"
                            .to_string(),
                    }
                } else {
                    self.phase = WorkflowPhase::Implement;
                    GateCheck::Pass
                }
            }
            WorkflowPhase::Implement => {
                self.phase = WorkflowPhase::Verify;
                GateCheck::Pass
            }
            WorkflowPhase::Verify => {
                let has_passing_test_run = self.evidence.iter().any(|e| {
                    e.gate == WorkflowPhase::Verify
                        && e.kind == "test_run"
                        && !e.detail.trim().is_empty()
                });
                if has_passing_test_run {
                    self.phase = WorkflowPhase::Review;
                    GateCheck::Pass
                } else {
                    GateCheck::Blocked {
                        reason: "no passing test_run evidence recorded for Verify; run the \
                                 tests and record evidence before advancing to Review"
                            .to_string(),
                    }
                }
            }
            WorkflowPhase::Review => {
                let has_review_evidence = self
                    .evidence
                    .iter()
                    .any(|e| e.gate == WorkflowPhase::Review && e.kind == "review");
                if has_review_evidence {
                    self.phase = WorkflowPhase::Done;
                    GateCheck::Pass
                } else {
                    GateCheck::Blocked {
                        reason: "no review evidence recorded; a review must be recorded before \
                                 advancing to Done"
                            .to_string(),
                    }
                }
            }
            WorkflowPhase::Done => GateCheck::Blocked {
                reason: "workflow complete".to_string(),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_state_starts_idle_with_no_evidence() {
        let state = WorkflowState::default();
        assert_eq!(state.phase, WorkflowPhase::Idle);
        assert!(state.task_ref.is_none());
        assert!(state.acceptance_criteria.is_empty());
        assert!(state.evidence.is_empty());
    }
}
