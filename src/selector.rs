use crate::{
    config::{Agent, Config, Endpoint, FzfKeybindings, Profile, Selector},
    launcher,
};
use anyhow::{Context, Result, bail};
use std::{
    fmt,
    io::Write,
    path::Path,
    process::{Command, Stdio},
};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

const REPLACEMENT_CHARACTER: char = '\u{fffd}';
const CHOOSE_AGENT_QUERY: &str = "\u{200b}";
const CHOOSE_ENDPOINT_QUERY: &str = "\u{200c}";
const FOLLOW_PROFILE_ENDPOINT_LABEL: &str = "Follow selected profile endpoint";

#[derive(Clone)]
struct Item {
    id: String,
    order: i32,
    label: String,
    agent: String,
    model: String,
    reasoning: String,
    tags: String,
    display: String,
    preview: String,
}

pub struct Selection {
    pub profile: String,
    pub endpoint: Option<String>,
}

impl fmt::Display for Item {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.display)
    }
}

pub fn select(
    config: &Config,
    config_path: &Path,
    last_profile: Option<&str>,
    agent_filter: Option<Agent>,
    endpoint_override: Option<&str>,
) -> Result<Option<Selection>> {
    let items = items(config, agent_filter);
    if items.is_empty() {
        bail!("No enabled profiles");
    }
    match config.selector {
        Selector::Builtin => select_builtin(items, last_profile).map(|selection| {
            selection.map(|profile| Selection {
                profile,
                endpoint: endpoint_override.map(str::to_owned),
            })
        }),
        Selector::Fzf => select_fzf(
            config,
            config_path,
            last_profile,
            agent_filter,
            endpoint_override,
        ),
    }
}

pub fn preview(config: &Config, profile_id: &str) -> Result<String> {
    items(config, None)
        .into_iter()
        .find(|item| item.id == profile_id)
        .map(|item| item.preview)
        .context("Selector profile is unavailable")
}

pub fn table(config: &Config, width: usize) -> String {
    table_filtered(config, width, None)
}

pub fn table_filtered(config: &Config, width: usize, agent_filter: Option<Agent>) -> String {
    table_items(items(config, agent_filter), width)
}

fn table_items(items: Vec<Item>, width: usize) -> String {
    if items.is_empty() {
        return String::new();
    }
    let width = width.max(1);
    if width < 19 {
        let mut output = format!("{}\n", truncate("PROFILE", width));
        for item in items {
            output.push_str(&truncate(&item.id, width));
            output.push('\n');
        }
        return output;
    }
    let wide = width >= 100;
    let id_width = items
        .iter()
        .map(|item| display_width(&item.id))
        .max()
        .unwrap_or(7)
        .clamp(7, if wide { 24 } else { (width / 3).max(7) });
    let agent_width = 7;
    let separator_width = if wide { 32 } else { 4 };
    let label_width = width
        .saturating_sub(id_width + agent_width + separator_width)
        .max(1);
    let mut output = if wide {
        format!(
            "{:<id_width$}  {:<agent_width$}  {:<14}  {:<10}  {}\n",
            "PROFILE",
            "AGENT",
            "MODEL",
            "EFFORT",
            truncate("LABEL / TAGS", label_width)
        )
    } else {
        format!(
            "{:<id_width$}  {:<agent_width$}  {}\n",
            "PROFILE",
            "AGENT",
            truncate("LABEL", label_width)
        )
    };
    for item in items {
        let profile = truncate(&item.id, id_width);
        let label = truncate(&item.label, label_width);
        if wide {
            let model = truncate(&item.model, 14);
            let effort = truncate(&item.reasoning, 10);
            let label_and_tags = truncate(&format!("{label} {}", item.tags), label_width);
            output.push_str(&format!(
                "{}  {}  {}  {}  {label_and_tags}\n",
                pad(&profile, id_width),
                pad(&item.agent, agent_width),
                pad(&model, 14),
                pad(&effort, 10),
            ));
        } else {
            output.push_str(&format!(
                "{}  {}  {label}\n",
                pad(&profile, id_width),
                pad(&item.agent, agent_width),
            ));
        }
    }
    output
}

pub fn terminal_width() -> usize {
    crossterm::terminal::size()
        .map(|(width, _)| usize::from(width))
        .ok()
        .or_else(|| {
            std::env::var("COLUMNS")
                .ok()
                .and_then(|value| value.parse().ok())
        })
        .filter(|width| *width > 0)
        .unwrap_or(100)
}

fn items(config: &Config, agent_filter: Option<Agent>) -> Vec<Item> {
    let mut result: Vec<Item> = config
        .profiles
        .iter()
        .filter(|(_, profile)| {
            !profile.disabled && agent_filter.is_none_or(|agent| profile.agent == agent)
        })
        .map(|(id, profile)| item(id, profile))
        .collect();
    result.sort_by(|left, right| {
        left.order
            .cmp(&right.order)
            .then_with(|| left.id.cmp(&right.id))
    });
    result
}

fn item(id: &str, profile: &Profile) -> Item {
    let id = display_field(id);
    let label = profile
        .label
        .as_deref()
        .map(display_field)
        .unwrap_or_else(|| id.clone());
    let description = profile.description.as_deref().map(display_field);
    let tags = profile
        .tags
        .iter()
        .map(|tag| display_field(tag))
        .collect::<Vec<_>>()
        .join(", ");
    let agent = profile.agent.executable();
    let endpoint = display_field(&profile.endpoint);
    let model = profile
        .model
        .as_deref()
        .map(display_field)
        .unwrap_or_else(|| "native default".to_owned());
    let reasoning = profile
        .reasoning
        .as_deref()
        .map(display_field)
        .unwrap_or_else(|| "native default".to_owned());
    let display = format!(
        "{id}  {label}  {agent}  {endpoint}  {model}  {reasoning}  {tags}  {}",
        description.as_deref().unwrap_or_default()
    );
    let mut preview = format!(
        "{label}\n\nProfile: {id}\nAgent: {agent}\nEndpoint: {endpoint}\nModel: {model}\nReasoning: {reasoning}"
    );
    if !tags.is_empty() {
        preview.push_str(&format!("\nTags: {tags}"));
    }
    if let Some(description) = description.filter(|description| !description.is_empty()) {
        preview.push_str(&format!("\n\n{description}"));
    }
    Item {
        id,
        order: profile.order.unwrap_or(0),
        label,
        agent: agent.to_owned(),
        model,
        reasoning,
        tags,
        display,
        preview,
    }
}

fn select_builtin(items: Vec<Item>, last_profile: Option<&str>) -> Result<Option<String>> {
    let page_size = builtin_page_size(items.len());
    let starting_cursor = last_profile
        .and_then(|last| items.iter().position(|item| item.id == last))
        .unwrap_or(0);
    match inquire::Select::new("Agent profile", items)
        .with_help_message("Type to filter · ↑↓ to move · Enter to launch · Esc to cancel")
        .with_page_size(page_size)
        .with_starting_cursor(starting_cursor)
        .prompt()
    {
        Ok(item) => Ok(Some(item.id)),
        Err(
            inquire::InquireError::OperationCanceled | inquire::InquireError::OperationInterrupted,
        ) => Ok(None),
        Err(error) => Err(error.into()),
    }
}

fn select_fzf(
    config: &Config,
    config_path: &Path,
    last_profile: Option<&str>,
    initial_agent: Option<Agent>,
    initial_endpoint: Option<&str>,
) -> Result<Option<Selection>> {
    let executable = launcher::locate(Path::new("fzf")).context("fzf selector is configured but fzf is unavailable; set selector = \"builtin\" to use the built-in selector")?;
    let current_exe =
        std::env::current_exe().context("Cannot resolve Nomad executable for fzf preview")?;
    let preview = format!(
        "{} --config {} __selector-preview -- {{1}}",
        shell_quote_path(&current_exe)?,
        shell_quote_path(config_path)?
    );
    let mut agent = initial_agent;
    let mut endpoint = initial_endpoint.map(str::to_owned);
    loop {
        let items = items(config, agent);
        if items.is_empty() {
            bail!("No enabled profiles match the selected agent");
        }
        let result = run_fzf_profiles(FzfProfileRequest {
            executable: &executable,
            items: &items,
            preview: &preview,
            last_profile,
            bindings: &config.keybindings.fzf,
            agent,
            endpoint: endpoint.as_deref(),
            agent_choice_enabled: initial_agent.is_none(),
        })?;
        match result {
            FzfResult::Selected(profile) => {
                return Ok(Some(Selection { profile, endpoint }));
            }
            FzfResult::ChooseAgent => {
                let Some(selected) = choose_agent(&executable, &config.keybindings.fzf)? else {
                    continue;
                };
                agent = Some(selected);
                endpoint = None;
            }
            FzfResult::ChooseEndpoint => {
                if agent.is_none() {
                    let Some(selected) = choose_agent(&executable, &config.keybindings.fzf)? else {
                        continue;
                    };
                    agent = Some(selected);
                    endpoint = None;
                }
                let reset_id = follow_profile_endpoint_id(config);
                let mut choices = compatible_endpoints(config, agent);
                if choices.is_empty() {
                    bail!("No compatible endpoints are available for the selected agent");
                }
                choices.insert(
                    0,
                    (reset_id.clone(), FOLLOW_PROFILE_ENDPOINT_LABEL.to_owned()),
                );
                if let Some(selected) =
                    choose_value(&executable, "Endpoint", &choices, &config.keybindings.fzf)?
                {
                    endpoint = (selected != reset_id).then_some(selected);
                }
            }
            FzfResult::Cancelled => return Ok(None),
        }
    }
}

enum FzfResult {
    Selected(String),
    ChooseAgent,
    ChooseEndpoint,
    Cancelled,
}

struct FzfProfileRequest<'a> {
    executable: &'a Path,
    items: &'a [Item],
    preview: &'a str,
    last_profile: Option<&'a str>,
    bindings: &'a FzfKeybindings,
    agent: Option<Agent>,
    endpoint: Option<&'a str>,
    agent_choice_enabled: bool,
}

fn run_fzf_profiles(request: FzfProfileRequest<'_>) -> Result<FzfResult> {
    let FzfProfileRequest {
        executable,
        items,
        preview,
        last_profile,
        bindings,
        agent,
        endpoint,
        agent_choice_enabled,
    } = request;
    let mut arguments = vec![
        "--read0".to_owned(),
        "--print0".to_owned(),
        "--print-query".to_owned(),
        "--prompt=Nomad> ".to_owned(),
        "--height=90%".to_owned(),
        "--layout=reverse".to_owned(),
        "--border=rounded".to_owned(),
        "--info=inline".to_owned(),
        format!(
            "--header={}",
            profile_help(bindings, agent, endpoint, agent_choice_enabled)
        ),
        "--delimiter=\\t".to_owned(),
        "--with-nth=2".to_owned(),
        "--preview-window=right:55%:wrap".to_owned(),
        format!("--bind={}:accept", bindings.accept),
        format!("--bind={}:abort", bindings.cancel),
        format!("--bind={}:up", bindings.previous),
        format!("--bind={}:down", bindings.next),
        format!("--bind={}:toggle-preview", bindings.toggle_preview),
        format!(
            "--bind={}:change-preview-window(down:65%:wrap)",
            bindings.preview_below
        ),
        format!(
            "--bind={}:change-preview-window(right:55%:wrap)",
            bindings.preview_right
        ),
        "--bind=ctrl-c:abort".to_owned(),
    ];
    if agent_choice_enabled {
        arguments.push(format!(
            "--bind={}:change-query({CHOOSE_AGENT_QUERY})+first+accept",
            bindings.choose_agent
        ));
    }
    arguments.push(format!(
        "--bind={}:change-query({CHOOSE_ENDPOINT_QUERY})+first+accept",
        bindings.choose_endpoint
    ));
    if supports_no_tty_default(executable) {
        arguments.push("--no-tty-default".to_owned());
    }
    if let Some(position) =
        last_profile.and_then(|last| items.iter().position(|item| item.id == last))
    {
        arguments.push(format!("--bind=load:pos({})", position + 1));
    }
    let entries = items
        .iter()
        .map(|item| {
            (
                item.id.clone(),
                format!(
                    "{}{}{}",
                    item.display, CHOOSE_AGENT_QUERY, CHOOSE_ENDPOINT_QUERY
                ),
            )
        })
        .collect::<Vec<_>>();
    let output = run_fzf(
        executable,
        arguments,
        Some(preview),
        entries
            .iter()
            .map(|(id, display)| (id.as_str(), display.as_str())),
    )?;
    parse_profile_output(items, bindings, output)
}

fn run_fzf<'a>(
    executable: &Path,
    arguments: Vec<String>,
    preview: Option<&str>,
    entries: impl IntoIterator<Item = (&'a str, &'a str)>,
) -> Result<std::process::Output> {
    let mut command = Command::new(executable);
    command
        .args(&arguments)
        .env_remove("FZF_DEFAULT_COMMAND")
        .env_remove("FZF_DEFAULT_OPTS")
        .env_remove("FZF_DEFAULT_OPTS_FILE")
        .env("SHELL", "/bin/sh")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped());
    if let Some(preview) = preview {
        command.arg("--preview").arg(preview);
    }
    let mut child = command.spawn().context("Cannot start fzf")?;
    let mut input = child.stdin.take().context("Cannot open fzf input")?;
    for (id, display) in entries {
        input.write_all(id.as_bytes())?;
        input.write_all(b"\t")?;
        input.write_all(display.as_bytes())?;
        input.write_all(&[0])?;
    }
    drop(input);
    child.wait_with_output().context("Cannot wait for fzf")
}

fn parse_profile_output(
    items: &[Item],
    _bindings: &FzfKeybindings,
    output: std::process::Output,
) -> Result<FzfResult> {
    if !output.status.success() {
        if matches!(output.status.code(), Some(1 | 130)) {
            return Ok(FzfResult::Cancelled);
        }
        bail!("fzf failed with status {}", output.status);
    }
    let selected = String::from_utf8(output.stdout).context("fzf returned non-UTF-8 output")?;
    let (event, record) = fzf_event_and_record(&selected)?;
    if event == Some(CHOOSE_AGENT_QUERY) {
        return Ok(FzfResult::ChooseAgent);
    }
    if event == Some(CHOOSE_ENDPOINT_QUERY) {
        return Ok(FzfResult::ChooseEndpoint);
    }
    let id = record
        .split_once('\t')
        .map(|(id, _)| id)
        .context("fzf returned an invalid selection")?;
    if !items.iter().any(|item| item.id == id) {
        bail!("fzf returned a profile that Nomad did not offer");
    }
    Ok(FzfResult::Selected(id.to_owned()))
}

fn fzf_event_and_record(selected: &str) -> Result<(Option<&str>, &str)> {
    let selected = selected
        .strip_suffix('\0')
        .context("fzf returned no selected profile")?;
    if selected.contains('\0') {
        let mut parts = selected.split('\0');
        let event = parts.next().unwrap_or_default();
        let record = parts.next().context("fzf returned no selected profile")?;
        if parts.next().is_some() {
            bail!("fzf returned multiple selections");
        }
        return Ok((Some(event), record));
    }
    if let Some((event, record)) = selected.split_once('\n') {
        return Ok((Some(event), record));
    }
    Ok((None, selected))
}

fn profile_help(
    bindings: &FzfKeybindings,
    agent: Option<Agent>,
    endpoint: Option<&str>,
    agent_choice_enabled: bool,
) -> String {
    let agent = agent.map(Agent::executable).unwrap_or("all");
    let endpoint = endpoint.unwrap_or("profile default");
    let agent_choice = if agent_choice_enabled {
        format!(" · {} agent", bindings.choose_agent)
    } else {
        "".to_owned()
    };
    format!(
        "Type to filter · {} launch{agent_choice} · {} endpoint · {} narrow preview · {} wide preview · {} hide/show preview · {} cancel · Agent: {agent} · Endpoint: {endpoint}",
        bindings.accept,
        bindings.choose_endpoint,
        bindings.preview_below,
        bindings.preview_right,
        bindings.toggle_preview,
        bindings.cancel,
    )
}

fn choose_agent(executable: &Path, bindings: &FzfKeybindings) -> Result<Option<Agent>> {
    let choices = [
        ("claude", "Claude".to_owned()),
        ("codex", "Codex".to_owned()),
    ];
    let selected = choose_value(executable, "Agent", &choices, bindings)?;
    Ok(selected.and_then(|value| match value.as_str() {
        "claude" => Some(Agent::Claude),
        "codex" => Some(Agent::Codex),
        _ => None,
    }))
}

fn compatible_endpoints(config: &Config, agent: Option<Agent>) -> Vec<(String, String)> {
    config
        .endpoints
        .iter()
        .filter(|(_, endpoint)| agent.is_none_or(|agent| endpoint_is_compatible(agent, endpoint)))
        .map(|(name, endpoint)| {
            let auth = match endpoint {
                Endpoint::Native => "native",
                Endpoint::ApiKey { .. } => "api-key",
            };
            (name.clone(), format!("{name} ({auth})"))
        })
        .collect()
}

fn follow_profile_endpoint_id(config: &Config) -> String {
    let mut candidate = "__nomad_follow_profile_endpoint__".to_owned();
    while config.endpoints.contains_key(&candidate) {
        candidate.push('_');
    }
    candidate
}

fn endpoint_is_compatible(agent: Agent, endpoint: &Endpoint) -> bool {
    matches!(endpoint, Endpoint::Native)
        || matches!(
            (agent, endpoint),
            (
                Agent::Claude,
                Endpoint::ApiKey {
                    protocol: crate::config::Protocol::AnthropicMessages,
                    ..
                }
            ) | (
                Agent::Codex,
                Endpoint::ApiKey {
                    protocol: crate::config::Protocol::OpenaiResponses,
                    ..
                }
            )
        )
}

fn choose_value<T>(
    executable: &Path,
    prompt: &str,
    choices: &[(T, String)],
    bindings: &FzfKeybindings,
) -> Result<Option<String>>
where
    T: AsRef<str>,
{
    let arguments = vec![
        "--read0".to_owned(),
        "--print0".to_owned(),
        format!("--prompt={prompt}> "),
        format!(
            "--header=Type to filter · {} choose · {} cancel",
            bindings.accept, bindings.cancel
        ),
        format!("--bind={}:accept", bindings.accept),
        format!("--bind={}:abort", bindings.cancel),
        format!("--bind={}:up", bindings.previous),
        format!("--bind={}:down", bindings.next),
        "--bind=ctrl-c:abort".to_owned(),
        "--delimiter=\\t".to_owned(),
        "--with-nth=2".to_owned(),
    ];
    let output = run_fzf(
        executable,
        arguments,
        None,
        choices
            .iter()
            .map(|(id, display)| (id.as_ref(), display.as_str())),
    )?;
    if !output.status.success() {
        if matches!(output.status.code(), Some(1 | 130)) {
            return Ok(None);
        }
        bail!("fzf failed with status {}", output.status);
    }
    let selected = String::from_utf8(output.stdout).context("fzf returned non-UTF-8 output")?;
    let (_, record) = fzf_event_and_record(&selected)?;
    let id = record
        .split_once('\t')
        .map(|(id, _)| id)
        .context("fzf returned an invalid selection")?;
    if !choices.iter().any(|(choice, _)| choice.as_ref() == id) {
        bail!("fzf returned a selection that Nomad did not offer");
    }
    Ok(Some(id.to_owned()))
}

pub fn display_field(value: &str) -> String {
    value
        .chars()
        .map(|character| {
            if character.is_control() {
                REPLACEMENT_CHARACTER
            } else {
                character
            }
        })
        .collect()
}

fn shell_quote_path(path: &Path) -> Result<String> {
    let value = path
        .to_str()
        .context("fzf preview requires UTF-8 executable and configuration paths")?;
    Ok(format!("'{}'", value.replace('\'', "'\"'\"'")))
}

fn truncate(value: &str, width: usize) -> String {
    if display_width(value) <= width {
        return value.to_owned();
    }
    if width <= 1 {
        return "…".to_owned();
    }
    let available = width - 1;
    let mut result = String::new();
    let mut used = 0;
    for character in value.chars() {
        let character_width = UnicodeWidthChar::width(character).unwrap_or(0);
        if used + character_width > available {
            break;
        }
        result.push(character);
        used += character_width;
    }
    format!("{result}…")
}

fn builtin_page_size(item_count: usize) -> usize {
    let rows = crossterm::terminal::size()
        .map(|(_, rows)| usize::from(rows))
        .ok()
        .or_else(|| {
            std::env::var("LINES")
                .ok()
                .and_then(|value| value.parse::<usize>().ok())
        })
        .unwrap_or(24);
    rows.saturating_sub(7).clamp(5, 18).min(item_count)
}

fn display_width(value: &str) -> usize {
    UnicodeWidthStr::width(value)
}

fn pad(value: &str, width: usize) -> String {
    format!(
        "{value}{}",
        " ".repeat(width.saturating_sub(display_width(value)))
    )
}

fn supports_no_tty_default(executable: &Path) -> bool {
    let Ok(output) = Command::new(executable).arg("--version").output() else {
        return false;
    };
    let Ok(version) = String::from_utf8(output.stdout) else {
        return false;
    };
    let mut parts = version
        .split_whitespace()
        .next()
        .unwrap_or_default()
        .split('.')
        .filter_map(|part| part.parse::<u32>().ok());
    matches!((parts.next(), parts.next()), (Some(major), Some(minor)) if major > 0 || (major == 0 && minor >= 74))
}

#[cfg(test)]
mod tests {
    use super::{
        builtin_page_size, display_field, display_width, shell_quote_path, table, truncate,
    };
    use crate::config::Config;
    use std::path::Path;

    #[test]
    fn selector_fields_cannot_add_terminal_controls() {
        assert_eq!(display_field("line\n\t\u{1b}[31m"), "line���[31m");
    }

    #[test]
    fn preview_command_paths_are_posix_quoted() {
        assert_eq!(
            shell_quote_path(Path::new("a b/'quoted'")).unwrap(),
            "'a b/'\"'\"'quoted'\"'\"''"
        );
    }

    #[test]
    fn table_cells_are_width_limited() {
        assert_eq!(truncate("abcdefgh", 4), "abc…");
        assert_eq!(truncate("棋子棋子", 5), "棋子…");
    }

    #[test]
    fn builtin_page_size_is_bounded_by_the_list() {
        assert_eq!(builtin_page_size(2), 2);
    }

    #[test]
    fn narrow_table_never_exceeds_terminal_width() {
        let config: Config = toml::from_str(
            "version=1\n[endpoints.official]\nauth='native'\n[profiles.very-long-profile-name]\nagent='codex'\nendpoint='official'\nlabel='棋子棋子棋子棋子'\n",
        )
        .unwrap();
        for line in table(&config, 20).lines() {
            assert!(display_width(line) <= 20, "{line:?}");
        }
    }
}
