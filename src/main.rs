use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};
mod config;
mod endpoint;
mod error;
mod integration;
mod launcher;
mod security;
mod selector;
mod sharing;
mod storage;
use config::Config;
use std::{
    ffi::OsString,
    io::{IsTerminal, Write},
    path::PathBuf,
};

#[derive(Parser)]
#[command(
    version,
    about = "Portable coding-agent profiles and endpoint launcher"
)]
struct Cli {
    #[arg(long, global = true)]
    config: Option<PathBuf>,
    #[arg(long, global = true)]
    agent: Option<config::Agent>,
    #[arg(long, global = true)]
    endpoint: Option<String>,
    #[command(subcommand)]
    command: Option<Action>,
}
#[derive(Subcommand)]
enum Action {
    Init,
    List {
        #[arg(long)]
        table: bool,
    },
    Doctor,
    Export {
        directory: PathBuf,
        #[arg(long = "profile")]
        profiles: Vec<String>,
        #[arg(long = "skill")]
        skills: Vec<PathBuf>,
    },
    Import {
        directory: PathBuf,
        #[arg(long)]
        skills_dir: PathBuf,
        #[arg(long = "replace")]
        replace: Vec<String>,
        #[arg(long = "key-env")]
        key_env: Vec<String>,
    },
    Sync {
        directory: PathBuf,
        #[arg(long)]
        skills_dir: PathBuf,
        #[arg(long = "replace")]
        replace: Vec<String>,
        #[arg(long = "key-env")]
        key_env: Vec<String>,
    },
    Apply {
        operation_id: String,
    },
    Restore {
        operation_id: String,
    },
    Run {
        profile: String,
        #[arg(long)]
        dry_run: bool,
        #[arg(long)]
        cwd: Option<PathBuf>,
        #[arg(last = true)]
        args: Vec<OsString>,
    },
    Integrate {
        #[command(subcommand)]
        action: IntegrationAction,
    },
    #[command(name = "__selector-preview", hide = true)]
    SelectorPreview {
        profile: String,
    },
}
#[derive(Subcommand)]
enum IntegrationAction {
    Plan { component: String },
    Apply { component: String },
    Restore { operation_id: String },
}

enum LoadedAction {
    List {
        table: bool,
    },
    Doctor,
    Run {
        profile: String,
        dry_run: bool,
        cwd: Option<PathBuf>,
        args: Vec<OsString>,
    },
    IntegratePlan {
        component: String,
    },
    IntegrateApply {
        component: String,
    },
    SelectorPreview {
        profile: String,
    },
    Export {
        directory: PathBuf,
        profiles: Vec<String>,
        skills: Vec<PathBuf>,
    },
    Import {
        directory: PathBuf,
        skills_dir: PathBuf,
        replace: Vec<String>,
        key_env: Vec<String>,
    },
    Interactive,
}

fn run() -> Result<()> {
    let cli = Cli::parse();
    let agent_filter = cli.agent;
    let endpoint_override = cli.endpoint;
    let path = cli.config.map(Ok).unwrap_or_else(config::default_path)?;
    let action = match cli.command {
        Some(Action::Init) => {
            std::fs::create_dir_all(path.parent().context("Configuration path has no parent")?)?;
            let mut file = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&path)
                .context(
                    "Cannot initialize configuration; an existing file is never overwritten",
                )?;
            file.write_all(config::INITIAL.as_bytes())?;
            println!("Created {}", path.display());
            return Ok(());
        }
        Some(Action::Apply { operation_id }) => return sharing::apply(&operation_id).map(|_| ()),
        Some(Action::Restore { operation_id }) => return sharing::restore(&operation_id),
        Some(Action::Integrate {
            action: IntegrationAction::Restore { operation_id },
        }) => return integration::restore(&operation_id),
        Some(Action::List { table }) => LoadedAction::List { table },
        Some(Action::Doctor) => LoadedAction::Doctor,
        Some(Action::Export {
            directory,
            profiles,
            skills,
        }) => LoadedAction::Export {
            directory,
            profiles,
            skills,
        },
        Some(Action::Import {
            directory,
            skills_dir,
            replace,
            key_env,
        })
        | Some(Action::Sync {
            directory,
            skills_dir,
            replace,
            key_env,
        }) => LoadedAction::Import {
            directory,
            skills_dir,
            replace,
            key_env,
        },
        Some(Action::Run {
            profile,
            dry_run,
            cwd,
            args,
        }) => LoadedAction::Run {
            profile,
            dry_run,
            cwd,
            args,
        },
        Some(Action::Integrate {
            action: IntegrationAction::Plan { component },
        }) => LoadedAction::IntegratePlan { component },
        Some(Action::Integrate {
            action: IntegrationAction::Apply { component },
        }) => LoadedAction::IntegrateApply { component },
        Some(Action::SelectorPreview { profile }) => LoadedAction::SelectorPreview { profile },
        None => LoadedAction::Interactive,
    };
    let config = Config::load(&path)?;
    match action {
        LoadedAction::List { table } => print_profiles(&config, table, agent_filter),
        LoadedAction::Doctor => {
            let mut failed = false;
            for (name, p) in &config.profiles {
                if agent_filter.is_some_and(|agent| p.agent != agent) {
                    continue;
                }
                if p.disabled {
                    println!("{name}: disabled");
                    continue;
                }
                match launcher::build(&config, name, None, &[]) {
                    Ok(plan) => {
                        let missing = plan.credential.as_ref().is_some_and(|(key, _)| {
                            std::env::var_os(key).is_none_or(|v| v.is_empty())
                        });
                        println!(
                            "{name}: {}",
                            if missing {
                                "credential variable missing"
                            } else {
                                "local configuration ready; account access not checked"
                            }
                        );
                        failed |= missing;
                    }
                    Err(e) => {
                        println!("{name}: {e}");
                        failed = true;
                    }
                }
            }
            if failed {
                bail!("One or more profiles need attention");
            }
        }
        LoadedAction::Run {
            profile,
            dry_run,
            cwd,
            args,
        } => {
            ensure_agent_filter(&config, &profile, agent_filter)?;
            let plan = launcher::build_with_endpoint(
                &config,
                &profile,
                endpoint_override.as_deref(),
                cwd.as_deref(),
                &args,
            )?;
            if dry_run {
                print!("{}", plan.preview());
            } else {
                plan.execute()?;
            }
        }
        LoadedAction::IntegratePlan { component } => integration::plan(&config, &component)?,
        LoadedAction::IntegrateApply { component } => integration::apply(&config, &component)?,
        LoadedAction::SelectorPreview { profile } => {
            print!("{}", selector::preview(&config, &profile)?)
        }
        LoadedAction::Export {
            directory,
            profiles,
            skills,
        } => {
            let agent = agent_filter.context("export requires --agent claude|codex")?;
            sharing::export(
                &config,
                &directory,
                &sharing::ExportOptions {
                    agent,
                    profiles,
                    skills,
                },
            )?;
        }
        LoadedAction::Import {
            directory,
            skills_dir,
            replace,
            key_env,
        } => {
            let agent = agent_filter.context("import requires --agent claude|codex")?;
            let options = sharing::ImportOptions {
                agent,
                skills_dir,
                replace: parse_replace(&replace)?,
                key_env: parse_key_env(&key_env)?,
            };
            let preview = sharing::plan_import(&config, &path, &directory, &options)?;
            println!("{}", preview.id);
            for change in preview.changes {
                println!("{} {} {}", change.action, change.kind, change.id);
            }
        }
        LoadedAction::Interactive => {
            if !std::io::stdin().is_terminal() || !std::io::stdout().is_terminal() {
                bail!("Noninteractive use requires nomad run <profile>");
            }
            let last_file = config::state_dir()?.join("last-profile");
            let last = std::fs::read_to_string(&last_file).ok();
            let Some(selected) = selector::select(
                &config,
                &path,
                last.as_deref().map(str::trim),
                agent_filter,
                endpoint_override.as_deref(),
            )?
            else {
                return Ok(());
            };
            let plan = launcher::build_with_endpoint(
                &config,
                &selected.profile,
                selected.endpoint.as_deref(),
                None,
                &[],
            )?;
            config::write_state(&last_file, selected.profile.as_bytes())?;
            plan.execute()?;
        }
    }
    Ok(())
}

fn print_profiles(config: &Config, table: bool, agent_filter: Option<config::Agent>) {
    if table {
        let table = match agent_filter {
            Some(agent) => {
                selector::table_filtered(config, selector::terminal_width(), Some(agent))
            }
            None => selector::table(config, selector::terminal_width()),
        };
        print!("{table}");
        return;
    }
    for (name, profile) in &config.profiles {
        if !profile.disabled && agent_filter.is_none_or(|agent| profile.agent == agent) {
            println!(
                "{name}\t{}\t{}\t{}\t{}",
                profile.agent.executable(),
                selector::display_field(&profile.endpoint),
                selector::display_field(profile.model.as_deref().unwrap_or("native default")),
                selector::display_field(profile.reasoning.as_deref().unwrap_or("native default"))
            );
        }
    }
}

fn ensure_agent_filter(
    config: &Config,
    profile: &str,
    agent_filter: Option<config::Agent>,
) -> Result<()> {
    if let Some(agent) = agent_filter {
        let profile_agent = config
            .profiles
            .get(profile)
            .ok_or(error::LaunchError::UnknownProfile)?
            .agent;
        if profile_agent != agent {
            bail!("Profile agent does not match --agent")
        }
    }
    Ok(())
}

fn parse_replace(values: &[String]) -> Result<std::collections::BTreeSet<String>> {
    let mut result = std::collections::BTreeSet::new();
    for value in values {
        let (kind, id) = value.split_once(':').context("--replace must be kind:id")?;
        if !matches!(kind, "profile" | "endpoint" | "skill") || id.is_empty() {
            bail!("--replace must be profile:id, endpoint:id, or skill:id")
        }
        result.insert(value.clone());
    }
    Ok(result)
}

fn parse_key_env(values: &[String]) -> Result<std::collections::BTreeMap<String, String>> {
    let mut result = std::collections::BTreeMap::new();
    for value in values {
        let (endpoint, key_env) = value
            .split_once('=')
            .context("--key-env must be endpoint=ENV_VAR")?;
        if endpoint.is_empty() || key_env.is_empty() {
            bail!("--key-env must be endpoint=ENV_VAR")
        }
        if result
            .insert(endpoint.to_owned(), key_env.to_owned())
            .is_some()
        {
            bail!("--key-env may name each endpoint only once")
        }
    }
    Ok(result)
}

fn main() {
    if let Err(error) = run() {
        eprintln!("nomad: {error:#}");
        std::process::exit(1);
    }
}
