use anyhow::{Context, Result, bail};
use clap::ValueEnum;
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    io::Write,
    path::{Path, PathBuf},
};

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq, ValueEnum)]
#[serde(rename_all = "kebab-case")]
pub enum Agent {
    Claude,
    Codex,
}

impl Agent {
    pub fn executable(self) -> &'static str {
        match self {
            Self::Claude => "claude",
            Self::Codex => "codex",
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum Protocol {
    AnthropicMessages,
    OpenaiResponses,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "auth", rename_all = "kebab-case", deny_unknown_fields)]
pub enum Endpoint {
    Native,
    ApiKey {
        protocol: Protocol,
        base_url: String,
        key_env: String,
    },
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Profile {
    pub agent: Agent,
    pub endpoint: String,
    pub label: Option<String>,
    pub description: Option<String>,
    #[serde(default)]
    pub tags: Vec<String>,
    pub order: Option<i32>,
    pub model: Option<String>,
    pub reasoning: Option<String>,
    #[serde(default)]
    pub disabled: bool,
    #[serde(default)]
    pub args: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub version: u32,
    #[serde(default)]
    pub selector: Selector,
    pub endpoints: BTreeMap<String, Endpoint>,
    pub profiles: BTreeMap<String, Profile>,
    #[serde(default)]
    pub executables: BTreeMap<String, PathBuf>,
    #[serde(default)]
    pub integrations: BTreeMap<String, Integration>,
    #[serde(default)]
    pub keybindings: Keybindings,
}

#[derive(Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum Selector {
    #[default]
    Tui,
    Builtin,
    Fzf,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Keybindings {
    #[serde(default)]
    pub tui: TuiKeybindings,
    #[serde(default)]
    pub fzf: FzfKeybindings,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TuiKeybindings {
    #[serde(default = "default_accept")]
    pub accept: String,
    #[serde(default = "default_cancel")]
    pub cancel: String,
    #[serde(default = "default_previous")]
    pub previous: String,
    #[serde(default = "default_next")]
    pub next: String,
    #[serde(default = "default_toggle_preview")]
    pub toggle_preview: String,
    #[serde(default = "default_preview_below")]
    pub preview_below: String,
    #[serde(default = "default_preview_right")]
    pub preview_right: String,
    #[serde(default = "default_choose_agent")]
    pub choose_agent: String,
    #[serde(default = "default_choose_endpoint")]
    pub choose_endpoint: String,
    #[serde(default = "default_help")]
    pub help: String,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FzfKeybindings {
    #[serde(default = "default_accept")]
    pub accept: String,
    #[serde(default = "default_cancel")]
    pub cancel: String,
    #[serde(default = "default_previous")]
    pub previous: String,
    #[serde(default = "default_next")]
    pub next: String,
    #[serde(default = "default_toggle_preview")]
    pub toggle_preview: String,
    #[serde(default = "default_preview_below")]
    pub preview_below: String,
    #[serde(default = "default_preview_right")]
    pub preview_right: String,
    #[serde(default = "default_choose_agent")]
    pub choose_agent: String,
    #[serde(default = "default_choose_endpoint")]
    pub choose_endpoint: String,
}

macro_rules! fzf_defaults {
    ($($name:ident = $value:literal),+ $(,)?) => {
        $(fn $name() -> String { $value.to_owned() })+
    };
}

fzf_defaults!(
    default_accept = "enter",
    default_cancel = "esc",
    default_previous = "up",
    default_next = "down",
    default_toggle_preview = "ctrl-p",
    default_preview_below = "ctrl-/",
    default_preview_right = "alt-/",
    default_choose_agent = "ctrl-l",
    default_choose_endpoint = "ctrl-e",
    default_help = "f1",
);

impl Default for TuiKeybindings {
    fn default() -> Self {
        Self {
            accept: default_accept(),
            cancel: default_cancel(),
            previous: default_previous(),
            next: default_next(),
            toggle_preview: default_toggle_preview(),
            preview_below: default_preview_below(),
            preview_right: default_preview_right(),
            choose_agent: default_choose_agent(),
            choose_endpoint: default_choose_endpoint(),
            help: default_help(),
        }
    }
}

impl TuiKeybindings {
    pub fn canonicalize(&mut self) -> Result<()> {
        self.accept = normalize_tui_key(&self.accept)?;
        self.cancel = normalize_tui_key(&self.cancel)?;
        self.previous = normalize_tui_key(&self.previous)?;
        self.next = normalize_tui_key(&self.next)?;
        self.toggle_preview = normalize_tui_key(&self.toggle_preview)?;
        self.preview_below = normalize_tui_key(&self.preview_below)?;
        self.preview_right = normalize_tui_key(&self.preview_right)?;
        self.choose_agent = normalize_tui_key(&self.choose_agent)?;
        self.choose_endpoint = normalize_tui_key(&self.choose_endpoint)?;
        self.help = normalize_tui_key(&self.help)?;
        Ok(())
    }

    pub fn validate(&self) -> Result<()> {
        let bindings = [
            ("accept", &self.accept),
            ("cancel", &self.cancel),
            ("previous", &self.previous),
            ("next", &self.next),
            ("toggle_preview", &self.toggle_preview),
            ("preview_below", &self.preview_below),
            ("preview_right", &self.preview_right),
            ("choose_agent", &self.choose_agent),
            ("choose_endpoint", &self.choose_endpoint),
            ("help", &self.help),
        ];
        let mut keys = std::collections::BTreeSet::new();
        for (action, key) in bindings {
            let key = normalize_tui_key(key)?;
            if key == "ctrl-c" {
                bail!("Ctrl-C is reserved for cancellation and cannot be rebound");
            }
            if !keys.insert(key) {
                bail!("TUI keybindings must not use the same key more than once ({action})");
            }
        }
        Ok(())
    }
}

impl Default for FzfKeybindings {
    fn default() -> Self {
        Self {
            accept: default_accept(),
            cancel: default_cancel(),
            previous: default_previous(),
            next: default_next(),
            toggle_preview: default_toggle_preview(),
            preview_below: default_preview_below(),
            preview_right: default_preview_right(),
            choose_agent: default_choose_agent(),
            choose_endpoint: default_choose_endpoint(),
        }
    }
}

impl FzfKeybindings {
    pub fn canonicalize(&mut self) -> Result<()> {
        self.accept = normalize_fzf_key(&self.accept)?;
        self.cancel = normalize_fzf_key(&self.cancel)?;
        self.previous = normalize_fzf_key(&self.previous)?;
        self.next = normalize_fzf_key(&self.next)?;
        self.toggle_preview = normalize_fzf_key(&self.toggle_preview)?;
        self.preview_below = normalize_fzf_key(&self.preview_below)?;
        self.preview_right = normalize_fzf_key(&self.preview_right)?;
        self.choose_agent = normalize_fzf_key(&self.choose_agent)?;
        self.choose_endpoint = normalize_fzf_key(&self.choose_endpoint)?;
        Ok(())
    }

    pub fn validate(&self) -> Result<()> {
        let bindings = [
            ("accept", &self.accept),
            ("cancel", &self.cancel),
            ("previous", &self.previous),
            ("next", &self.next),
            ("toggle_preview", &self.toggle_preview),
            ("preview_below", &self.preview_below),
            ("preview_right", &self.preview_right),
            ("choose_agent", &self.choose_agent),
            ("choose_endpoint", &self.choose_endpoint),
        ];
        let mut keys = std::collections::BTreeSet::new();
        for (action, key) in bindings {
            let key = normalize_fzf_key(key)?;
            if key == "ctrl-c" {
                bail!("Ctrl-C is reserved for cancellation and cannot be rebound");
            }
            if !keys.insert(key) {
                bail!("Fzf keybindings must not use the same key more than once ({action})");
            }
        }
        Ok(())
    }
}

pub fn normalize_fzf_key(value: &str) -> Result<String> {
    if value.is_empty() || value.trim() != value || !value.is_ascii() {
        bail!("Fzf keybindings must use a named key or ctrl-/alt- modifier key");
    }
    let key = value.to_ascii_lowercase();
    if [
        "enter",
        "esc",
        "up",
        "down",
        "tab",
        "btab",
        "backspace",
        "delete",
        "home",
        "end",
        "pgup",
        "pgdn",
    ]
    .contains(&key.as_str())
    {
        return Ok(key);
    }
    if let Some(suffix) = key.strip_prefix("ctrl-") {
        return match suffix {
            "m" => Ok("enter".to_owned()),
            "i" => Ok("tab".to_owned()),
            "h" => Ok("backspace".to_owned()),
            "[" => Ok("esc".to_owned()),
            _ if suffix.len() == 1
                && (suffix.as_bytes()[0].is_ascii_lowercase() || suffix == "/") =>
            {
                Ok(key)
            }
            _ => bail!("Fzf keybindings must use a named key or ctrl-/alt- modifier key"),
        };
    }
    if let Some(suffix) = key.strip_prefix("alt-")
        && suffix.len() == 1
        && (suffix.as_bytes()[0].is_ascii_lowercase() || suffix == "/")
    {
        return Ok(key);
    }
    bail!("Fzf keybindings must use a named key or ctrl-/alt- modifier key")
}

pub fn normalize_tui_key(value: &str) -> Result<String> {
    if value.is_empty() || value.trim() != value || !value.is_ascii() {
        bail!("TUI keybindings must use a supported named key or ctrl-/alt- modifier key");
    }
    let key = value.to_ascii_lowercase();
    if [
        "enter",
        "esc",
        "up",
        "down",
        "left",
        "right",
        "tab",
        "btab",
        "backspace",
        "delete",
        "home",
        "end",
        "pgup",
        "pgdn",
        "f1",
        "f2",
        "f3",
        "f4",
        "f5",
        "f6",
        "f7",
        "f8",
        "f9",
        "f10",
        "f11",
        "f12",
    ]
    .contains(&key.as_str())
    {
        return Ok(key);
    }
    if let Some(suffix) = key.strip_prefix("ctrl-") {
        return match suffix {
            "m" => Ok("enter".to_owned()),
            "i" => Ok("tab".to_owned()),
            "h" => Ok("backspace".to_owned()),
            "[" => Ok("esc".to_owned()),
            _ if suffix.len() == 1
                && (suffix.as_bytes()[0].is_ascii_lowercase() || suffix == "/") =>
            {
                Ok(key)
            }
            _ => bail!("TUI keybindings must use a supported named key or ctrl-/alt- modifier key"),
        };
    }
    if let Some(suffix) = key.strip_prefix("alt-")
        && suffix.len() == 1
        && (suffix.as_bytes()[0].is_ascii_lowercase() || suffix == "/")
    {
        return Ok(key);
    }
    bail!("TUI keybindings must use a supported named key or ctrl-/alt- modifier key")
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Integration {
    pub target: PathBuf,
    pub source: Option<PathBuf>,
    pub expected_command: Option<String>,
}

pub fn home() -> Result<PathBuf> {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .context("HOME is not set")
}

pub fn default_path() -> Result<PathBuf> {
    let root = match std::env::var_os("XDG_CONFIG_HOME") {
        Some(value) if !value.is_empty() => PathBuf::from(value),
        _ => home()?.join(".config"),
    };
    Ok(root.join("nomad/config.toml"))
}

pub fn state_dir() -> Result<PathBuf> {
    let root = match std::env::var_os("XDG_STATE_HOME") {
        Some(value) if !value.is_empty() => PathBuf::from(value),
        _ => home()?.join(".local/state"),
    };
    Ok(root.join("nomad"))
}

pub fn write_state(path: &Path, bytes: &[u8]) -> Result<()> {
    let parent = path.parent().context("State path has no parent")?;
    std::fs::create_dir_all(parent).context("Cannot create Nomad state directory")?;
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .context("State path has no file name")?;
    let temporary = parent.join(format!(".{name}.{}.tmp", std::process::id()));
    let mut file = std::fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&temporary)
        .context("Cannot create temporary Nomad state file")?;
    let result: Result<()> = (|| {
        file.write_all(bytes)?;
        file.sync_all()?;
        std::fs::rename(&temporary, path)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&temporary);
    }
    result.context("Cannot update Nomad state")
}

impl Config {
    pub fn load(path: &Path) -> Result<Self> {
        let content = std::fs::read_to_string(path)
            .with_context(|| format!("Cannot read {}; run nomad init first", path.display()))?;
        // Parser diagnostics may contain source excerpts, so never print them.
        let raw: toml::Value = toml::from_str(&content).map_err(|_| {
            anyhow::anyhow!(
                "Invalid Nomad TOML configuration; check types, required fields, and unknown keys"
            )
        })?;
        reject_legacy_config(&raw)?;
        let mut config: Self = toml::from_str(&content).map_err(|_| {
            anyhow::anyhow!(
                "Invalid Nomad TOML configuration; check types, required fields, and unknown keys"
            )
        })?;
        if config.version != 1 {
            bail!("Unsupported configuration version {}", config.version);
        }
        config.keybindings.fzf.canonicalize()?;
        config.keybindings.fzf.validate()?;
        config.keybindings.tui.canonicalize()?;
        config.keybindings.tui.validate()?;
        let absolute = if path.is_absolute() {
            path.to_owned()
        } else {
            std::env::current_dir()?.join(path)
        };
        let base = absolute.parent().unwrap_or(Path::new("."));
        for executable in config.executables.values_mut() {
            if executable.is_relative() {
                *executable = base.join(&*executable);
            }
        }
        for integration in config.integrations.values_mut() {
            if integration.target.is_relative() {
                integration.target = base.join(&integration.target);
            }
            if let Some(source) = &mut integration.source
                && source.is_relative()
            {
                *source = base.join(&*source);
            }
        }
        for name in config.profiles.keys() {
            if name.is_empty()
                || !name
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
            {
                bail!("Profile names must contain only letters, digits, '.', '_' or '-'");
            }
        }
        Ok(config)
    }
}

fn reject_legacy_config(raw: &toml::Value) -> Result<()> {
    let Some(root) = raw.as_table() else {
        return Ok(());
    };
    let pi_profile = root
        .get("profiles")
        .and_then(toml::Value::as_table)
        .is_some_and(|profiles| {
            profiles.values().any(|profile| {
                profile
                    .as_table()
                    .and_then(|profile| profile.get("agent"))
                    .and_then(toml::Value::as_str)
                    == Some("pi")
            })
        });
    let pi_executable = root
        .get("executables")
        .and_then(toml::Value::as_table)
        .is_some_and(|executables| executables.contains_key("pi"));
    let pi_integration = root
        .get("integrations")
        .and_then(toml::Value::as_table)
        .is_some_and(|integrations| integrations.contains_key("pi-statusline"));
    let chat_completions = root
        .get("endpoints")
        .and_then(toml::Value::as_table)
        .is_some_and(|endpoints| {
            endpoints.values().any(|endpoint| {
                endpoint
                    .as_table()
                    .and_then(|endpoint| endpoint.get("protocol"))
                    .and_then(toml::Value::as_str)
                    == Some("openai-chat-completions")
            })
        });
    let token_limits = root
        .get("profiles")
        .and_then(toml::Value::as_table)
        .is_some_and(|profiles| {
            profiles.values().any(|profile| {
                profile.as_table().is_some_and(|profile| {
                    profile.contains_key("context_window") || profile.contains_key("max_tokens")
                })
            })
        });
    if pi_profile || pi_executable || pi_integration || chat_completions || token_limits {
        bail!(
            "This configuration uses removed Pi or chat-completions settings; migrate profiles and endpoints to Claude/Codex native or supported API-key settings"
        )
    }
    Ok(())
}

pub const INITIAL: &str = "version = 1\n\n[endpoints.official]\nauth = \"native\"\n\n[profiles.claude]\nagent = \"claude\"\nendpoint = \"official\"\n\n[profiles.codex]\nagent = \"codex\"\nendpoint = \"official\"\n";
