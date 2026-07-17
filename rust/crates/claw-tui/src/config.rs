//! Small config bridge used while the TUI is still a standalone application.
//!
//! The production CLI stores model aliases in JSON and `/login` credentials in
//! an owner-only `.env`. This module mirrors those keys and locations so the
//! standalone TUI behaves predictably before the runtime is wired in.

use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde_json::{Map, Value};

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProviderConfig {
    pub kind: Option<String>,
    pub api_key: Option<String>,
    pub base_url: Option<String>,
    pub model: Option<String>,
}

#[must_use]
pub fn load_provider() -> ProviderConfig {
    let root = load_merged_settings();
    let provider = root.get("provider").and_then(Value::as_object);
    let dotenv = load_dotenv_values(&config_home().join(".env"));
    let kind = nonempty_env("CLAW_ENDPOINT_TYPE")
        .or_else(|| dotenv.get("CLAW_ENDPOINT_TYPE").cloned())
        .and_then(|value| match value.as_str() {
            "openai-compatible" => Some("openai-compatible".to_string()),
            "anthropic-compatible" => Some("anthropic-compatible".to_string()),
            _ => None,
        })
        .or_else(|| {
            provider
                .and_then(|value| value.get("kind"))
                .and_then(Value::as_str)
                .map(ToOwned::to_owned)
        });
    let credential_key = kind
        .as_deref()
        .map_or("OPENAI_API_KEY", credential_env_for_kind);
    let base_url_key = kind
        .as_deref()
        .map_or("OPENAI_BASE_URL", base_url_env_for_kind);
    ProviderConfig {
        kind,
        api_key: provider
            .and_then(|value| value.get("apiKey"))
            .and_then(Value::as_str)
            .map(ToOwned::to_owned),
        base_url: provider
            .and_then(|value| value.get("baseUrl"))
            .and_then(Value::as_str)
            .map(ToOwned::to_owned),
        model: root
            .get("model")
            .and_then(Value::as_str)
            .map(ToOwned::to_owned)
            .or_else(|| {
                provider
                    .and_then(|value| value.get("model"))
                    .and_then(Value::as_str)
                    .map(ToOwned::to_owned)
            }),
    }
    .with_dotenv_fallback(&dotenv, credential_key, base_url_key)
}

impl ProviderConfig {
    fn with_dotenv_fallback(
        mut self,
        dotenv: &BTreeMap<String, String>,
        credential_key: &str,
        base_url_key: &str,
    ) -> Self {
        self.api_key = nonempty_env(credential_key)
            .or_else(|| dotenv.get(credential_key).cloned())
            .or_else(|| nonempty_value(self.api_key));
        self.base_url = nonempty_env(base_url_key)
            .or_else(|| dotenv.get(base_url_key).cloned())
            .or_else(|| nonempty_value(self.base_url));
        self
    }
}

#[must_use]
pub fn configured_model() -> Option<String> {
    [
        "CLAW_MODEL",
        "NVIDIA_NIM_MODEL",
        "OPENAI_MODEL",
        "ANTHROPIC_MODEL",
        "ANTHROPIC_DEFAULT_MODEL",
    ]
    .into_iter()
    .find_map(|name| {
        std::env::var(name)
            .ok()
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())
    })
    .or_else(|| load_provider().model)
}

#[must_use]
pub fn load_aliases() -> BTreeMap<String, String> {
    load_merged_settings()
        .get("aliases")
        .and_then(Value::as_object)
        .map(|aliases| {
            aliases
                .iter()
                .filter_map(|(name, value)| {
                    value
                        .as_str()
                        .map(|model| (name.clone(), model.to_string()))
                })
                .collect()
        })
        .unwrap_or_default()
}

pub fn save_provider(
    kind: &str,
    api_key: &str,
    base_url: Option<&str>,
    model: Option<&str>,
) -> io::Result<PathBuf> {
    let path = settings_path();
    let mut root = read_object(&path);
    let mut provider = Map::new();
    provider.insert("kind".to_string(), Value::String(kind.to_string()));
    provider.insert("apiKey".to_string(), Value::String(api_key.to_string()));
    if let Some(base_url) = base_url.filter(|value| !value.is_empty()) {
        provider.insert("baseUrl".to_string(), Value::String(base_url.to_string()));
    }
    root.insert("provider".to_string(), Value::Object(provider));
    if let Some(model) = model.filter(|value| !value.is_empty()) {
        root.insert("model".to_string(), Value::String(model.to_string()));
    }
    write_object(&path, root)?;
    Ok(path)
}

/// Persist the production `/login` connection shape: the model is written to
/// `settings.json`, while the endpoint and API key are written to the
/// owner-only `.env` file used by the runtime.
pub fn save_login(
    protocol: &str,
    api_key: &str,
    base_url: &str,
    model: &str,
) -> io::Result<PathBuf> {
    let settings = settings_path();
    let mut root = read_object(&settings);
    if !model.trim().is_empty() {
        root.insert("model".to_string(), Value::String(model.trim().to_string()));
    }
    write_object(&settings, root)?;

    let (base_url_key, api_key_key) = match protocol {
        "anthropic-compatible" => ("ANTHROPIC_BASE_URL", "ANTHROPIC_API_KEY"),
        _ => ("OPENAI_BASE_URL", "OPENAI_API_KEY"),
    };
    let dotenv = config_home().join(".env");
    upsert_dotenv(
        &dotenv,
        &[
            ("CLAW_ENDPOINT_TYPE", protocol),
            (base_url_key, base_url),
            (api_key_key, api_key),
        ],
    )?;
    Ok(dotenv)
}

pub fn clear_provider() -> io::Result<()> {
    let path = settings_path();
    if path.exists() {
        let mut root = read_object(&path);
        root.remove("provider");
        root.remove("model");
        write_object(&path, root)?;
    }
    remove_dotenv_keys(&config_home().join(".env"))
}

pub fn save_alias(alias: &str, model: &str) -> io::Result<PathBuf> {
    let path = settings_path();
    let mut root = read_object(&path);
    let aliases = root
        .entry("aliases".to_string())
        .or_insert_with(|| Value::Object(Map::new()));
    if !aliases.is_object() {
        *aliases = Value::Object(Map::new());
    }
    aliases.as_object_mut().expect("aliases object").insert(
        alias.trim().to_string(),
        Value::String(model.trim().to_string()),
    );
    write_object(&path, root)?;
    Ok(path)
}

pub fn remove_alias(alias: &str) -> io::Result<bool> {
    let path = settings_path();
    if !path.exists() {
        return Ok(false);
    }
    let mut root = read_object(&path);
    let Some(aliases) = root.get_mut("aliases").and_then(Value::as_object_mut) else {
        return Ok(false);
    };
    let removed = aliases.remove(alias.trim()).is_some();
    if removed {
        write_object(&path, root)?;
    }
    Ok(removed)
}

fn settings_path() -> PathBuf {
    config_home().join("settings.json")
}

fn config_home() -> PathBuf {
    std::env::var_os("CLAW_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".claw")))
        .unwrap_or_else(|| PathBuf::from(".claw"))
}

fn read_object(path: &Path) -> Map<String, Value> {
    fs::read_to_string(path)
        .ok()
        .and_then(|contents| serde_json::from_str::<Value>(&contents).ok())
        .and_then(|value| value.as_object().cloned())
        .unwrap_or_default()
}

fn write_object(path: &Path, root: Map<String, Value>) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let contents = serde_json::to_string_pretty(&Value::Object(root)).map_err(io::Error::other)?;
    fs::write(path, contents)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
    }
    Ok(())
}

fn credential_env_for_kind(kind: &str) -> &'static str {
    match kind {
        "anthropic" | "anthropic-compatible" => "ANTHROPIC_API_KEY",
        "xai" => "XAI_API_KEY",
        "dashscope" => "DASHSCOPE_API_KEY",
        _ => "OPENAI_API_KEY",
    }
}

fn base_url_env_for_kind(kind: &str) -> &'static str {
    match kind {
        "anthropic" | "anthropic-compatible" => "ANTHROPIC_BASE_URL",
        "xai" => "XAI_BASE_URL",
        "dashscope" => "DASHSCOPE_BASE_URL",
        _ => "OPENAI_BASE_URL",
    }
}

fn nonempty_value(value: Option<String>) -> Option<String> {
    value.filter(|value| !value.trim().is_empty())
}

fn nonempty_env(key: &str) -> Option<String> {
    std::env::var(key)
        .ok()
        .filter(|value| !value.trim().is_empty())
}

fn load_dotenv_values(path: &Path) -> BTreeMap<String, String> {
    fs::read_to_string(path)
        .ok()
        .map(|contents| {
            contents
                .lines()
                .filter_map(|line| {
                    let line = line.trim();
                    if line.is_empty() || line.starts_with('#') {
                        return None;
                    }
                    let (key, value) = line.split_once('=')?;
                    Some((dotenv_key(key).to_string(), value.trim().to_string()))
                })
                .collect()
        })
        .unwrap_or_default()
}

fn upsert_dotenv(path: &Path, values: &[(&str, &str)]) -> io::Result<()> {
    if values
        .iter()
        .any(|(_, value)| value.contains('\n') || value.contains('\r'))
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "endpoint settings cannot contain newlines",
        ));
    }
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let existing = fs::read_to_string(path).unwrap_or_default();
    let mut lines = Vec::new();
    let mut written = BTreeMap::new();
    for line in existing.lines() {
        let Some((raw_key, _)) = line.split_once('=') else {
            lines.push(line.to_string());
            continue;
        };
        let key = dotenv_key(raw_key);
        if let Some((_, value)) = values.iter().find(|(candidate, _)| *candidate == key) {
            lines.push(format!("{key}={value}"));
            written.insert(key.to_string(), ());
        } else {
            lines.push(line.to_string());
        }
    }
    for (key, value) in values {
        if !written.contains_key(*key) {
            lines.push(format!("{key}={value}"));
        }
    }
    let mut contents = lines.join("\n");
    if !contents.is_empty() {
        contents.push('\n');
    }
    fs::write(path, contents)?;
    restrict_file_permissions(path)
}

fn remove_dotenv_keys(path: &Path) -> io::Result<()> {
    let contents = match fs::read_to_string(path) {
        Ok(contents) => contents,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error),
    };
    let keys = [
        "CLAW_ENDPOINT_TYPE",
        "OPENAI_BASE_URL",
        "OPENAI_API_KEY",
        "ANTHROPIC_BASE_URL",
        "ANTHROPIC_API_KEY",
        "ANTHROPIC_AUTH_TOKEN",
        "XAI_API_KEY",
        "NVIDIA_API_KEY",
        "DASHSCOPE_API_KEY",
    ];
    let retained = contents
        .lines()
        .filter(|line| {
            let Some((key, _)) = line.split_once('=') else {
                return true;
            };
            !keys.contains(&dotenv_key(key))
        })
        .collect::<Vec<_>>();
    if retained.is_empty() {
        fs::remove_file(path)?;
    } else {
        fs::write(path, format!("{}\n", retained.join("\n")))?;
        restrict_file_permissions(path)?;
    }
    Ok(())
}

fn restrict_file_permissions(path: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
    }
    Ok(())
}

fn dotenv_key(raw_key: &str) -> &str {
    raw_key
        .trim()
        .strip_prefix("export ")
        .map_or(raw_key.trim(), str::trim)
}

fn load_merged_settings() -> Map<String, Value> {
    let cwd = std::env::current_dir().unwrap_or_default();
    let home = config_home();
    let paths = [
        home.parent().map_or_else(
            || PathBuf::from(".claw.json"),
            |parent| parent.join(".claw.json"),
        ),
        home.join("settings.json"),
        cwd.join(".claw.json"),
        cwd.join(".claw/settings.json"),
        cwd.join(".claw/settings.local.json"),
    ];
    let mut merged = Map::new();
    for path in paths {
        merge_objects(&mut merged, read_object(&path));
    }
    merged
}

fn merge_objects(target: &mut Map<String, Value>, source: Map<String, Value>) {
    for (key, value) in source {
        match (target.get_mut(&key), value) {
            (Some(Value::Object(existing)), Value::Object(incoming)) => {
                merge_objects(existing, incoming);
            }
            (_, value) => {
                target.insert(key, value);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::merge_objects;
    use serde_json::{json, Map, Value};

    #[test]
    fn settings_merge_preserves_nested_values() {
        let mut target = Map::new();
        target.insert("provider".to_string(), json!({"kind": "openai"}));
        let source = Map::from_iter([(String::from("provider"), json!({"model": "gpt-4.1"}))]);
        merge_objects(&mut target, source);
        assert_eq!(target["provider"]["kind"], Value::String("openai".into()));
        assert_eq!(target["provider"]["model"], Value::String("gpt-4.1".into()));
    }
}
