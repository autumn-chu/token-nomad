use std::{
    env,
    io::Write,
    path::Path,
    process::{Command, Output, Stdio},
};

use serde_json::json;
use tempfile::{TempDir, tempdir};

fn run_renderer_input(root: &TempDir, input: &str, columns: &str) -> Output {
    let project = root.path().join("project");
    std::fs::create_dir_all(&project).unwrap();
    let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/claude-statusline.sh");
    let path = env::var_os("PATH").unwrap_or_default();
    let mut child = Command::new("bash")
        .arg(script)
        .env_clear()
        .env("PATH", path)
        .env("HOME", root.path())
        .env("XDG_CACHE_HOME", root.path().join("cache"))
        .env("XDG_CONFIG_HOME", root.path().join("config"))
        .env("XDG_STATE_HOME", root.path().join("state"))
        .env("CODEX_HOME", root.path().join("codex"))
        .env("CLAUDE_CONFIG_DIR", root.path().join("claude"))
        .env("COLUMNS", columns)
        .current_dir(&project)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(input.as_bytes())
        .unwrap();
    child.wait_with_output().unwrap()
}

fn run_renderer(root: &TempDir, payload: serde_json::Value, columns: &str) -> Output {
    run_renderer_input(root, &payload.to_string(), columns)
}

fn fixture_payload(root: &TempDir) -> serde_json::Value {
    let current_dir = root.path().join("project");
    json!({
        "workspace": {
            "current_dir": current_dir,
            "project_dir": current_dir,
            "added_dirs": []
        },
        "session_id": "fixture-session",
        "context_window": {
            "used_percentage": 42,
            "context_window_size": 100000,
            "total_input_tokens": 1200,
            "total_output_tokens": 300,
            "current_usage": {
                "input_tokens": 1200,
                "cache_creation_input_tokens": 0,
                "cache_read_input_tokens": 300
            }
        },
        "exceeds_200k_tokens": false,
        "cost": {"total_cost_usd": 0.0123},
        "effort": {"level": "medium"},
        "model": {"display_name": "Sonnet\u{1b}[31mInjected"},
        "rate_limits": {
            "five_hour": {"used_percentage": 12},
            "seven_day": {"used_percentage": 5}
        },
        "agent": {"name": "reviewer"}
    })
}

fn visible_chars(value: &str) -> usize {
    let mut chars = value.chars();
    let mut count = 0;
    while let Some(ch) = chars.next() {
        if ch == '\u{1b}' {
            if chars.next() == Some('[') {
                for code in chars.by_ref() {
                    if code.is_ascii_alphabetic() || code == '@' {
                        break;
                    }
                }
            }
        } else if ch != '\n' {
            count += 1;
        }
    }
    count
}

fn strip_ansi(value: &str) -> String {
    let mut chars = value.chars();
    let mut result = String::new();
    while let Some(ch) = chars.next() {
        if ch == '\u{1b}' && chars.next() == Some('[') {
            for code in chars.by_ref() {
                if code.is_ascii_alphabetic() || code == '@' {
                    break;
                }
            }
        } else {
            result.push(ch);
        }
    }
    result
}

#[test]
fn renders_fixture_fields_and_strips_terminal_controls() {
    let root = tempdir().unwrap();
    let output = run_renderer(&root, fixture_payload(&root), "240");
    assert!(output.status.success(), "renderer failed: {:?}", output);
    let text = String::from_utf8(output.stdout).unwrap();
    let visible = strip_ansi(&text);
    assert!(visible.contains("📂 project"));
    assert!(visible.contains("42%"));
    assert!(visible.contains("SonnetInjected"));
    assert!(visible.contains("reviewer"));
    assert!(visible.contains("1k↑/300↓/100k"));
    assert!(visible.contains("cache:20%"));
    assert!(visible.contains("0.0123"));
    assert!(!text.contains("\u{1b}[31mInjected"));
    assert!(
        root.path()
            .join("cache/nomad/claude/git-fixture-session")
            .is_file()
    );
}

#[test]
fn keeps_context_visible_when_the_terminal_is_narrow() {
    let root = tempdir().unwrap();
    let mut payload = fixture_payload(&root);
    payload["context_window"]["used_percentage"] = json!(95);
    payload["model"]["display_name"] = json!("A very long model name used only by this fixture");
    let output = run_renderer(&root, payload, "40");
    assert!(output.status.success(), "renderer failed: {:?}", output);
    let text = String::from_utf8(output.stdout).unwrap();
    assert!(text.contains("95%"));
    assert!(text.contains("🔴"));
    assert!(visible_chars(&text) <= 40 - 12);
}

#[test]
fn preserves_percent_metacharacters_and_promotes_hot_rate_limits() {
    let root = tempdir().unwrap();
    let mut payload = fixture_payload(&root);
    payload["model"]["display_name"] = json!("100% ready");
    payload["rate_limits"]["five_hour"]["used_percentage"] = json!(83);
    payload["rate_limits"]["seven_day"]["used_percentage"] = json!(91);
    let output = run_renderer(&root, payload, "240");
    assert!(output.status.success(), "renderer failed: {:?}", output);
    let text = String::from_utf8(output.stdout).unwrap();
    assert!(text.contains("100% ready"));
    assert!(text.contains("5h:83%"));
    assert!(text.contains("7d:91%"));
}

#[test]
fn handles_missing_and_malformed_payloads_in_the_fixture_cwd() {
    let root = tempdir().unwrap();
    let missing = run_renderer_input(&root, "{}", "120");
    assert!(missing.status.success(), "renderer failed: {:?}", missing);
    assert!(strip_ansi(&String::from_utf8_lossy(&missing.stdout)).contains("📂 /"));

    let malformed = run_renderer_input(&root, "not json", "120");
    assert!(
        malformed.status.success(),
        "renderer failed: {:?}",
        malformed
    );
    assert!(strip_ansi(&String::from_utf8_lossy(&malformed.stdout)).contains("📂 /"));
    assert!(String::from_utf8_lossy(&malformed.stderr).contains("jq:"));
}
