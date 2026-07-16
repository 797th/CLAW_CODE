//! Embedded starter-pack templates for `clawcli init --harness` (Task 12).
//!
//! Each constant is the verbatim contents of one `.claw/` asset file, baked
//! into the binary via `include_str!` so the scaffolding works with no
//! network access and no extra install step. See `crate::init::initialize_harness`
//! for how these are written to disk (write-if-absent only — never
//! overwrites a file the user has already created or edited).

pub(crate) const QAS_AGENT: &str = include_str!("harness_templates/qas_agent.md");
pub(crate) const START_WORK_COMMAND: &str =
    include_str!("harness_templates/start_work_command.md");
pub(crate) const PRE_PR_COMMAND: &str = include_str!("harness_templates/pre_pr_command.md");
pub(crate) const END_WORK_COMMAND: &str = include_str!("harness_templates/end_work_command.md");
pub(crate) const PATTERN_DISCOVERY_SKILL: &str =
    include_str!("harness_templates/pattern_discovery_skill.md");
pub(crate) const VERIFICATION_BEFORE_COMPLETION_SKILL: &str =
    include_str!("harness_templates/verification_before_completion_skill.md");
