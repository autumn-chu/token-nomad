use crate::config::{Agent, Endpoint, home};
use anyhow::{Context, Result, bail};
use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
};

pub const CLAUDE_ROUTING: &[&str] = &[
    "ANTHROPIC_BASE_URL",
    "ANTHROPIC_API_KEY",
    "ANTHROPIC_AUTH_TOKEN",
    "ANTHROPIC_CUSTOM_HEADERS",
    "CLAUDE_CODE_USE_BEDROCK",
    "CLAUDE_CODE_USE_VERTEX",
    "CLAUDE_CODE_USE_FOUNDRY",
];

fn root(variable: &str, default: PathBuf) -> PathBuf {
    std::env::var_os(variable)
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .unwrap_or(default)
}

fn json(path: &Path) -> Result<Option<serde_json::Value>> {
    match std::fs::read(path) {
        Ok(bytes) => serde_json::from_slice(&bytes).map(Some).map_err(|_| {
            anyhow::anyhow!("Cannot inspect invalid agent JSON at {}", path.display())
        }),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => {
            Err(e).with_context(|| format!("Cannot inspect agent settings at {}", path.display()))
        }
    }
}

/// Refuse native settings that could override the selected endpoint after exec.
pub fn check(agent: Agent, endpoint: &Endpoint, cwd: &Path) -> Result<()> {
    let home = home()?;
    let ancestors: Vec<_> = cwd
        .canonicalize()?
        .ancestors()
        .map(Path::to_owned)
        .collect();
    match agent {
        Agent::Claude => {
            let mut files = BTreeSet::new();
            let managed = if cfg!(target_os = "macos") {
                Path::new("/Library/Application Support/ClaudeCode")
            } else {
                Path::new("/etc/claude-code")
            };
            files.insert(managed.join("managed-settings.json"));
            match std::fs::read_dir(managed.join("managed-settings.d")) {
                Ok(entries) => {
                    for entry in entries {
                        let path = entry?.path();
                        if path.extension().is_some_and(|ext| ext == "json") {
                            files.insert(path);
                        }
                    }
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(error).context("Cannot inspect Claude managed settings"),
            }
            files.insert(root("CLAUDE_CONFIG_DIR", home.join(".claude")).join("settings.json"));
            for dir in &ancestors {
                files.insert(dir.join(".claude/settings.json"));
                files.insert(dir.join(".claude/settings.local.json"));
            }
            for path in files {
                if let Some(settings) = json(&path)? {
                    let env = settings.get("env");
                    let conflicts = CLAUDE_ROUTING
                        .iter()
                        .any(|key| env.and_then(|v| v.get(key)).is_some_and(|v| !v.is_null()));
                    if conflicts || settings.get("apiKeyHelper").is_some_and(|v| !v.is_null()) {
                        bail!(
                            "Claude endpoint settings conflict at {}; resolve routing/auth configuration before launching",
                            path.display()
                        );
                    }
                    if matches!(endpoint, Endpoint::ApiKey { .. })
                        && env.and_then(|v| v.get("CLAUDE_CODE_OAUTH_TOKEN")).is_some()
                    {
                        bail!("Claude OAuth setting conflicts with the selected API-key endpoint");
                    }
                }
            }
        }
        Agent::Codex => {
            let mut files = BTreeSet::new();
            files.insert(root("CODEX_HOME", home.join(".codex")).join("config.toml"));
            for dir in &ancestors {
                files.insert(dir.join(".codex/config.toml"));
            }
            let provider = if matches!(endpoint, Endpoint::Native) {
                "openai"
            } else {
                "nomad"
            };
            for path in files {
                let content = match std::fs::read_to_string(&path) {
                    Ok(s) => s,
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
                    Err(e) => {
                        return Err(e).with_context(|| {
                            format!("Cannot inspect Codex configuration at {}", path.display())
                        });
                    }
                };
                let config: toml::Value = toml::from_str(&content).map_err(|_| {
                    anyhow::anyhow!(
                        "Cannot inspect invalid Codex configuration at {}",
                        path.display()
                    )
                })?;
                if config
                    .get("model_providers")
                    .and_then(|v| v.get(provider))
                    .is_some()
                {
                    bail!(
                        "Codex provider '{provider}' is customized at {}; resolve this endpoint conflict first",
                        path.display()
                    );
                }
            }
        }
    }
    Ok(())
}
