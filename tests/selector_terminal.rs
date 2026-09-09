use portable_pty::{Child, CommandBuilder, MasterPty, PtySize, native_pty_system};
use std::{
    fs,
    io::{Read, Write},
    os::unix::fs::PermissionsExt,
    process::Command,
    sync::{
        Arc, Mutex,
        mpsc::{self, Receiver},
    },
    thread,
    time::{Duration, Instant},
};
use tempfile::TempDir;

struct RunningSelector {
    _master: Box<dyn MasterPty>,
    child: Box<dyn Child + Send + Sync>,
    writer: Arc<Mutex<Box<dyn Write + Send>>>,
    output: String,
    receiver: Receiver<String>,
}

impl RunningSelector {
    fn abort(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }

    fn wait_until(&mut self, description: &str, matches: impl Fn(&str) -> bool) {
        let deadline = Instant::now() + Duration::from_secs(10);
        while !matches(&self.output) {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                let output = self.output.clone();
                self.abort();
                panic!("timed out waiting for {description:?}; output: {output}");
            }
            match self.receiver.recv_timeout(remaining) {
                Ok(chunk) => self.output.push_str(&chunk),
                Err(error) => {
                    let output = self.output.clone();
                    self.abort();
                    panic!("timed out waiting for {description:?}: {error}; output: {output}");
                }
            }
        }
    }

    fn wait_for(&mut self, needle: &str) {
        self.wait_until(needle, |output| output.contains(needle));
    }

    fn cursor(&self) -> usize {
        self.output.len()
    }

    fn wait_for_after(&mut self, cursor: usize, needle: &str) {
        self.wait_until(needle, |output| output[cursor..].contains(needle));
    }

    fn wait_for_fzf_selection(&mut self, profile: &str) {
        self.wait_until(profile, |output| has_fzf_selection(output, profile));
    }

    fn send(&mut self, bytes: &[u8]) {
        let mut writer = self.writer.lock().unwrap();
        writer.write_all(bytes).unwrap();
        writer.flush().unwrap();
    }

    fn finish(mut self) -> (u32, String) {
        let deadline = Instant::now() + Duration::from_secs(10);
        let status = loop {
            if let Some(status) = self.child.try_wait().unwrap() {
                break status;
            }
            if Instant::now() >= deadline {
                let output = self.output.clone();
                self.abort();
                panic!("selector did not exit; output: {output}");
            }
            thread::sleep(Duration::from_millis(20));
        };
        drop(self.writer);
        drop(self._master);
        while let Ok(chunk) = self.receiver.recv_timeout(Duration::from_millis(25)) {
            self.output.push_str(&chunk);
        }
        (status.exit_code(), self.output)
    }
}

fn has_fzf_selection(output: &str, profile: &str) -> bool {
    output.match_indices(profile).any(|(index, _)| {
        let prefix = &output[..index];
        let Some(style_start) = prefix.rfind("\x1b[") else {
            return false;
        };
        let style = &prefix[style_start..];
        style.ends_with('m') && style.contains('7')
    })
}

fn require_fzf() {
    let output = Command::new("fzf")
        .arg("--version")
        .output()
        .expect("selector PTY tests require fzf on PATH");
    assert!(
        output.status.success(),
        "selector PTY tests require a working fzf on PATH: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn fixture(selector: &str, profile_count: usize) -> TempDir {
    let root = tempfile::tempdir().unwrap();
    fs::create_dir_all(root.path().join("config/nomad")).unwrap();
    fs::create_dir_all(root.path().join("state")).unwrap();
    let agent = root.path().join("fake-agent");
    fs::write(
        &agent,
        "#!/bin/sh\nfor argument in \"$@\"; do\n  case \"$argument\" in\n    selected=*) printf 'SELECTED=%s\\n' \"${argument#selected=}\" ;;\n  esac\ndone\nexit 0\n",
    )
    .unwrap();
    fs::set_permissions(&agent, fs::Permissions::from_mode(0o700)).unwrap();

    let mut config = format!(
        "version=1\nselector={selector:?}\n[executables]\ncodex={:?}\n[endpoints.official]\nauth='native'\n",
        agent.to_str().unwrap()
    );
    for index in 0..profile_count {
        let name = format!("profile-{index:02}");
        config.push_str(&format!(
            "[profiles.'{name}']\nagent='codex'\nendpoint='official'\nlabel='label-{index:02}'\ndescription='description-only-{index:02}'\ntags=['tag-{index:02}']\nmodel='model-{index:02}'\nreasoning='high'\nargs=['selected={name}']\n"
        ));
    }
    fs::write(root.path().join("config/nomad/config.toml"), config).unwrap();
    root
}

fn spawn_selector(root: &TempDir, size: PtySize) -> RunningSelector {
    spawn_selector_with_args(root, size, &[])
}

fn spawn_selector_with_args(root: &TempDir, size: PtySize, arguments: &[&str]) -> RunningSelector {
    let pair = native_pty_system().openpty(size).unwrap();
    let mut command = CommandBuilder::new(env!("CARGO_BIN_EXE_nomad"));
    command.args([
        "--config",
        root.path()
            .join("config/nomad/config.toml")
            .to_str()
            .unwrap(),
    ]);
    command.args(arguments);
    command.cwd(root.path());
    command.env("TERM", "xterm-256color");
    command.env("HOME", root.path());
    command.env("XDG_CONFIG_HOME", root.path().join("config"));
    command.env("XDG_STATE_HOME", root.path().join("state"));
    command.env("XDG_CACHE_HOME", root.path().join("cache"));
    command.env("CODEX_HOME", root.path().join("codex-home"));
    command.env("CLAUDE_CONFIG_DIR", root.path().join("claude-home"));
    command.env("TEST_CLAUDE_KEY", "synthetic-test-key");
    command.env_remove("LINES");
    command.env_remove("COLUMNS");
    if let Some(path) = std::env::var_os("PATH") {
        command.env("PATH", path);
    }
    for key in [
        "ANTHROPIC_API_KEY",
        "ANTHROPIC_AUTH_TOKEN",
        "ANTHROPIC_BASE_URL",
        "ANTHROPIC_CUSTOM_HEADERS",
        "CLAUDE_CODE_OAUTH_TOKEN",
        "CLAUDE_CODE_USE_BEDROCK",
        "CLAUDE_CODE_USE_VERTEX",
        "CLAUDE_CODE_USE_FOUNDRY",
        "GEMINI_API_KEY",
        "GOOGLE_API_KEY",
        "OPENAI_API_KEY",
        "OPENAI_BASE_URL",
    ] {
        command.env_remove(key);
    }
    let child = pair.slave.spawn_command(command).unwrap();
    drop(pair.slave);
    let mut reader = pair.master.try_clone_reader().unwrap();
    let writer = Arc::new(Mutex::new(pair.master.take_writer().unwrap()));
    let response_writer = Arc::clone(&writer);
    let (sender, receiver) = mpsc::channel();
    thread::spawn(move || {
        let mut buffer = [0; 4096];
        while let Ok(count) = reader.read(&mut buffer) {
            if count == 0 {
                break;
            }
            let text = String::from_utf8_lossy(&buffer[..count]).into_owned();
            if text.contains("\x1b[6n")
                && let Ok(mut writer) = response_writer.lock()
            {
                let _ = writer.write_all(b"\x1b[1;1R");
                let _ = writer.flush();
            }
            if sender.send(text).is_err() {
                break;
            }
        }
    });
    RunningSelector {
        _master: pair.master,
        child,
        writer,
        output: String::new(),
        receiver,
    }
}

fn custom_fzf_fixture() -> TempDir {
    let root = tempfile::tempdir().unwrap();
    fs::create_dir_all(root.path().join("config/nomad")).unwrap();
    fs::create_dir_all(root.path().join("state")).unwrap();
    let agent = root.path().join("fake-agent");
    fs::write(
        &agent,
        "#!/bin/sh\nfor argument in \"$@\"; do\n  case \"$argument\" in\n    selected=*) printf 'SELECTED=%s ROUTE=%s\\n' \"${argument#selected=}\" \"${ANTHROPIC_BASE_URL-unset}\" ;;\n  esac\ndone\n",
    )
    .unwrap();
    fs::set_permissions(&agent, fs::Permissions::from_mode(0o700)).unwrap();
    fs::write(
        root.path().join("config/nomad/config.toml"),
        format!(
            "version=1\nselector='fzf'\n[executables]\nclaude={agent:?}\ncodex={agent:?}\n[endpoints.official]\nauth='native'\n[endpoints.claude-api]\nauth='api-key'\nprotocol='anthropic-messages'\nbase_url='https://claude.example/v1'\nkey_env='TEST_CLAUDE_KEY'\n[profiles.codex]\nagent='codex'\nendpoint='official'\norder=-1\nargs=['selected=codex']\n[profiles.claude]\nagent='claude'\nendpoint='official'\nmodel='claude-test'\nargs=['selected=claude']\n[keybindings.fzf]\naccept='ctrl-g'\ncancel='esc'\nprevious='ctrl-n'\nnext='down'\ntoggle_preview='ctrl-p'\npreview_below='ctrl-/'\npreview_right='alt-/'\nchoose_agent='ctrl-o'\nchoose_endpoint='ctrl-y'\n"
        ),
    )
    .unwrap();
    root
}

fn last_profile(root: &TempDir) -> std::path::PathBuf {
    root.path().join("state/nomad/last-profile")
}

fn seed_last_profile(root: &TempDir, profile: &str) {
    fs::create_dir_all(root.path().join("state/nomad")).unwrap();
    fs::write(last_profile(root), profile).unwrap();
}

#[test]
fn builtin_selector_executes_a_profile_in_a_narrow_pty_and_records_metadata() {
    let root = fixture("builtin", 3);
    let mut selector = spawn_selector(
        &root,
        PtySize {
            rows: 12,
            cols: 40,
            pixel_width: 0,
            pixel_height: 0,
        },
    );
    selector.wait_for("label-01");
    selector.wait_for("model-01");
    assert!(selector.output.contains("model-01"), "{}", selector.output);
    selector.send(b"\x1b[B");
    selector.send(b"\r");
    let (code, output) = selector.finish();

    assert_eq!(code, 0, "{output}");
    assert!(output.contains("SELECTED=profile-01"), "{output}");
    assert_eq!(
        fs::read_to_string(last_profile(&root)).unwrap(),
        "profile-01"
    );
}

#[test]
fn builtin_selector_uses_pty_height_to_scroll_many_profiles_without_dimension_env() {
    let root = fixture("builtin", 10);
    let mut selector = spawn_selector(
        &root,
        PtySize {
            rows: 12,
            cols: 80,
            pixel_width: 0,
            pixel_height: 0,
        },
    );
    selector.wait_for("Type to filter");
    assert!(!selector.output.contains("label-09"), "{}", selector.output);
    for _ in 0..9 {
        selector.send(b"\x1b[B");
    }
    selector.wait_for("label-09");
    selector.send(b"\r");
    let (code, output) = selector.finish();

    assert_eq!(code, 0, "{output}");
    assert!(output.contains("SELECTED=profile-09"), "{output}");
    assert_eq!(
        fs::read_to_string(last_profile(&root)).unwrap(),
        "profile-09"
    );
}

#[test]
fn fzf_selector_searches_more_than_seven_profiles_and_executes_the_match() {
    require_fzf();
    let root = fixture("fzf", 10);
    let mut selector = spawn_selector(
        &root,
        PtySize {
            rows: 24,
            cols: 120,
            pixel_width: 0,
            pixel_height: 0,
        },
    );
    selector.wait_for("label-09");
    assert!(selector.output.contains("model-09"), "{}", selector.output);
    selector.send(b"profile-09");
    selector.wait_for("1/10");
    selector.send(b"\r");
    let (code, output) = selector.finish();

    assert_eq!(code, 0, "{output}");
    assert!(output.contains("SELECTED=profile-09"), "{output}");
    assert_eq!(
        fs::read_to_string(last_profile(&root)).unwrap(),
        "profile-09"
    );
}

#[test]
fn fzf_selector_searches_description_only_metadata_and_executes_the_match() {
    require_fzf();
    let root = fixture("fzf", 10);
    let mut selector = spawn_selector(
        &root,
        PtySize {
            rows: 24,
            cols: 120,
            pixel_width: 0,
            pixel_height: 0,
        },
    );
    selector.wait_for("label-09");
    selector.send(b"description-only-09");
    selector.wait_for("1/10");
    selector.send(b"\r");
    let (code, output) = selector.finish();

    assert_eq!(code, 0, "{output}");
    assert!(output.contains("SELECTED=profile-09"), "{output}");
    assert_eq!(
        fs::read_to_string(last_profile(&root)).unwrap(),
        "profile-09"
    );
}

#[test]
fn fzf_selector_honors_last_profile_cursor_without_reordering_profiles() {
    require_fzf();
    let root = fixture("fzf", 10);
    seed_last_profile(&root, "profile-04");
    let mut selector = spawn_selector(
        &root,
        PtySize {
            rows: 24,
            cols: 120,
            pixel_width: 0,
            pixel_height: 0,
        },
    );
    selector.wait_for_fzf_selection("profile-04");
    let profile00 = selector.output.find("profile-00").unwrap();
    let profile04 = selector.output.find("profile-04").unwrap();
    let profile09 = selector.output.find("profile-09").unwrap();
    assert!(
        profile00 < profile04 && profile04 < profile09,
        "{}",
        selector.output
    );
    selector.send(b"\r");
    let (code, output) = selector.finish();

    assert_eq!(code, 0, "{output}");
    assert!(output.contains("SELECTED=profile-04"), "{output}");
    assert_eq!(
        fs::read_to_string(last_profile(&root)).unwrap(),
        "profile-04"
    );
}

#[test]
fn fzf_custom_keybindings_switch_agent_and_choose_a_compatible_endpoint() {
    require_fzf();
    let root = custom_fzf_fixture();
    let mut selector = spawn_selector(
        &root,
        PtySize {
            rows: 24,
            cols: 120,
            pixel_width: 0,
            pixel_height: 0,
        },
    );
    selector.wait_for("codex");
    let phase = selector.cursor();
    selector.send(&[15]);
    selector.wait_for_after(phase, "Agent>");
    let phase = selector.cursor();
    selector.send(b"claude");
    selector.wait_for_after(phase, "1/2");
    let phase = selector.cursor();
    selector.send(&[7]);
    selector.wait_for_after(phase, "Nomad>");
    let phase = selector.cursor();
    selector.send(&[25]);
    selector.wait_for_after(phase, "Endpoint>");
    let phase = selector.cursor();
    selector.send(b"claude-api");
    selector.wait_for_after(phase, "1/3");
    let phase = selector.cursor();
    selector.send(&[7]);
    selector.wait_for_after(phase, "1/1");
    selector.send(&[7]);
    let (code, output) = selector.finish();

    assert_eq!(code, 0, "{output}");
    assert!(
        output.contains("SELECTED=claude ROUTE=https://claude.example/v1"),
        "{output}"
    );
    assert_eq!(fs::read_to_string(last_profile(&root)).unwrap(), "claude");
}

#[test]
fn fzf_endpoint_chooser_can_reset_an_initial_override_to_the_profile_default() {
    require_fzf();
    let root = custom_fzf_fixture();
    let mut selector = spawn_selector_with_args(
        &root,
        PtySize {
            rows: 24,
            cols: 120,
            pixel_width: 0,
            pixel_height: 0,
        },
        &["--agent", "claude", "--endpoint", "claude-api"],
    );
    selector.wait_for("claude");
    let phase = selector.cursor();
    selector.send(&[25]);
    selector.wait_for_after(phase, "Endpoint>");
    let phase = selector.cursor();
    selector.send(b"Follow selected profile endpoint");
    selector.wait_for_after(phase, "1/3");
    let phase = selector.cursor();
    selector.send(&[7]);
    selector.wait_for_after(phase, "1/1");
    selector.send(&[7]);
    let (code, output) = selector.finish();

    assert_eq!(code, 0, "{output}");
    assert!(output.contains("SELECTED=claude ROUTE=unset"), "{output}");
    assert_eq!(fs::read_to_string(last_profile(&root)).unwrap(), "claude");
}

#[test]
fn fzf_endpoint_chooser_cancel_keeps_an_initial_override() {
    require_fzf();
    let root = custom_fzf_fixture();
    let mut selector = spawn_selector_with_args(
        &root,
        PtySize {
            rows: 24,
            cols: 120,
            pixel_width: 0,
            pixel_height: 0,
        },
        &["--agent", "claude", "--endpoint", "claude-api"],
    );
    selector.wait_for("claude");
    let phase = selector.cursor();
    selector.send(&[25]);
    selector.wait_for_after(phase, "Endpoint>");
    let phase = selector.cursor();
    selector.send(b"\x1b");
    selector.wait_for_after(phase, "1/1");
    selector.send(&[7]);
    let (code, output) = selector.finish();

    assert_eq!(code, 0, "{output}");
    assert!(
        output.contains("SELECTED=claude ROUTE=https://claude.example/v1"),
        "{output}"
    );
}

#[test]
fn builtin_selector_cancel_does_not_create_last_profile_state() {
    let root = fixture("builtin", 3);
    let mut selector = spawn_selector(&root, PtySize::default());
    selector.wait_for("Agent profile");
    selector.send(&[3]);
    let (code, output) = selector.finish();

    assert_eq!(code, 0, "{output}");
    assert!(!last_profile(&root).exists());
}

#[test]
fn fzf_selector_cancel_does_not_create_last_profile_state() {
    require_fzf();
    let root = fixture("fzf", 3);
    let mut selector = spawn_selector(
        &root,
        PtySize {
            rows: 10,
            cols: 40,
            pixel_width: 0,
            pixel_height: 0,
        },
    );
    selector.wait_for("profile-00");
    selector.send(&[3]);
    let (code, output) = selector.finish();

    assert_eq!(code, 0, "{output}");
    assert!(!last_profile(&root).exists());
}
