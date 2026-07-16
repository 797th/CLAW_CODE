//! Harness asset discovery: `.claw/{skills,commands,agents}/**.md`.
//!
//! Scans the project-level `.claw/` directory (rooted at a given `cwd`) and
//! the user-level `$CLAW_CONFIG_HOME` equivalent for the three harness asset
//! kinds — skills, commands, and agents — parsing YAML-ish frontmatter for
//! metadata. Project-level assets shadow user-level assets that share a name.
//!
//! Discovery never panics: a file with malformed or missing frontmatter is
//! skipped and a human-readable warning is collected onto the result instead.
//!
//! This module owns the canonical frontmatter parser for the workspace
//! (`parse_frontmatter_value`, below). `tools` depends on `runtime` (see
//! `crates/tools/Cargo.toml`), never the reverse, so there is no dependency
//! cycle in having `tools::parse_skill_name` call straight into
//! `runtime::harness_assets::parse_frontmatter_value` instead of keeping its
//! own copy — which is exactly what it now does.

use std::fs;
use std::path::{Path, PathBuf};

/// Discovered role for an agent asset, parsed from optional `role:`
/// frontmatter. Defaults to `Implementer` when absent or unrecognized.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AgentRole {
    #[default]
    Implementer,
    Reviewer,
    Gate,
}

impl AgentRole {
    fn parse(value: &str) -> Option<AgentRole> {
        match value.trim().to_ascii_lowercase().as_str() {
            "implementer" => Some(AgentRole::Implementer),
            "reviewer" => Some(AgentRole::Reviewer),
            "gate" => Some(AgentRole::Gate),
            _ => None,
        }
    }
}

/// Metadata for a discovered skill asset.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkillMeta {
    pub name: String,
    pub description: String,
    pub path: PathBuf,
}

/// Metadata for a discovered command asset.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandMeta {
    pub name: String,
    pub description: String,
    pub path: PathBuf,
    pub argument_hint: Option<String>,
}

/// Metadata for a discovered agent asset.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentMeta {
    pub name: String,
    pub description: String,
    pub path: PathBuf,
    pub role: AgentRole,
}

/// The full set of harness assets discovered for a workspace, plus any
/// warnings collected along the way (malformed frontmatter, unknown role
/// values, etc). Warnings are informational only — discovery never fails.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HarnessAssets {
    pub skills: Vec<SkillMeta>,
    pub commands: Vec<CommandMeta>,
    pub agents: Vec<AgentMeta>,
    pub warnings: Vec<String>,
}

/// The three asset kinds discovery understands. Each has its own
/// subdirectory name under a `.claw`-style root.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AssetKind {
    Skill,
    Command,
    Agent,
}

impl AssetKind {
    fn dir_name(self) -> &'static str {
        match self {
            AssetKind::Skill => "skills",
            AssetKind::Command => "commands",
            AssetKind::Agent => "agents",
        }
    }
}

/// A single frontmatter-bearing markdown file found on disk, tagged with
/// the precedence tier it came from (lower = higher precedence).
struct Candidate {
    path: PathBuf,
    tier: u8,
}

/// Discover all `.claw` harness assets visible from `cwd`.
///
/// Lookup precedence mirrors `skill_lookup_roots()` in `tools/src/lib.rs`:
/// project-level `.claw/` (searched from `cwd` upward through ancestors, in
/// place — the first, closest ancestor's `.claw` is highest precedence)
/// takes precedence over the user-level `$CLAW_CONFIG_HOME/.claw`-style
/// equivalent. When two assets of the same kind share a `name`, the
/// higher-precedence (project) one wins and the other is dropped silently
/// (not a warning — this is expected shadowing behavior, not malformed
/// input).
pub fn discover(cwd: &Path) -> HarnessAssets {
    let mut warnings = Vec::new();

    let mut skill_candidates = Vec::new();
    let mut command_candidates = Vec::new();
    let mut agent_candidates = Vec::new();

    // Tier 0: project-level `.claw` roots, nearest ancestor first.
    let mut tier = 0u8;
    for ancestor in cwd.ancestors() {
        let root = ancestor.join(".claw");
        if root.is_dir() {
            collect_kind(&root, AssetKind::Skill, tier, &mut skill_candidates);
            collect_kind(&root, AssetKind::Command, tier, &mut command_candidates);
            collect_kind(&root, AssetKind::Agent, tier, &mut agent_candidates);
            tier = tier.saturating_add(1);
        }
    }

    // Tier N: user-level `$CLAW_CONFIG_HOME/.claw` equivalent (lower
    // precedence than any project root found above).
    if let Ok(claw_config_home) = std::env::var("CLAW_CONFIG_HOME") {
        let root = PathBuf::from(claw_config_home);
        if root.is_dir() {
            collect_kind(&root, AssetKind::Skill, tier, &mut skill_candidates);
            collect_kind(&root, AssetKind::Command, tier, &mut command_candidates);
            collect_kind(&root, AssetKind::Agent, tier, &mut agent_candidates);
        }
    }

    let skills = resolve_skills(skill_candidates, &mut warnings);
    let commands = resolve_commands(command_candidates, &mut warnings);
    let agents = resolve_agents(agent_candidates, &mut warnings);

    HarnessAssets {
        skills,
        commands,
        agents,
        warnings,
    }
}

/// Walk `root/<kind_dir>` (bounded, non-recursive-symlink) and collect every
/// markdown asset file for `kind`, tagged with `tier`.
fn collect_kind(root: &Path, kind: AssetKind, tier: u8, out: &mut Vec<Candidate>) {
    let dir = root.join(kind.dir_name());
    if !dir.is_dir() {
        return;
    }
    // Canonicalize once so every candidate we accept can be checked against
    // the same boundary — mirrors the workspace-escape guard pattern used in
    // file_ops.rs (`canonicalize_workspace_root` + `validate_workspace_boundary`).
    let canonical_dir = dir.canonicalize().unwrap_or_else(|_| dir.clone());
    walk_bounded(&dir, &canonical_dir, 0, out, tier);
}

/// Maximum recursion depth for the bounded walk. Harness asset trees are
/// shallow (`skills/<name>/SKILL.md` or `skills/<name>.md`); this simply
/// guards against pathological/cyclic directory structures.
const MAX_WALK_DEPTH: usize = 8;

fn walk_bounded(
    dir: &Path,
    canonical_root: &Path,
    depth: usize,
    out: &mut Vec<Candidate>,
    tier: u8,
) {
    if depth > MAX_WALK_DEPTH {
        return;
    }
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(_) => return,
    };

    // `fs::read_dir` order is OS/filesystem-dependent. Sort each batch by
    // path so that walk order — and therefore which candidate "arrives
    // first" when two same-tier files resolve to the same asset name — is
    // deterministic across platforms and runs.
    let mut sorted_entries: Vec<_> = entries.flatten().map(|entry| entry.path()).collect();
    sorted_entries.sort();

    for path in sorted_entries {
        // Never follow symlinks: reject outright if the entry itself is a
        // symlink (whether to a file or a directory). This is stricter than
        // file_ops's "escape" check but is appropriate here since we don't
        // need to support intentional in-workspace symlinks for harness
        // assets, only prevent traversal outside the discovered root.
        let is_symlink = fs::symlink_metadata(&path)
            .map(|meta| meta.is_symlink())
            .unwrap_or(false);
        if is_symlink {
            continue;
        }

        // Defense in depth against `..`-style escapes: verify the resolved
        // path still lives under the canonical root before treating it as
        // part of this walk.
        if let Ok(canonical_path) = path.canonicalize() {
            if !canonical_path.starts_with(canonical_root) {
                continue;
            }
        } else {
            continue;
        }

        if path.is_dir() {
            walk_bounded(&path, canonical_root, depth + 1, out, tier);
        } else if path
            .extension()
            .is_some_and(|ext| ext.eq_ignore_ascii_case("md"))
        {
            out.push(Candidate { path, tier });
        }
    }
}

/// Parse a single frontmatter value (`key: value`) out of a markdown file's
/// leading `---`-delimited YAML-ish block. This is the canonical parser for
/// the workspace; `tools::parse_skill_name` calls straight into this
/// function rather than keeping its own copy (see module doc comment).
pub fn parse_frontmatter_value(contents: &str, key: &str) -> Option<String> {
    let mut lines = contents.lines();
    if lines.next().map(str::trim) != Some("---") {
        return None;
    }

    for line in lines {
        let trimmed = line.trim();
        if trimmed == "---" {
            break;
        }
        if let Some(value) = trimmed.strip_prefix(&format!("{key}:")) {
            let value = value
                .trim()
                .trim_matches(|ch| matches!(ch, '"' | '\''))
                .trim();
            if !value.is_empty() {
                return Some(value.to_string());
            }
        }
    }

    None
}

/// Returns `true` if the file has a well-formed `---`-delimited frontmatter
/// block at all (regardless of which keys it contains).
fn has_frontmatter_block(contents: &str) -> bool {
    let mut lines = contents.lines();
    if lines.next().map(str::trim) != Some("---") {
        return false;
    }
    lines.any(|line| line.trim() == "---")
}

/// Derive the display name for an asset file when no `name:` frontmatter
/// key is present: `skills/<name>/SKILL.md` -> `<name>`, `commands/<name>.md`
/// -> `<name>`.
fn implicit_name(path: &Path) -> Option<String> {
    let stem = path.file_stem()?.to_str()?;
    if stem.eq_ignore_ascii_case("SKILL") {
        // `skills/<name>/SKILL.md` form — the name is the parent dir.
        path.parent()?.file_name()?.to_str().map(|s| s.to_string())
    } else {
        Some(stem.to_string())
    }
}

fn read_and_check(path: &Path, warnings: &mut Vec<String>) -> Option<String> {
    match fs::read_to_string(path) {
        Ok(contents) => {
            if !has_frontmatter_block(&contents) {
                warnings.push(format!(
                    "skipping {}: missing or malformed frontmatter block",
                    path.display()
                ));
                return None;
            }
            Some(contents)
        }
        Err(error) => {
            warnings.push(format!("skipping {}: {error}", path.display()));
            None
        }
    }
}

/// Read, validate, and pull the common (`name`, `description`) fields shared
/// by every asset kind out of a candidate file. Returns `None` (after
/// pushing a warning) when the file is unreadable, has no frontmatter block,
/// or has no way to determine a name. On success, also hands back the raw
/// file contents so callers can pull kind-specific fields (`role`,
/// `argument-hint`) out of the same parse.
fn extract_common(
    candidate: &Candidate,
    warnings: &mut Vec<String>,
    kind_label: &str,
) -> Option<(String, String, String)> {
    let contents = read_and_check(&candidate.path, warnings)?;
    let name =
        parse_frontmatter_value(&contents, "name").or_else(|| implicit_name(&candidate.path));
    let Some(name) = name else {
        warnings.push(format!(
            "skipping {}: could not determine {kind_label} name",
            candidate.path.display()
        ));
        return None;
    };
    let description = parse_frontmatter_value(&contents, "description").unwrap_or_default();
    Some((name, description, contents))
}

fn resolve_skills(candidates: Vec<Candidate>, warnings: &mut Vec<String>) -> Vec<SkillMeta> {
    let mut store: Vec<(String, u8, PathBuf, SkillMeta)> = Vec::new();

    for candidate in candidates {
        let Some((name, description, _contents)) = extract_common(&candidate, warnings, "skill")
        else {
            continue;
        };
        upsert_by_precedence(
            &mut store,
            name.clone(),
            candidate.tier,
            candidate.path.clone(),
            SkillMeta {
                name,
                description,
                path: candidate.path,
            },
            warnings,
            "skill",
        );
    }

    store.into_iter().map(|(_, _, _, meta)| meta).collect()
}

fn resolve_commands(candidates: Vec<Candidate>, warnings: &mut Vec<String>) -> Vec<CommandMeta> {
    let mut store: Vec<(String, u8, PathBuf, CommandMeta)> = Vec::new();

    for candidate in candidates {
        let Some((name, description, contents)) = extract_common(&candidate, warnings, "command")
        else {
            continue;
        };
        let argument_hint = parse_frontmatter_value(&contents, "argument-hint")
            .or_else(|| parse_frontmatter_value(&contents, "argument_hint"));
        upsert_by_precedence(
            &mut store,
            name.clone(),
            candidate.tier,
            candidate.path.clone(),
            CommandMeta {
                name,
                description,
                path: candidate.path,
                argument_hint,
            },
            warnings,
            "command",
        );
    }

    store.into_iter().map(|(_, _, _, meta)| meta).collect()
}

fn resolve_agents(candidates: Vec<Candidate>, warnings: &mut Vec<String>) -> Vec<AgentMeta> {
    let mut store: Vec<(String, u8, PathBuf, AgentMeta)> = Vec::new();

    for candidate in candidates {
        let Some((name, description, contents)) = extract_common(&candidate, warnings, "agent")
        else {
            continue;
        };
        let role = match parse_frontmatter_value(&contents, "role") {
            None => AgentRole::default(),
            Some(raw) => match AgentRole::parse(&raw) {
                Some(role) => role,
                None => {
                    warnings.push(format!(
                        "{}: unknown role '{raw}', defaulting to implementer",
                        candidate.path.display()
                    ));
                    AgentRole::default()
                }
            },
        };
        upsert_by_precedence(
            &mut store,
            name.clone(),
            candidate.tier,
            candidate.path.clone(),
            AgentMeta {
                name,
                description,
                path: candidate.path,
                role,
            },
            warnings,
            "agent",
        );
    }

    store.into_iter().map(|(_, _, _, meta)| meta).collect()
}

/// Insert `meta` under `name`, keeping only the highest-precedence
/// (lowest-tier) entry when a name collides across tiers — that case is
/// expected shadowing (project over user) and is silent.
///
/// When two candidates collide on the *same* tier (e.g. both
/// `skills/foo.md` and `skills/foo/SKILL.md` resolve to name `foo`, or two
/// files declare the same `name:` frontmatter), the outcome would otherwise
/// depend on directory walk order. Walk order is already made deterministic
/// by sorting each `read_dir` batch (see `walk_bounded`), and here we apply
/// an explicit, deterministic tie-break on top of that: the
/// lexicographically-first path wins. The loser is dropped with a
/// collectable warning so the collision isn't silent.
fn upsert_by_precedence<T>(
    store: &mut Vec<(String, u8, PathBuf, T)>,
    name: String,
    tier: u8,
    path: PathBuf,
    meta: T,
    warnings: &mut Vec<String>,
    kind_label: &str,
) {
    if let Some(existing) = store
        .iter_mut()
        .find(|(existing_name, _, _, _)| *existing_name == name)
    {
        match tier.cmp(&existing.1) {
            std::cmp::Ordering::Less => {
                existing.1 = tier;
                existing.2 = path;
                existing.3 = meta;
            }
            std::cmp::Ordering::Equal => {
                let new_wins = path < existing.2;
                let kept_path = if new_wins { &path } else { &existing.2 };
                warnings.push(format!(
                    "{kind_label} name '{name}' is defined in both {} and {} at the same precedence tier; keeping {} (lexicographically first)",
                    existing.2.display(),
                    path.display(),
                    kept_path.display()
                ));
                if new_wins {
                    existing.1 = tier;
                    existing.2 = path;
                    existing.3 = meta;
                }
            }
            std::cmp::Ordering::Greater => {}
        }
        return;
    }
    store.push((name, tier, path, meta));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_simple_frontmatter_value() {
        let contents = "---\nname: foo\ndescription: bar baz\n---\nBody\n";
        assert_eq!(
            parse_frontmatter_value(contents, "name"),
            Some("foo".to_string())
        );
        assert_eq!(
            parse_frontmatter_value(contents, "description"),
            Some("bar baz".to_string())
        );
    }

    #[test]
    fn missing_frontmatter_block_returns_none() {
        let contents = "no frontmatter\nat all\n";
        assert_eq!(parse_frontmatter_value(contents, "name"), None);
        assert!(!has_frontmatter_block(contents));
    }

    #[test]
    fn agent_role_parse_is_case_insensitive() {
        assert_eq!(AgentRole::parse("Gate"), Some(AgentRole::Gate));
        assert_eq!(AgentRole::parse("REVIEWER"), Some(AgentRole::Reviewer));
        assert_eq!(
            AgentRole::parse("implementer"),
            Some(AgentRole::Implementer)
        );
        assert_eq!(AgentRole::parse("wizard"), None);
    }
}
