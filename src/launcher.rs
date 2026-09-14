use crate::config::{Agent, Config, Endpoint, Profile, Protocol};
use crate::error::LaunchError;
use anyhow::{Context, Result, bail};
use std::{
    collections::BTreeMap,
    ffi::OsString,
    os::unix::process::CommandExt,
    path::{Path, PathBuf},
    process::Command,
};

pub struct LaunchPlan {
    pub executable: PathBuf,
    pub args: Vec<OsString>,
    pub cwd: PathBuf,
    pub removals: Vec<&'static str>,
    pub environment: BTreeMap<String, OsString>,
    pub credential: Option<(String, String)>,
}

pub fn locate(name: &Path) -> Result<PathBuf> {
    use std::os::unix::fs::PermissionsExt;
    let candidates = if name.components().count() > 1 || name.is_absolute() {
        vec![name.to_owned()]
    } else {
        std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default())
            .map(|p| p.join(name))
            .collect()
    };
    candidates
        .into_iter()
        .find(|p| {
            p.metadata()
                .is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
        })
        .with_context(|| format!("Executable {} is unavailable", name.display()))?
        .canonicalize()
        .context("Cannot resolve executable path")
}

pub fn validate(profile: &Profile, endpoint: &Endpoint) -> Result<()> {
    if profile.disabled {
        return Err(LaunchError::DisabledProfile.into());
    }
    if let Some(level) = &profile.reasoning {
        let valid = match profile.agent {
            Agent::Claude => ["low", "medium", "high", "xhigh", "max"].contains(&level.as_str()),
            Agent::Codex => {
                ["low", "medium", "high", "xhigh", "max", "ultra"].contains(&level.as_str())
            }
        };
        if !valid {
            bail!(
                "Unsupported reasoning level for {}",
                profile.agent.executable()
            );
        }
    }
    if let Endpoint::ApiKey {
        protocol,
        base_url,
        key_env,
    } = endpoint
    {
        let url = url::Url::parse(base_url).map_err(|_| anyhow::anyhow!("Invalid endpoint URL"))?;
        if !["http", "https"].contains(&url.scheme())
            || url.host_str().is_none()
            || !url.username().is_empty()
            || url.password().is_some()
            || url.query().is_some()
            || url.fragment().is_some()
        {
            bail!("Endpoint URL must be HTTP(S), without embedded credentials, query, or fragment");
        }
        if key_env.is_empty()
            || !key_env
                .bytes()
                .enumerate()
                .all(|(i, b)| b == b'_' || b.is_ascii_alphabetic() || (i > 0 && b.is_ascii_digit()))
        {
            bail!("Invalid credential environment-variable name");
        }
        let supported = match profile.agent {
            Agent::Claude => *protocol == Protocol::AnthropicMessages,
            Agent::Codex => *protocol == Protocol::OpenaiResponses,
        };
        if !supported {
            return Err(LaunchError::IncompatibleEndpoint.into());
        }
        if profile.model.as_deref().is_none_or(str::is_empty) {
            bail!("Custom endpoints require an explicit model");
        }
    }
    Ok(())
}

fn push(args: &mut Vec<OsString>, flag: &str, value: impl Into<OsString>) {
    args.push(flag.into());
    args.push(value.into());
}

pub fn build(
    config: &Config,
    name: &str,
    cwd: Option<&Path>,
    native: &[OsString],
) -> Result<LaunchPlan> {
    build_with_endpoint(config, name, None, cwd, native)
}

pub fn build_with_endpoint(
    config: &Config,
    name: &str,
    endpoint_name: Option<&str>,
    cwd: Option<&Path>,
    native: &[OsString],
) -> Result<LaunchPlan> {
    let profile = config
        .profiles
        .get(name)
        .ok_or(LaunchError::UnknownProfile)?;
    let endpoint_name = endpoint_name.unwrap_or(&profile.endpoint);
    let endpoint = config
        .endpoints
        .get(endpoint_name)
        .ok_or(LaunchError::UnknownEndpoint)?;
    validate(profile, endpoint)?;
    let executable = locate(
        config
            .executables
            .get(profile.agent.executable())
            .map(PathBuf::as_path)
            .unwrap_or(Path::new(profile.agent.executable())),
    )?;
    let cwd = cwd.map(PathBuf::from).unwrap_or(std::env::current_dir()?);
    if !cwd.is_dir() {
        bail!("Working directory does not exist");
    }
    crate::endpoint::check(profile.agent, endpoint, &cwd)?;
    let mut plan = LaunchPlan {
        executable,
        args: vec![],
        cwd,
        removals: vec![],
        environment: BTreeMap::new(),
        credential: None,
    };
    match profile.agent {
        Agent::Claude => {
            plan.removals
                .extend(crate::endpoint::CLAUDE_ROUTING.iter().copied());
            plan.removals.extend([
                "ANTHROPIC_BASE_URL",
                "ANTHROPIC_API_KEY",
                "ANTHROPIC_AUTH_TOKEN",
                "CLAUDE_CODE_USE_BEDROCK",
                "CLAUDE_CODE_USE_VERTEX",
                "CLAUDE_CODE_USE_FOUNDRY",
            ]);
            if let Some(model) = &profile.model {
                push(&mut plan.args, "--model", model);
            }
            if let Some(level) = &profile.reasoning {
                push(&mut plan.args, "--effort", level);
            }
        }
        Agent::Codex => {
            plan.removals.extend(["OPENAI_BASE_URL", "OPENAI_API_KEY"]);
            if let Some(model) = &profile.model {
                push(&mut plan.args, "--model", model);
            }
            if let Some(level) = &profile.reasoning {
                push(
                    &mut plan.args,
                    "-c",
                    format!(
                        "model_reasoning_effort={}",
                        toml::Value::String(level.clone())
                    ),
                );
            }
        }
    }
    match endpoint {
        Endpoint::Native => {
            if profile.agent == Agent::Codex {
                push(&mut plan.args, "-c", "model_provider=\"openai\"");
            }
        }
        Endpoint::ApiKey {
            protocol: _,
            base_url,
            key_env,
        } => match profile.agent {
            Agent::Claude => {
                plan.removals.push("CLAUDE_CODE_OAUTH_TOKEN");
                plan.environment
                    .insert("ANTHROPIC_BASE_URL".into(), base_url.into());
                plan.credential = Some((key_env.clone(), "ANTHROPIC_API_KEY".into()));
            }
            Agent::Codex => {
                for value in [
                    "model_provider=\"nomad\"".to_string(),
                    "model_providers.nomad.name=\"Nomad\"".to_string(),
                    format!(
                        "model_providers.nomad.base_url={}",
                        toml::Value::String(base_url.clone())
                    ),
                    "model_providers.nomad.env_key=\"NOMAD_ENDPOINT_KEY\"".to_string(),
                    "model_providers.nomad.requires_openai_auth=false".to_string(),
                    "model_providers.nomad.wire_api=\"responses\"".to_string(),
                ] {
                    push(&mut plan.args, "-c", value);
                }
                plan.credential = Some((key_env.clone(), "NOMAD_ENDPOINT_KEY".into()));
            }
        },
    }
    plan.args.extend(profile.args.iter().map(OsString::from));
    plan.args.extend_from_slice(native);
    Ok(plan)
}

impl LaunchPlan {
    pub fn preview(&self) -> String {
        // Native arguments can carry arbitrary secrets; only show their count.
        format!(
            "executable: {}\nworking directory: {}\narguments: {} (values hidden)\ncredential source: {}\n",
            self.executable.display(),
            self.cwd.display(),
            self.args.len(),
            self.credential
                .as_ref()
                .map(|(source, _)| source.as_str())
                .unwrap_or("native login")
        )
    }
    pub fn execute(mut self) -> Result<()> {
        if let Some((source, target)) = &self.credential {
            let value = std::env::var_os(source)
                .filter(|v| !v.is_empty())
                .ok_or_else(|| LaunchError::MissingCredential(source.clone()))?;
            self.environment.insert(target.clone(), value);
        }
        let mut command = Command::new(self.executable);
        command.args(self.args).current_dir(self.cwd);
        for key in self.removals {
            command.env_remove(key);
        }
        command.envs(self.environment);
        let error = command.exec();
        Err(error).context("Unable to execute agent")
    }
}
