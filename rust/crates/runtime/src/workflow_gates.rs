//! Task 9 — gate enforcement in the agent loop.
//!
//! Bridges the SAW-style [`WorkflowState`](crate::workflow::WorkflowState)
//! into the conversation loop's decision-merge points. Two gates fire:
//!
//! - **Stop-the-line (PreToolUse):** a file-writing tool while the workflow is
//!   still in `Spec` (acceptance criteria not yet confirmed) is blocked.
//! - **QAS (Stop):** finishing a turn while in `Verify` with no `test_run`
//!   evidence is blocked, reusing the Stop re-prompt machinery.
//!
//! The stop-the-line gate is expressed as a data-driven [`PolicyRule`] over a
//! [`LaneContext`] carrying the workflow phase, so future phase gates are added
//! as data. The QAS gate is evidence-based (it inspects recorded evidence,
//! which the `LaneContext` model does not carry) and is therefore evaluated
//! directly; see the report for the rationale.
//!
//! [`WorkflowGateMode`] governs behavior: `Off` (default) makes every function
//! here return `None`; `Advisory` surfaces a warning + context without
//! blocking; `Enforced` blocks. A workflow that is `None` or in `Idle` never
//! triggers a gate regardless of mode.

use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::config::WorkflowGateMode;
use crate::policy_engine::{
    DiffScope, LaneBlocker, LaneContext, PolicyAction, PolicyCondition, PolicyEngine, PolicyRule,
    ReviewStatus,
};
use crate::workflow::{WorkflowPhase, WorkflowState};

/// Schema tag for `gate_check` audit events, following the `claw.<name>.vN`
/// convention used by `report_schema` / `lane_events`.
pub const GATE_CHECK_SCHEMA: &str = "claw.gate_check.v1";

/// Wire event name for the audit record.
pub const GATE_CHECK_EVENT: &str = "gate_check";

/// Reason surfaced when the stop-the-line gate fires.
pub const STOP_THE_LINE_REASON: &str =
    "Stop-the-line: acceptance criteria not confirmed; record them via /workflow or ask the user.";

/// Reason surfaced when the QAS gate fires.
pub const QAS_GATE_REASON: &str =
    "QAS gate: run the test suite and record evidence before finishing.";

const SPEC_GATE_RULE: &str = "stop-the-line-spec";
const QAS_GATE_RULE: &str = "qas-verify-evidence";

/// Whether a gate decision blocked progress or was merely advisory.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GateDecision {
    /// Progress was blocked (mode `Enforced`).
    Block,
    /// Progress allowed; a warning + context were surfaced (mode `Advisory`).
    Advisory,
}

/// Auditable NDJSON record for a single gate decision.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GateCheckEvent {
    /// Schema tag ([`GATE_CHECK_SCHEMA`]).
    pub schema: String,
    /// Wire event name ([`GATE_CHECK_EVENT`]).
    pub event: String,
    /// Workflow phase the gate observed.
    pub phase: WorkflowPhase,
    /// Name of the gate rule that fired.
    pub rule: String,
    /// Whether the decision blocked or was advisory.
    pub decision: GateDecision,
    /// Human-facing reason string.
    pub reason: String,
}

impl GateCheckEvent {
    #[must_use]
    fn new(
        phase: WorkflowPhase,
        rule: &str,
        decision: GateDecision,
        reason: impl Into<String>,
    ) -> Self {
        Self {
            schema: GATE_CHECK_SCHEMA.to_string(),
            event: GATE_CHECK_EVENT.to_string(),
            phase,
            rule: rule.to_string(),
            decision,
            reason: reason.into(),
        }
    }
}

/// Outcome of evaluating a gate. Callers block when `blocking` is set (mode
/// `Enforced`) or surface `reason` as a warning/context otherwise (mode
/// `Advisory`). `event` is always emitted for audit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GateOutcome {
    /// Whether the caller should block on this decision.
    pub blocking: bool,
    /// Reason to surface (block reason or advisory context).
    pub reason: String,
    /// Audit record for this decision.
    pub event: GateCheckEvent,
}

/// File-writing tools that the stop-the-line gate guards. Matches both the
/// canonical registry names and the capitalized aliases the model emits.
///
/// `Bash`/`bash` is treated as file-writing wholesale — even a read-only shell
/// command is gated in `Spec`. This is intentional: `Bash` is an opaque
/// mutation surface (it can write files, run `git`, etc.) and the gate cannot
/// cheaply prove a command is side-effect-free, so it fails closed until
/// acceptance criteria are confirmed.
#[must_use]
pub fn is_file_writing_tool(tool_name: &str) -> bool {
    matches!(
        tool_name.trim(),
        "Write"
            | "write_file"
            | "write"
            | "Edit"
            | "edit_file"
            | "edit"
            | "multi_edit"
            | "MultiEdit"
            | "Bash"
            | "bash"
    )
}

/// Data-driven engine for phase gates evaluated at PreToolUse time. Currently
/// one rule: block file-writing tools while in `Spec`. Add future phase gates
/// here as additional [`PolicyRule`]s.
#[must_use]
fn spec_gate_engine() -> PolicyEngine {
    PolicyEngine::new(vec![PolicyRule::new(
        SPEC_GATE_RULE,
        PolicyCondition::WorkflowPhaseIs(WorkflowPhase::Spec),
        PolicyAction::Block {
            reason: STOP_THE_LINE_REASON.to_string(),
        },
        0,
    )])
}

fn gate_lane_context(phase: WorkflowPhase) -> LaneContext {
    LaneContext::new(
        "workflow-gate",
        0,
        Duration::from_secs(0),
        LaneBlocker::None,
        ReviewStatus::Pending,
        DiffScope::Full,
        false,
    )
    .with_workflow_phase(phase)
}

/// Evaluate the stop-the-line PreToolUse gate for `tool_name`. Returns `None`
/// when the gate does not apply (mode `Off`, no workflow, `Idle`, non-file
/// tool, or a phase past `Spec`).
#[must_use]
pub fn evaluate_pre_tool_use_gate(
    mode: WorkflowGateMode,
    workflow: Option<&WorkflowState>,
    tool_name: &str,
) -> Option<GateOutcome> {
    if mode == WorkflowGateMode::Off {
        return None;
    }
    let phase = workflow.map(|workflow| workflow.phase)?;
    if phase == WorkflowPhase::Idle {
        return None;
    }
    if !is_file_writing_tool(tool_name) {
        return None;
    }

    let engine = spec_gate_engine();
    let context = gate_lane_context(phase);
    let reason = engine.evaluate(&context).into_iter().find_map(|action| {
        if let PolicyAction::Block { reason } = action {
            Some(reason)
        } else {
            None
        }
    })?;

    Some(build_outcome(mode, phase, SPEC_GATE_RULE, reason))
}

/// Evaluate the QAS Stop gate. Returns `None` when it does not apply (mode
/// `Off`, no workflow, not in `Verify`, or `test_run` evidence already
/// recorded).
#[must_use]
pub fn evaluate_stop_gate(
    mode: WorkflowGateMode,
    workflow: Option<&WorkflowState>,
) -> Option<GateOutcome> {
    if mode == WorkflowGateMode::Off {
        return None;
    }
    let workflow = workflow?;
    if workflow.phase != WorkflowPhase::Verify {
        return None;
    }
    let has_test_run = workflow.evidence.iter().any(|evidence| {
        evidence.gate == WorkflowPhase::Verify
            && evidence.kind == "test_run"
            && !evidence.detail.trim().is_empty()
    });
    if has_test_run {
        return None;
    }

    Some(build_outcome(
        mode,
        WorkflowPhase::Verify,
        QAS_GATE_RULE,
        QAS_GATE_REASON.to_string(),
    ))
}

fn build_outcome(
    mode: WorkflowGateMode,
    phase: WorkflowPhase,
    rule: &str,
    reason: String,
) -> GateOutcome {
    let blocking = mode == WorkflowGateMode::Enforced;
    let decision = if blocking {
        GateDecision::Block
    } else {
        GateDecision::Advisory
    };
    GateOutcome {
        blocking,
        event: GateCheckEvent::new(phase, rule, decision, reason.clone()),
        reason,
    }
}

#[cfg(test)]
mod tests {
    use super::{
        evaluate_pre_tool_use_gate, evaluate_stop_gate, GateDecision, GATE_CHECK_EVENT,
        GATE_CHECK_SCHEMA, QAS_GATE_REASON, STOP_THE_LINE_REASON,
    };
    use crate::config::WorkflowGateMode;
    use crate::workflow::{GateEvidence, WorkflowPhase, WorkflowState};

    fn spec_workflow() -> WorkflowState {
        let mut state = WorkflowState::default();
        state.start("TASK-9");
        state
    }

    fn verify_workflow() -> WorkflowState {
        let mut state = spec_workflow();
        state.acceptance_criteria.push("AC1".to_string());
        state.try_advance(); // Spec -> Implement
        state.try_advance(); // Implement -> Verify
        assert_eq!(state.phase, WorkflowPhase::Verify);
        state
    }

    #[test]
    fn off_mode_never_triggers() {
        let workflow = spec_workflow();
        assert!(
            evaluate_pre_tool_use_gate(WorkflowGateMode::Off, Some(&workflow), "Write").is_none()
        );
        let verify = verify_workflow();
        assert!(evaluate_stop_gate(WorkflowGateMode::Off, Some(&verify)).is_none());
    }

    #[test]
    fn no_workflow_or_idle_never_triggers() {
        assert!(evaluate_pre_tool_use_gate(WorkflowGateMode::Enforced, None, "Write").is_none());
        let idle = WorkflowState::default();
        assert!(
            evaluate_pre_tool_use_gate(WorkflowGateMode::Enforced, Some(&idle), "Write").is_none()
        );
        assert!(evaluate_stop_gate(WorkflowGateMode::Enforced, Some(&idle)).is_none());
    }

    #[test]
    fn enforced_spec_gate_blocks_file_writes_only() {
        let workflow = spec_workflow();
        let outcome =
            evaluate_pre_tool_use_gate(WorkflowGateMode::Enforced, Some(&workflow), "Write")
                .expect("write in Spec should trigger the gate");
        assert!(outcome.blocking);
        assert_eq!(outcome.reason, STOP_THE_LINE_REASON);
        assert_eq!(outcome.event.decision, GateDecision::Block);

        // Non-file tool: no gate.
        assert!(evaluate_pre_tool_use_gate(
            WorkflowGateMode::Enforced,
            Some(&workflow),
            "read_file"
        )
        .is_none());
    }

    #[test]
    fn advisory_spec_gate_does_not_block() {
        let workflow = spec_workflow();
        let outcome =
            evaluate_pre_tool_use_gate(WorkflowGateMode::Advisory, Some(&workflow), "edit_file")
                .expect("edit in Spec should trigger advisory gate");
        assert!(!outcome.blocking);
        assert_eq!(outcome.event.decision, GateDecision::Advisory);
    }

    #[test]
    fn spec_gate_does_not_fire_past_spec() {
        let verify = verify_workflow();
        assert!(
            evaluate_pre_tool_use_gate(WorkflowGateMode::Enforced, Some(&verify), "Write")
                .is_none()
        );
    }

    #[test]
    fn qas_gate_blocks_verify_without_evidence_and_clears_with_it() {
        let mut workflow = verify_workflow();
        let outcome = evaluate_stop_gate(WorkflowGateMode::Enforced, Some(&workflow))
            .expect("Verify without evidence should trigger QAS gate");
        assert!(outcome.blocking);
        assert_eq!(outcome.reason, QAS_GATE_REASON);

        workflow.record_evidence(GateEvidence {
            gate: WorkflowPhase::Verify,
            kind: "test_run".to_string(),
            detail: "42 passed".to_string(),
        });
        assert!(evaluate_stop_gate(WorkflowGateMode::Enforced, Some(&workflow)).is_none());
    }

    #[test]
    fn gate_event_carries_schema_consistent_fields() {
        let workflow = spec_workflow();
        let outcome =
            evaluate_pre_tool_use_gate(WorkflowGateMode::Enforced, Some(&workflow), "Bash")
                .expect("bash in Spec should trigger the gate");
        let json = serde_json::to_value(&outcome.event).expect("gate event serializes");
        assert_eq!(json["schema"], GATE_CHECK_SCHEMA);
        assert_eq!(json["event"], GATE_CHECK_EVENT);
        assert_eq!(json["phase"], "spec");
        assert_eq!(json["rule"], "stop-the-line-spec");
        assert_eq!(json["decision"], "block");
    }
}
