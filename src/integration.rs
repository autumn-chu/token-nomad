use crate::{
    config::{Config, state_dir},
    security::MAX_TEXT_BYTES,
    storage,
};
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::path::{Component, Path, PathBuf};

const SCHEMA: u8 = 2;
const CLAUDE_STATUSLINE: &[u8] = include_bytes!("../assets/claude-statusline.sh");
const SHELL_START: &str = "# >>> token-nomad shell >>>";
const SHELL_END: &str = "# <<< token-nomad shell <<<";
const SHELL_LINE: &str = "if ! command -v ap >/dev/null 2>&1; then alias ap='nomad'; fi";
const SHELL_BLOCK: &str = "# >>> token-nomad shell >>>\nif ! command -v ap >/dev/null 2>&1; then alias ap='nomad'; fi\n# <<< token-nomad shell <<<\n";
const CODEX_STATUS_LINE: [&str; 10] = [
    "five-hour-limit",
    "weekly-limit",
    "current-dir",
    "context-used",
    "git-branch",
    "model-with-reasoning",
    "total-input-tokens",
    "total-output-tokens",
    "task-progress",
    "thread-title",
];

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct Operation {
    schema: u8,
    component: String,
    target: PathBuf,
    file_before: FileBefore,
    file_after_sha256: String,
    change: ManagedChange,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct FileBefore {
    existed: bool,
    mode: u32,
    sha256: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "kebab-case", deny_unknown_fields)]
enum ManagedChange {
    InstallFile {
        source: InstallSource,
        installed_sha256: String,
        already_installed: bool,
    },
    ShellBlock {
        already_installed: bool,
        prefix_newlines: u8,
    },
    ClaudeSettings {
        before: ClaudeFields,
        after_command: String,
    },
    CodexStatusLine {
        before: Option<Vec<String>>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "kebab-case", deny_unknown_fields)]
enum InstallSource {
    Embedded,
    File { path: PathBuf, sha256: String },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct ClaudeFields {
    status_line_existed: bool,
    type_was_command: bool,
    command: Option<String>,
}

struct RegularFile {
    bytes: Vec<u8>,
    mode: u32,
}

pub fn atomic_write(path: &Path, bytes: &[u8]) -> Result<()> {
    storage::atomic_write(&absolute_path(path)?, bytes, 0o600)
}

fn atomic_write_mode(path: &Path, bytes: &[u8], mode: u32) -> Result<()> {
    storage::atomic_write_mode(&absolute_path(path)?, bytes, mode)
}

fn absolute_path(path: &Path) -> Result<PathBuf> {
    let path = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()?.join(path)
    };
    if path.components().any(|component| {
        matches!(
            component,
            Component::ParentDir | Component::CurDir | Component::Prefix(_)
        )
    }) {
        bail!("Integration paths may not traverse directories");
    }
    Ok(path)
}

fn inspect_regular(path: &Path, bounded: bool) -> Result<Option<RegularFile>> {
    let path = absolute_path(path)?;
    let limit = if bounded {
        MAX_TEXT_BYTES
    } else {
        u64::MAX - 1
    };
    storage::read_regular(&path, limit).map(|file| {
        file.map(|file| RegularFile {
            bytes: file.bytes,
            mode: file.mode,
        })
    })
}

fn read_text(path: &Path) -> Result<(String, FileBefore)> {
    match inspect_regular(path, true)? {
        Some(file) => {
            let sha256 = digest(&file.bytes);
            Ok((
                String::from_utf8(file.bytes).map_err(|_| {
                    anyhow::anyhow!(
                        "Integration target must be UTF-8 text at {}",
                        path.display()
                    )
                })?,
                FileBefore {
                    existed: true,
                    mode: file.mode,
                    sha256: Some(sha256),
                },
            ))
        }
        None => Ok((
            String::new(),
            FileBefore {
                existed: false,
                mode: 0o600,
                sha256: None,
            },
        )),
    }
}

fn unique_id() -> Result<String> {
    Ok(format!(
        "{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_nanos()
    ))
}
fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn cleanup_file(path: &Path, operation: &str) -> Result<()> {
    storage::remove_file_if_exists(path)
        .with_context(|| format!("{operation} at {}", path.display()))
}

fn finish_with_cleanup<T>(result: Result<T>, cleanup: Result<()>) -> Result<T> {
    match (result, cleanup) {
        (Ok(value), Ok(())) => Ok(value),
        (Err(primary), Ok(())) => Err(primary),
        (Ok(_), Err(cleanup)) => Err(cleanup),
        (Err(primary), Err(cleanup)) => {
            Err(primary.context(format!("cleanup also failed: {cleanup:#}")))
        }
    }
}

fn desired(config: &Config, component: &str) -> Result<Operation> {
    let item = config
        .integrations
        .get(component)
        .context("Integration is not configured; set its target and optional source first")?;
    let target = absolute_path(&item.target)?;
    let (file_before, file_after_sha256, change) = match component {
        "claude-statusline" => desired_claude_statusline(&target, item.source.as_deref())?,
        "claude-settings" => desired_claude_settings(
            &target,
            item.source.as_deref(),
            item.expected_command.as_deref(),
        )?,
        "shell" => desired_shell(&target)?,
        "codex-statusline" => desired_codex(&target)?,
        _ => bail!(
            "Unknown integration component; supported values are shell, claude-statusline, claude-settings, and codex-statusline"
        ),
    };
    Ok(Operation {
        schema: SCHEMA,
        component: component.to_owned(),
        target,
        file_before,
        file_after_sha256,
        change,
    })
}

fn desired_claude_statusline(
    target: &Path,
    source: Option<&Path>,
) -> Result<(FileBefore, String, ManagedChange)> {
    let (source, bytes) = match source {
        Some(source) => {
            let source = absolute_path(source)?;
            if source == target {
                bail!("Claude statusline source and target must differ");
            }
            let file = inspect_regular(&source, true)?
                .context("Claude statusline source does not exist")?;
            let sha256 = digest(&file.bytes);
            (
                InstallSource::File {
                    path: source,
                    sha256,
                },
                file.bytes,
            )
        }
        None => (InstallSource::Embedded, CLAUDE_STATUSLINE.to_vec()),
    };
    let installed_sha256 = digest(&bytes);
    let current = inspect_regular(target, true)?;
    let already_installed = current
        .as_ref()
        .is_some_and(|file| digest(&file.bytes) == installed_sha256);
    if current.is_some() && !already_installed {
        bail!(
            "Existing Claude statusline file differs; resolve it before enabling this integration"
        );
    }
    Ok((
        FileBefore {
            existed: current.is_some(),
            mode: current.as_ref().map_or(0o600, |file| file.mode),
            sha256: current.as_ref().map(|file| digest(&file.bytes)),
        },
        installed_sha256.clone(),
        ManagedChange::InstallFile {
            source,
            installed_sha256,
            already_installed,
        },
    ))
}

fn desired_shell(target: &Path) -> Result<(FileBefore, String, ManagedChange)> {
    let (text, file_before) = read_text(target)?;
    let occurrences = text.matches(SHELL_BLOCK).count();
    let has_marker = text.contains(SHELL_START) || text.contains(SHELL_END);
    if occurrences > 1 || (has_marker && occurrences != 1) {
        bail!("Existing Nomad shell block is unsafe to manage; resolve it manually");
    }
    if occurrences == 0 && text.lines().any(|line| line == SHELL_LINE) {
        bail!(
            "Legacy Nomad shell content requires manual migration before it can be managed safely"
        );
    }
    let prefix_newlines = if occurrences == 1 || text.is_empty() {
        0
    } else if text.ends_with('\n') {
        1
    } else {
        2
    };
    let mut after = text;
    if occurrences == 0 {
        after.push_str(&"\n".repeat(usize::from(prefix_newlines)));
        after.push_str(SHELL_BLOCK);
    }
    Ok((
        file_before,
        digest(after.as_bytes()),
        ManagedChange::ShellBlock {
            already_installed: occurrences == 1,
            prefix_newlines,
        },
    ))
}

fn desired_codex(target: &Path) -> Result<(FileBefore, String, ManagedChange)> {
    let (text, file_before) = read_text(target)?;
    let mut doc = text
        .parse::<toml_edit::DocumentMut>()
        .map_err(|_| anyhow::anyhow!("Invalid existing Codex TOML"))?;
    if doc.get("tui").is_some_and(|value| !value.is_table_like()) {
        bail!("Existing Codex tui setting is not a table");
    }
    let before = codex_status_line(&doc)?;
    if let Some(values) = &before
        && !is_codex_status_line(values)
    {
        bail!("Existing Codex status_line requires explicit conflict resolution");
    }
    if !before.as_deref().is_some_and(is_codex_status_line) {
        set_codex_status_line(
            &mut doc,
            Some(
                CODEX_STATUS_LINE
                    .iter()
                    .map(|value| (*value).to_owned())
                    .collect(),
            ),
        )?;
    }
    let after_sha256 = digest(doc.to_string().as_bytes());
    Ok((
        file_before,
        after_sha256,
        ManagedChange::CodexStatusLine { before },
    ))
}

fn desired_claude_settings(
    target: &Path,
    source: Option<&Path>,
    expected_command: Option<&str>,
) -> Result<(FileBefore, String, ManagedChange)> {
    let source = absolute_path(
        source.context("Claude settings integration requires the renderer source path")?,
    )?;
    let source_file = inspect_regular(&source, true)?.context("Renderer source does not exist")?;
    if source_file.bytes.is_empty() {
        bail!("Renderer source must not be empty");
    }
    let source_text = source
        .to_str()
        .context("Renderer source path must be valid UTF-8")?;
    if !safe_renderer_path(source_text) {
        bail!("Renderer source path contains unsupported characters");
    }
    let command = format!("bash '{source_text}'");
    let (text, file_before) = read_text(target)?;
    let mut settings: serde_json::Value = if !file_before.existed {
        serde_json::json!({})
    } else {
        serde_json::from_str(&text).map_err(|_| anyhow::anyhow!("Invalid existing Claude JSON"))?
    };
    let object = settings
        .as_object_mut()
        .context("Claude settings must be a JSON object")?;
    let before = claude_fields(object)?;
    if let Some(existing) = before.command.as_deref()
        && existing != command
    {
        if expected_command != Some(existing) {
            bail!(
                "Existing Claude statusLine differs; specify its exact expected_command after resolving this conflict"
            );
        }
        if !safe_renderer_command(existing) {
            bail!(
                "Existing Claude statusLine command is unsafe to journal; restore it manually before applying"
            );
        }
    }
    let status = object
        .entry("statusLine")
        .or_insert_with(|| serde_json::json!({}))
        .as_object_mut()
        .context("Existing Claude statusLine must be an object")?;
    status.insert("type".into(), serde_json::json!("command"));
    status.insert("command".into(), serde_json::json!(&command));
    let mut after = serde_json::to_vec_pretty(&settings)?;
    after.push(b'\n');
    Ok((
        file_before,
        digest(&after),
        ManagedChange::ClaudeSettings {
            before,
            after_command: command,
        },
    ))
}

fn safe_renderer_path(value: &str) -> bool {
    value.starts_with('/')
        && value.len() <= 4096
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"/._- ".contains(&byte))
}
fn safe_renderer_command(value: &str) -> bool {
    if value.len() > 4103 || value.bytes().any(|byte| byte.is_ascii_control()) {
        return false;
    }
    let Some(argument) = value.strip_prefix("bash ") else {
        return false;
    };
    let path = argument
        .strip_prefix('\'')
        .and_then(|rest| rest.strip_suffix('\''))
        .unwrap_or(argument);
    safe_renderer_path(path)
}

fn is_change(operation: &Operation) -> bool {
    match &operation.change {
        ManagedChange::InstallFile {
            already_installed, ..
        }
        | ManagedChange::ShellBlock {
            already_installed, ..
        } => !already_installed,
        ManagedChange::ClaudeSettings {
            before,
            after_command,
        } => !before.type_was_command || before.command.as_deref() != Some(after_command),
        ManagedChange::CodexStatusLine { before } => {
            !before.as_deref().is_some_and(is_codex_status_line)
        }
    }
}

fn private_directory(path: &Path) -> Result<PathBuf> {
    let path = absolute_path(path)?;
    storage::ensure_private_dir(&path)?;
    Ok(path)
}

fn private_state_dir() -> Result<PathBuf> {
    private_directory(&state_dir()?)
}

fn encode_operation(operation: &Operation) -> Result<Vec<u8>> {
    let bytes = serde_json::to_vec(operation)?;
    if bytes.len() as u64 > MAX_TEXT_BYTES {
        bail!("Integration operation exceeds the size limit");
    }
    Ok(bytes)
}

fn decode_operation(bytes: &[u8]) -> Result<Operation> {
    if bytes.len() as u64 > MAX_TEXT_BYTES {
        bail!("Integration operation exceeds the size limit");
    }
    let value: serde_json::Value = serde_json::from_slice(bytes)
        .map_err(|_| anyhow::anyhow!("Invalid integration operation record"))?;
    if value.get("before").is_some() || value.get("after").is_some() {
        bail!(
            "Legacy whole-snapshot integration operation detected; restore it manually and leave the operation record in place"
        );
    }
    let operation: Operation = serde_json::from_value(value)
        .map_err(|_| anyhow::anyhow!("Invalid or unsupported integration operation record"))?;
    if operation.schema != SCHEMA {
        bail!("Unsupported integration operation schema; leave the operation record in place");
    }
    validate_operation(&operation)?;
    Ok(operation)
}

fn validate_operation(operation: &Operation) -> Result<()> {
    if !operation.target.is_absolute()
        || operation.file_before.mode > 0o777
        || operation.file_before.existed != operation.file_before.sha256.is_some()
    {
        bail!("Invalid integration operation record");
    }
    if let Some(before) = &operation.file_before.sha256 {
        validate_sha256(before)?;
    }
    validate_sha256(&operation.file_after_sha256)?;
    let component_matches = matches!(
        (&*operation.component, &operation.change),
        ("claude-statusline", ManagedChange::InstallFile { .. })
            | ("shell", ManagedChange::ShellBlock { .. })
            | ("claude-settings", ManagedChange::ClaudeSettings { .. })
            | ("codex-statusline", ManagedChange::CodexStatusLine { .. })
    );
    if !component_matches {
        bail!("Invalid integration operation record");
    }
    match &operation.change {
        ManagedChange::InstallFile {
            source,
            installed_sha256,
            ..
        } => {
            validate_sha256(installed_sha256)?;
            if operation.file_after_sha256 != *installed_sha256 {
                bail!("Invalid integration operation record");
            }
            if let InstallSource::File { path, sha256 } = source {
                if !path.is_absolute() {
                    bail!("Invalid integration operation record");
                }
                validate_sha256(sha256)?;
            }
        }
        ManagedChange::ShellBlock {
            prefix_newlines, ..
        } if *prefix_newlines > 2 => bail!("Invalid integration operation record"),
        ManagedChange::ClaudeSettings {
            before,
            after_command,
        } => {
            if !safe_renderer_command(after_command)
                || before
                    .command
                    .as_deref()
                    .is_some_and(|command| !safe_renderer_command(command))
            {
                bail!("Invalid integration operation record");
            }
        }
        ManagedChange::CodexStatusLine { before }
            if before
                .as_deref()
                .is_some_and(|values| !is_codex_status_line(values)) =>
        {
            bail!("Invalid integration operation record")
        }
        _ => {}
    }
    Ok(())
}

fn validate_sha256(value: &str) -> Result<()> {
    if value.len() != 64 || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        bail!("Invalid integration operation record");
    }
    Ok(())
}

pub fn plan(config: &Config, component: &str) -> Result<()> {
    let operation = desired(config, component)?;
    let root = private_directory(&private_state_dir()?.join("plans"))?;
    atomic_write(
        &root.join(format!("{component}.json")),
        &encode_operation(&operation)?,
    )?;
    println!(
        "component: {component}\ntarget: {}\nchange: {}\nonly managed fields or blocks are recorded; native file contents are never journaled",
        operation.target.display(),
        if is_change(&operation) {
            "update"
        } else {
            "none"
        }
    );
    Ok(())
}

pub fn apply(config: &Config, component: &str) -> Result<()> {
    let state = private_state_dir()?;
    let operations = private_directory(&state.join("operations"))?;
    let _lock = Lock::acquire(&operations)?;
    let operation = desired(config, component)?;
    let preview_path = state.join("plans").join(format!("{component}.json"));
    let preview = decode_operation(
        &inspect_regular(&preview_path, true)?
            .context("Preview this integration with nomad integrate plan first")?
            .bytes,
    )?;
    if !is_change(&operation) {
        println!("Already applied");
        return Ok(());
    }
    if preview != operation {
        bail!(
            "Configuration or managed target state changed since preview; run integrate plan again"
        );
    }
    let id = unique_id()?;
    let operation_path = operations.join(format!("{id}.json"));
    atomic_write(&operation_path, &encode_operation(&operation)?)?;
    if let Err(error) = apply_operation(&operation) {
        let unchanged = desired(config, component).is_ok_and(|current| current == operation);
        let cleanup = if unchanged {
            cleanup_file(
                &operation_path,
                "Failed to remove saved integration operation after an unchanged installation failure",
            )
        } else {
            Ok(())
        };
        let message = if unchanged {
            "Integration installation failed; target remained unchanged"
        } else {
            "Integration installation failed after target state changed; operation record retained for explicit restore"
        };
        return finish_with_cleanup(Err(error).context(message), cleanup);
    }
    println!("Applied {component}; restore with: nomad integrate restore {id}");
    Ok(())
}

fn apply_operation(operation: &Operation) -> Result<()> {
    match &operation.change {
        ManagedChange::InstallFile {
            source,
            installed_sha256,
            already_installed,
        } => {
            if *already_installed {
                return Ok(());
            }
            if inspect_regular(&operation.target, true)?.is_some() {
                bail!("Claude statusline target changed before installation");
            }
            let bytes = match source {
                InstallSource::Embedded => CLAUDE_STATUSLINE.to_vec(),
                InstallSource::File { path, sha256 } => {
                    let bytes = inspect_regular(path, true)?
                        .context("Claude statusline source disappeared before installation")?
                        .bytes;
                    if digest(&bytes) != *sha256 {
                        bail!("Claude statusline source changed since preview");
                    }
                    bytes
                }
            };
            if digest(&bytes) != *installed_sha256 {
                bail!("Claude statusline source does not match the reviewed operation");
            }
            atomic_write_mode(&operation.target, &bytes, operation.file_before.mode)
        }
        ManagedChange::ShellBlock {
            already_installed,
            prefix_newlines,
        } => {
            if *already_installed {
                return Ok(());
            }
            let (mut text, current) = read_text(&operation.target)?;
            if current != operation.file_before
                || text.contains(SHELL_START)
                || text.contains(SHELL_END)
                || text.lines().any(|line| line == SHELL_LINE)
            {
                bail!("Shell managed state changed before installation");
            }
            text.push_str(&"\n".repeat(usize::from(*prefix_newlines)));
            text.push_str(SHELL_BLOCK);
            atomic_write_mode(&operation.target, text.as_bytes(), current.mode)
        }
        ManagedChange::ClaudeSettings {
            before,
            after_command,
        } => {
            let (text, current) = read_text(&operation.target)?;
            if current != operation.file_before {
                bail!("Claude settings file state changed before installation");
            }
            let mut settings: serde_json::Value = if !current.existed {
                serde_json::json!({})
            } else {
                serde_json::from_str(&text)
                    .map_err(|_| anyhow::anyhow!("Invalid existing Claude JSON"))?
            };
            let object = settings
                .as_object_mut()
                .context("Claude settings must be a JSON object")?;
            if claude_fields(object)? != *before {
                bail!("Claude statusLine changed before installation");
            }
            let status = object
                .entry("statusLine")
                .or_insert_with(|| serde_json::json!({}))
                .as_object_mut()
                .context("Existing Claude statusLine must be an object")?;
            status.insert("type".into(), serde_json::json!("command"));
            status.insert("command".into(), serde_json::json!(after_command));
            let mut bytes = serde_json::to_vec_pretty(&settings)?;
            bytes.push(b'\n');
            atomic_write_mode(&operation.target, &bytes, current.mode)
        }
        ManagedChange::CodexStatusLine { before } => {
            let (text, current) = read_text(&operation.target)?;
            if current != operation.file_before {
                bail!("Codex settings file state changed before installation");
            }
            let mut doc = text
                .parse::<toml_edit::DocumentMut>()
                .map_err(|_| anyhow::anyhow!("Invalid existing Codex TOML"))?;
            if codex_status_line(&doc)? != *before {
                bail!("Codex status_line changed before installation");
            }
            set_codex_status_line(
                &mut doc,
                Some(
                    CODEX_STATUS_LINE
                        .iter()
                        .map(|value| (*value).to_owned())
                        .collect(),
                ),
            )?;
            atomic_write_mode(&operation.target, doc.to_string().as_bytes(), current.mode)
        }
    }
}

fn claude_fields(object: &serde_json::Map<String, serde_json::Value>) -> Result<ClaudeFields> {
    let Some(status_value) = object.get("statusLine") else {
        return Ok(ClaudeFields {
            status_line_existed: false,
            type_was_command: false,
            command: None,
        });
    };
    let status = status_value
        .as_object()
        .context("Existing Claude statusLine must be an object")?;
    let type_was_command = match status.get("type") {
        None => false,
        Some(value) if value.as_str() == Some("command") => true,
        Some(_) => bail!("Existing Claude statusLine type is unsafe to manage"),
    };
    let command = match status.get("command") {
        None => None,
        Some(value) => Some(
            value
                .as_str()
                .context("Existing Claude statusLine command is not a string")?
                .to_owned(),
        ),
    };
    Ok(ClaudeFields {
        status_line_existed: true,
        type_was_command,
        command,
    })
}

fn codex_status_line(doc: &toml_edit::DocumentMut) -> Result<Option<Vec<String>>> {
    match doc.get("tui").and_then(|value| value.get("status_line")) {
        None => Ok(None),
        Some(value) => Ok(Some(
            value
                .as_array()
                .context("Existing Codex status_line is not a safe string array")?
                .iter()
                .map(|value| {
                    value
                        .as_str()
                        .map(str::to_owned)
                        .context("Existing Codex status_line is not a safe string array")
                })
                .collect::<Result<Vec<_>>>()?,
        )),
    }
}

fn is_codex_status_line(values: &[String]) -> bool {
    values
        .iter()
        .map(String::as_str)
        .eq(CODEX_STATUS_LINE.iter().copied())
}

fn set_codex_status_line(
    doc: &mut toml_edit::DocumentMut,
    value: Option<Vec<String>>,
) -> Result<()> {
    if let Some(value) = value {
        let mut array = toml_edit::Array::new();
        for entry in value {
            array.push(entry);
        }
        if let Some(tui) = doc.get_mut("tui") {
            tui.as_table_like_mut()
                .context("Existing Codex tui setting is not a table")?
                .insert("status_line", toml_edit::value(array));
        } else {
            let mut tui = toml_edit::Table::new();
            tui.insert("status_line", toml_edit::value(array));
            doc.insert("tui", toml_edit::Item::Table(tui));
        }
    } else if let Some(tui) = doc.get_mut("tui") {
        tui.as_table_like_mut()
            .context("Existing Codex tui setting is not a table")?
            .remove("status_line");
        if tui.as_table_like().is_some_and(|table| table.is_empty()) {
            doc.remove("tui");
        }
    }
    Ok(())
}

pub fn restore(id: &str) -> Result<()> {
    if id.is_empty() || !id.bytes().all(|byte| byte.is_ascii_digit() || byte == b'-') {
        bail!("Invalid operation ID");
    }
    let root = private_directory(&private_state_dir()?.join("operations"))?;
    let _lock = Lock::acquire(&root)?;
    let path = root.join(format!("{id}.json"));
    let operation = decode_operation(
        &inspect_regular(&path, true)?
            .context("Unknown operation")?
            .bytes,
    )?;
    if !is_change(&operation) {
        bail!("Integration operation does not contain an applied managed change");
    }
    if preflight_restore(&operation)? == RestoreState::Applied {
        restore_operation(&operation)
            .context("Integration restore failed; operation record retained")?;
    }
    storage::remove_regular(&path).with_context(|| {
        format!(
            "Failed to remove integration operation record at {}",
            path.display()
        )
    })?;
    println!("Restored {id}");
    Ok(())
}

#[derive(PartialEq, Eq)]
enum RestoreState {
    Applied,
    Unchanged,
}

fn preflight_restore(operation: &Operation) -> Result<RestoreState> {
    let current = inspect_regular(&operation.target, true)?;
    let current_sha256 = current.as_ref().map(|file| digest(&file.bytes));
    if current_sha256 == operation.file_before.sha256 {
        return Ok(RestoreState::Unchanged);
    }
    if current_sha256.as_deref() != Some(operation.file_after_sha256.as_str()) {
        bail!("Target changed after installation; restore refused");
    }
    match &operation.change {
        ManagedChange::InstallFile {
            installed_sha256, ..
        } => {
            let file = inspect_regular(&operation.target, true)?
                .context("Managed Claude statusline file is missing; restore refused")?;
            if digest(&file.bytes) != *installed_sha256 {
                bail!("Managed Claude statusline file changed after installation; restore refused");
            }
        }
        ManagedChange::ShellBlock { .. } => {
            let (text, _) = read_text(&operation.target)?;
            if text.matches(SHELL_BLOCK).count() != 1 {
                bail!("Managed shell block changed after installation; restore refused");
            }
        }
        ManagedChange::ClaudeSettings { after_command, .. } => {
            let (text, current) = read_text(&operation.target)?;
            if !current.existed {
                bail!("Managed Claude settings file is missing; restore refused");
            }
            let settings: serde_json::Value = serde_json::from_str(&text)
                .map_err(|_| anyhow::anyhow!("Invalid existing Claude JSON"))?;
            let fields = claude_fields(
                settings
                    .as_object()
                    .context("Claude settings must be a JSON object")?,
            )?;
            if !fields.type_was_command || fields.command.as_deref() != Some(after_command) {
                bail!("Managed Claude statusLine changed after installation; restore refused");
            }
        }
        ManagedChange::CodexStatusLine { .. } => {
            let (text, current) = read_text(&operation.target)?;
            if !current.existed {
                bail!("Managed Codex settings file is missing; restore refused");
            }
            let doc = text
                .parse::<toml_edit::DocumentMut>()
                .map_err(|_| anyhow::anyhow!("Invalid existing Codex TOML"))?;
            if !codex_status_line(&doc)?
                .as_deref()
                .is_some_and(is_codex_status_line)
            {
                bail!("Managed Codex status_line changed after installation; restore refused");
            }
        }
    }
    Ok(RestoreState::Applied)
}

fn restore_operation(operation: &Operation) -> Result<()> {
    match &operation.change {
        ManagedChange::InstallFile { .. } => remove_regular(&operation.target),
        ManagedChange::ShellBlock {
            prefix_newlines, ..
        } => {
            let (text, current) = read_text(&operation.target)?;
            let start = text
                .find(SHELL_BLOCK)
                .context("Managed shell block is missing")?;
            let prefix = usize::from(*prefix_newlines);
            if start < prefix
                || !text.as_bytes()[start - prefix..start]
                    .iter()
                    .all(|byte| *byte == b'\n')
            {
                bail!("Managed shell block placement changed after installation; restore refused");
            }
            let mut restored = text;
            restored.replace_range(start - prefix..start + SHELL_BLOCK.len(), "");
            write_or_remove_empty(operation, current.mode, restored.as_bytes())
        }
        ManagedChange::ClaudeSettings { before, .. } => {
            let (text, current) = read_text(&operation.target)?;
            let mut settings: serde_json::Value = serde_json::from_str(&text)
                .map_err(|_| anyhow::anyhow!("Invalid existing Claude JSON"))?;
            let object = settings
                .as_object_mut()
                .context("Claude settings must be a JSON object")?;
            let status = object
                .get_mut("statusLine")
                .and_then(serde_json::Value::as_object_mut)
                .context("Managed Claude statusLine is missing")?;
            if before.type_was_command {
                status.insert("type".into(), serde_json::json!("command"));
            } else {
                status.remove("type");
            }
            if let Some(command) = &before.command {
                status.insert("command".into(), serde_json::json!(command));
            } else {
                status.remove("command");
            }
            if !before.status_line_existed && status.is_empty() {
                object.remove("statusLine");
            }
            if !operation.file_before.existed && object.is_empty() {
                return remove_regular(&operation.target);
            }
            let mut bytes = serde_json::to_vec_pretty(&settings)?;
            bytes.push(b'\n');
            atomic_write_mode(&operation.target, &bytes, current.mode)
        }
        ManagedChange::CodexStatusLine { before } => {
            let (text, current) = read_text(&operation.target)?;
            let mut doc = text
                .parse::<toml_edit::DocumentMut>()
                .map_err(|_| anyhow::anyhow!("Invalid existing Codex TOML"))?;
            set_codex_status_line(&mut doc, before.clone())?;
            write_or_remove_empty(operation, current.mode, doc.to_string().as_bytes())
        }
    }
}

fn write_or_remove_empty(operation: &Operation, mode: u32, bytes: &[u8]) -> Result<()> {
    if !operation.file_before.existed && bytes.is_empty() {
        remove_regular(&operation.target)
    } else {
        atomic_write_mode(&operation.target, bytes, mode)
    }
}

fn remove_regular(path: &Path) -> Result<()> {
    storage::remove_regular(path)
}

struct Lock(PathBuf);
impl Lock {
    fn acquire(root: &Path) -> Result<Self> {
        let path = root.join(".lock");
        storage::create_lock(&path).context("Another integration operation is active; inspect operations/.lock if a previous process was interrupted")?;
        Ok(Self(path))
    }
}
impl Drop for Lock {
    fn drop(&mut self) {
        if let Err(error) = cleanup_file(&self.0, "Failed to remove integration lock") {
            eprintln!("nomad: {error:#}");
        }
    }
}
