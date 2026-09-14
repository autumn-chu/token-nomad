use portable_pty::{Child, CommandBuilder, MasterPty, PtySize, native_pty_system};
use std::{
    fs,
    io::{Read, Write},
    os::unix::fs::PermissionsExt,
    path::PathBuf,
    sync::{
        Arc, Mutex,
        mpsc::{self, Receiver},
    },
    thread,
    time::{Duration, Instant},
};
use tempfile::TempDir;

struct RunningTui {
    _master: Box<dyn MasterPty>,
    child: ChildGuard,
    writer: Arc<Mutex<Box<dyn Write + Send>>>,
    output: String,
    screen: Arc<Mutex<vt100::Parser>>,
    receiver: Receiver<String>,
}

struct ChildGuard {
    child: Box<dyn Child + Send + Sync>,
    armed: bool,
}

impl ChildGuard {
    fn abort(&mut self) -> bool {
        if !self.armed {
            return true;
        }
        if self.child.try_wait().ok().flatten().is_some() {
            self.armed = false;
            return true;
        }
        let _ = self.child.kill();
        let deadline = Instant::now() + Duration::from_secs(2);
        while Instant::now() < deadline {
            if self.child.try_wait().ok().flatten().is_some() {
                self.armed = false;
                return true;
            }
            thread::sleep(Duration::from_millis(10));
        }
        self.armed = false;
        false
    }
}

impl Drop for ChildGuard {
    fn drop(&mut self) {
        let _ = self.abort();
    }
}

impl RunningTui {
    fn abort(&mut self) -> bool {
        self.child.abort()
    }

    fn screen_contents(&self) -> String {
        self.screen.lock().unwrap().screen().contents()
    }

    fn wait_until(&mut self, description: &str, matches: impl Fn(&str, &str) -> bool) {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let screen = self.screen_contents();
            if matches(&self.output, &screen) {
                return;
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                let output = self.output.clone();
                let child_terminated = self.abort();
                panic!(
                    "timed out waiting for {description:?}; child terminated: {child_terminated}; output: {output}"
                );
            }
            match self.receiver.recv_timeout(remaining) {
                Ok(chunk) => self.output.push_str(&chunk),
                Err(error) => {
                    let output = self.output.clone();
                    let child_terminated = self.abort();
                    panic!(
                        "timed out waiting for {description:?}: {error}; child terminated: {child_terminated}; output: {output}"
                    );
                }
            }
        }
    }

    fn wait_for(&mut self, needle: &str) {
        self.wait_until(needle, |_, screen| screen.contains(needle));
    }

    fn cursor(&self) -> usize {
        self.output.len()
    }

    fn wait_for_after(&mut self, cursor: usize, needle: &str) {
        self.wait_until(needle, |output, screen| {
            output.len() > cursor && screen.contains(needle)
        });
    }

    fn wait_for_output_after(&mut self, cursor: usize) {
        self.wait_until("a fresh render", |output, _| output.len() > cursor);
    }

    fn settle(&mut self) {
        while let Ok(chunk) = self.receiver.try_recv() {
            self.output.push_str(&chunk);
        }
    }

    fn send(&mut self, bytes: &[u8]) {
        let mut writer = self.writer.lock().unwrap();
        writer.write_all(bytes).unwrap();
        writer.flush().unwrap();
    }

    fn resize(&mut self, size: PtySize) {
        self._master.resize(size).unwrap();
        self.screen.lock().unwrap().set_size(size.rows, size.cols);
    }

    fn assert_absent_after(&mut self, cursor: usize, needle: &str, duration: Duration) {
        let deadline = Instant::now() + duration;
        while Instant::now() < deadline {
            let screen = self.screen_contents();
            assert!(
                !screen.contains(needle),
                "unexpected {needle:?} after cursor; screen: {screen}; output: {}",
                &self.output[cursor..],
            );
            let remaining = deadline.saturating_duration_since(Instant::now());
            match self.receiver.recv_timeout(remaining) {
                Ok(chunk) => self.output.push_str(&chunk),
                Err(mpsc::RecvTimeoutError::Timeout) => break,
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
            }
        }
        let screen = self.screen_contents();
        assert!(
            !screen.contains(needle),
            "unexpected {needle:?} after cursor; screen: {screen}; output: {}",
            &self.output[cursor..]
        );
    }

    fn finish(mut self) -> (u32, String) {
        let deadline = Instant::now() + Duration::from_secs(10);
        let status = loop {
            if let Some(status) = self.child.child.try_wait().unwrap() {
                self.child.armed = false;
                break status;
            }
            if Instant::now() >= deadline {
                let output = self.output.clone();
                let child_terminated = self.abort();
                panic!("TUI did not exit; child terminated: {child_terminated}; output: {output}");
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

#[derive(Clone, Copy)]
struct ProfileSpec<'a> {
    id: &'a str,
    agent: &'a str,
    endpoint: &'a str,
    label: &'a str,
    description: &'a str,
    model: Option<&'a str>,
}

fn fake_agent(root: &TempDir) -> PathBuf {
    let path = root.path().join("fake-agent");
    fs::write(
        &path,
        r##"#!/bin/sh
selected=unset
for argument in "$@"; do
  case "$argument" in
    selected=*) selected=${argument#selected=} ;;
  esac
done
printf 'AGENT_SELECTED=%s\n' "$selected"
printf 'AGENT_BASE_URL=%s\n' "${ANTHROPIC_BASE_URL-unset}"
term_state=$(stty -a 2>/dev/null || true)
if printf '%s\n' "$term_state" | grep -Eq '(^|[ ;:])icanon([ ;:]|$)'; then
  printf 'CHILD_CANONICAL=1\n'
else
  printf 'CHILD_CANONICAL=0\n'
fi
if printf '%s\n' "$term_state" | grep -Eq '(^|[ ;:])echo([ ;:]|$)'; then
  printf 'CHILD_ECHO=1\n'
else
  printf 'CHILD_ECHO=0\n'
fi
printf 'CHILD_SIZE=%s\n' "$(stty size 2>/dev/null || printf 'unknown unknown')"
exit 0
"##,
    )
    .unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
    path
}

fn create_root(
    selector: Option<&str>,
    profiles: &[ProfileSpec<'_>],
    tui_bindings: Option<&str>,
    with_api_endpoints: bool,
) -> TempDir {
    let root = tempfile::tempdir().unwrap();
    for directory in [
        "config/nomad",
        "state/nomad",
        "cache",
        "codex-home",
        "claude-home",
    ] {
        fs::create_dir_all(root.path().join(directory)).unwrap();
    }
    let agent = fake_agent(&root);
    let mut config = String::from("version=1\n");
    if let Some(selector) = selector {
        config.push_str(&format!("selector={selector:?}\n"));
    }
    config.push_str(&format!(
        "[executables]\nclaude={agent:?}\ncodex={agent:?}\n[endpoints.official]\nauth='native'\n",
        agent = agent.to_str().unwrap(),
    ));
    if with_api_endpoints {
        config.push_str(
            "[endpoints.claude-api]\nauth='api-key'\nprotocol='anthropic-messages'\nbase_url='https://claude.example/v1'\nkey_env='TEST_CLAUDE_KEY'\n[endpoints.codex-api]\nauth='api-key'\nprotocol='openai-responses'\nbase_url='https://codex.example/v1'\nkey_env='TEST_CODEX_KEY'\n",
        );
    }
    for profile in profiles {
        config.push_str(&format!(
            "[profiles.'{id}']\nagent='{agent}'\nendpoint='{endpoint}'\nlabel='{label}'\ndescription='{description}'\n{model}args=['selected={id}']\n",
            id = profile.id,
            agent = profile.agent,
            endpoint = profile.endpoint,
            label = profile.label,
            description = profile.description,
            model = profile
                .model
                .map(|value| format!("model='{value}'\n"))
                .unwrap_or_default(),
        ));
    }
    if let Some(bindings) = tui_bindings {
        config.push_str("[keybindings.tui]\n");
        config.push_str(bindings);
        config.push('\n');
    }
    fs::write(root.path().join("config/nomad/config.toml"), config).unwrap();
    root
}

fn spawn_tui(root: &TempDir, size: PtySize, arguments: &[&str]) -> RunningTui {
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
    command.env("TEST_CODEX_KEY", "synthetic-test-key");
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
        "AWS_ACCESS_KEY_ID",
        "AWS_SECRET_ACCESS_KEY",
        "AWS_SESSION_TOKEN",
        "AZURE_OPENAI_API_KEY",
        "AZURE_OPENAI_ENDPOINT",
    ] {
        command.env_remove(key);
    }
    let child = pair.slave.spawn_command(command).unwrap();
    drop(pair.slave);
    let mut reader = pair.master.try_clone_reader().unwrap();
    let writer = Arc::new(Mutex::new(pair.master.take_writer().unwrap()));
    let response_writer = Arc::clone(&writer);
    let screen = Arc::new(Mutex::new(vt100::Parser::new(size.rows, size.cols, 0)));
    let response_screen = Arc::clone(&screen);
    let (sender, receiver) = mpsc::channel();
    thread::spawn(move || {
        let mut buffer = [0; 4096];
        let mut pending = Vec::new();
        while let Ok(count) = reader.read(&mut buffer) {
            if count == 0 {
                break;
            }
            let text = String::from_utf8_lossy(&buffer[..count]).into_owned();
            response_screen.lock().unwrap().process(&buffer[..count]);
            let position_queries = position_queries(&mut pending, &buffer[..count]);
            if position_queries > 0
                && let Ok(mut writer) = response_writer.lock()
            {
                for _ in 0..position_queries {
                    let _ = writer.write_all(b"\x1b[1;1R");
                }
                let _ = writer.flush();
            }
            if sender.send(text).is_err() {
                break;
            }
        }
    });
    RunningTui {
        _master: pair.master,
        child: ChildGuard { child, armed: true },
        writer,
        output: String::new(),
        screen,
        receiver,
    }
}

fn position_queries(pending: &mut Vec<u8>, chunk: &[u8]) -> usize {
    const QUERY: &[u8] = b"\x1b[6n";
    pending.extend_from_slice(chunk);
    let count = pending
        .windows(QUERY.len())
        .filter(|window| *window == QUERY)
        .count();
    if pending.len() >= QUERY.len() {
        pending.drain(..pending.len() - (QUERY.len() - 1));
    }
    count
}

fn default_size() -> PtySize {
    PtySize {
        rows: 24,
        cols: 100,
        pixel_width: 0,
        pixel_height: 0,
    }
}

fn wait_for_root(tui: &mut RunningTui) {
    tui.wait_for("Nomad");
}

fn root_query_visible(screen: &str, query: &str) -> bool {
    screen.contains("Nomad")
        && if query.is_empty() {
            screen.contains("Search:")
        } else {
            screen.contains(&format!("Search: {query}"))
        }
}

fn close_help_with_escape(tui: &mut RunningTui, query: &str) {
    tui.settle();
    let phase = tui.cursor();
    tui.send(b"\x1b");
    tui.wait_until("closed help with query preserved", |output, screen| {
        output.len() > phase
            && !screen.contains("Cancel from anywhere")
            && root_query_visible(screen, query)
    });
}

fn close_agent_with_escape(tui: &mut RunningTui, query: &str) {
    tui.settle();
    let phase = tui.cursor();
    tui.send(b"\x1b");
    tui.wait_until(
        "closed agent menu with query preserved",
        |output, screen| {
            output.len() > phase
                && !screen.contains("Filter profiles")
                && root_query_visible(screen, query)
        },
    );
}

fn close_endpoint_with_escape(tui: &mut RunningTui) {
    tui.settle();
    let phase = tui.cursor();
    tui.send(b"\x1b");
    tui.wait_until("returned to endpoint agent step", |output, screen| {
        output.len() > phase && screen.contains("Select agent for temporary endpoint")
    });
    tui.settle();
    let phase = tui.cursor();
    tui.send(b"\x1b");
    tui.wait_until("closed endpoint staging", |output, screen| {
        output.len() > phase
            && !screen.contains("Select agent for temporary endpoint")
            && root_query_visible(screen, "")
    });
}

fn move_modal_down(tui: &mut RunningTui, count: usize) {
    for _ in 0..count {
        tui.send(b"\x1b[B");
    }
}

fn move_modal_up(tui: &mut RunningTui, count: usize) {
    for _ in 0..count {
        tui.send(b"\x1b[A");
    }
}

fn type_text(tui: &mut RunningTui, text: &str) -> usize {
    let mut last_phase = tui.cursor();
    for byte in text.bytes() {
        let phase = tui.cursor();
        last_phase = phase;
        tui.send(&[byte]);
        tui.wait_for_after(phase, &(byte as char).to_string());
    }
    last_phase
}

fn mixed_profiles() -> [ProfileSpec<'static>; 3] {
    [
        ProfileSpec {
            id: "codex-main",
            agent: "codex",
            endpoint: "official",
            label: "Codex main",
            description: "shared profile",
            model: Some("codex-model"),
        },
        ProfileSpec {
            id: "codex-alt",
            agent: "codex",
            endpoint: "official",
            label: "Codex alternate",
            description: "shared profile",
            model: Some("codex-model"),
        },
        ProfileSpec {
            id: "claude-main",
            agent: "claude",
            endpoint: "official",
            label: "Claude main",
            description: "shared profile",
            model: Some("claude-model"),
        },
    ]
}

#[test]
fn terminal_query_scanner_handles_split_multibyte_output() {
    let mut pending = Vec::new();
    let border = "┌".as_bytes();

    assert_eq!(position_queries(&mut pending, &border[..1]), 0);
    assert_eq!(position_queries(&mut pending, &border[1..]), 0);
    assert_eq!(position_queries(&mut pending, b"\x1b["), 0);
    assert_eq!(position_queries(&mut pending, b"6n"), 1);
    assert_eq!(position_queries(&mut pending, b"row\x1b[6n\x1b[6n"), 2);
    assert!(pending.len() <= 3);
}

#[test]
fn omitted_selector_uses_native_tui_and_fuzzy_matches_hidden_description() {
    let profiles = (0..10)
        .map(|index| {
            let id = Box::leak(format!("profile-{index:02}").into_boxed_str());
            let label = Box::leak(format!("Profile {index:02}").into_boxed_str());
            let description = Box::leak(format!("description-only-{index:02}").into_boxed_str());
            ProfileSpec {
                id,
                agent: "codex",
                endpoint: "official",
                label,
                description,
                model: Some("codex-model"),
            }
        })
        .collect::<Vec<_>>();
    let root = create_root(None, &profiles, None, false);
    let mut tui = spawn_tui(&root, default_size(), &[]);
    wait_for_root(&mut tui);
    assert!(tui.output.contains("Search: "), "{}", tui.output);
    type_text(&mut tui, "description-only-09");
    tui.send(b"\r");
    let (code, output) = tui.finish();

    assert_eq!(code, 0, "{output}");
    assert!(output.contains("AGENT_SELECTED=profile-09"), "{output}");
    assert_eq!(
        fs::read_to_string(root.path().join("state/nomad/last-profile")).unwrap(),
        "profile-09"
    );
}

#[test]
fn help_and_agent_cancel_preserve_query_and_cursor() {
    let profiles = mixed_profiles();
    let root = create_root(Some("tui"), &profiles, None, false);
    let mut tui = spawn_tui(&root, default_size(), &[]);
    wait_for_root(&mut tui);
    type_text(&mut tui, "shared");
    tui.send(b"\x1b[B");

    let phase = tui.cursor();
    tui.send(b"\x1bOP");
    tui.wait_for_after(phase, "Help");
    close_help_with_escape(&mut tui, "shared");
    type_text(&mut tui, "x");
    tui.send(&[127]);

    let phase = tui.cursor();
    tui.send(&[12]);
    tui.wait_for_after(phase, "Choose");
    close_agent_with_escape(&mut tui, "shared");
    type_text(&mut tui, "x");
    tui.send(&[127]);
    tui.send(b"\r");
    let (code, output) = tui.finish();

    assert_eq!(code, 0, "{output}");
    assert!(output.contains("AGENT_SELECTED=codex-alt"), "{output}");
    assert_eq!(
        fs::read_to_string(root.path().join("state/nomad/last-profile")).unwrap(),
        "codex-alt"
    );
}

#[test]
fn agent_filter_can_return_to_all_available_agents() {
    let profiles = mixed_profiles();
    let root = create_root(Some("tui"), &profiles, None, false);
    let mut tui = spawn_tui(&root, default_size(), &[]);
    wait_for_root(&mut tui);

    let phase = tui.cursor();
    tui.send(&[12]);
    tui.wait_for_after(phase, "Choose");
    move_modal_down(&mut tui, 2);
    let phase = tui.cursor();
    tui.send(b"\r");
    tui.wait_for_after(phase, "codex");

    let phase = tui.cursor();
    tui.send(&[12]);
    tui.wait_for_after(phase, "Choose");
    move_modal_up(&mut tui, 2);
    let phase = tui.cursor();
    tui.send(b"\r");
    tui.wait_for_after(phase, "all");

    move_modal_up(&mut tui, 1);
    tui.send(b"\r");
    let (code, output) = tui.finish();

    assert_eq!(code, 0, "{output}");
    assert!(output.contains("AGENT_SELECTED=claude-main"), "{output}");
}

#[test]
fn endpoint_stage_cancel_is_transactional_and_compatible_override_can_reset() {
    let profiles = mixed_profiles();
    let root = create_root(Some("tui"), &profiles, None, true);
    let mut tui = spawn_tui(&root, default_size(), &[]);
    wait_for_root(&mut tui);

    let phase = tui.cursor();
    tui.send(&[5]);
    tui.wait_for_after(phase, "Choose");
    tui.send(b"\x1b");
    let phase = tui.cursor();
    tui.wait_for_output_after(phase);
    tui.settle();
    let phase = tui.cursor();
    tui.send(&[5]);
    tui.wait_for_after(phase, "Choose");
    move_modal_down(&mut tui, 1);
    let phase = tui.cursor();
    tui.send(b"\r");
    tui.wait_for_after(phase, "codex-api");
    close_endpoint_with_escape(&mut tui);
    move_modal_down(&mut tui, 2);
    tui.send(b"\r");
    let (code, output) = tui.finish();
    assert_eq!(code, 0, "{output}");
    assert!(output.contains("AGENT_SELECTED=codex-main"), "{output}");
    assert!(output.contains("AGENT_BASE_URL=unset"), "{output}");

    let mut tui = spawn_tui(&root, default_size(), &[]);
    wait_for_root(&mut tui);
    let phase = tui.cursor();
    tui.send(&[5]);
    tui.wait_for_after(phase, "Choose");
    let phase = tui.cursor();
    tui.send(b"\r");
    tui.wait_for_after(phase, "claude-api");
    move_modal_down(&mut tui, 1);
    tui.send(b"\r");
    tui.wait_for("Endpoint: claude-api");
    let phase = tui.cursor();
    tui.send(&[5]);
    tui.wait_for_after(phase, "Choose");
    move_modal_up(&mut tui, 2);
    tui.send(b"\r");
    tui.wait_for("Endpoint: profile default");
    type_text(&mut tui, "claude-main");
    tui.send(b"\r");
    let (code, output) = tui.finish();

    assert_eq!(code, 0, "{output}");
    assert!(output.contains("AGENT_SELECTED=claude-main"), "{output}");
    assert!(output.contains("AGENT_BASE_URL=unset"), "{output}");
}

#[test]
fn cli_agent_filter_cannot_escape_to_another_agent() {
    let profiles = mixed_profiles();
    let root = create_root(Some("tui"), &profiles, None, false);
    let mut tui = spawn_tui(&root, default_size(), &["--agent", "claude"]);
    wait_for_root(&mut tui);
    assert!(tui.output.contains("Agent: claude"), "{}", tui.output);
    let phase = tui.cursor();
    tui.send(&[12]);
    tui.assert_absent_after(phase, "Choose", Duration::from_millis(500));
    let phase = type_text(&mut tui, "codex-main");
    tui.wait_for_after(phase, "No profiles match");
    tui.send(&[3]);
    let (code, output) = tui.finish();

    assert_eq!(code, 0, "{output}");
    assert!(!output.contains("AGENT_SELECTED="), "{output}");
    assert!(!root.path().join("state/nomad/last-profile").exists());
}

#[test]
fn custom_tui_keybindings_dispatch_modal_and_accept() {
    let profiles = mixed_profiles();
    let root = create_root(
        Some("tui"),
        &profiles,
        Some("accept='ctrl-h'\nchoose_agent='ctrl-o'"),
        false,
    );
    let mut tui = spawn_tui(&root, default_size(), &[]);
    wait_for_root(&mut tui);

    let phase = tui.cursor();
    tui.send(&[15]);
    tui.wait_for_after(phase, "Choose");
    close_agent_with_escape(&mut tui, "");
    tui.send(&[8]);
    let (code, output) = tui.finish();

    assert_eq!(code, 0, "{output}");
    assert!(output.contains("AGENT_SELECTED=claude-main"), "{output}");
}

#[test]
fn bracketed_multiline_paste_never_launches_a_profile() {
    let profiles = mixed_profiles();
    let root = create_root(Some("tui"), &profiles, None, false);
    let mut tui = spawn_tui(&root, default_size(), &[]);
    wait_for_root(&mut tui);

    let phase = tui.cursor();
    tui.send(b"\x1b[200~codex-main\r\n\x1b[201~");
    tui.wait_for_after(phase, "No profiles match");
    tui.assert_absent_after(phase, "AGENT_SELECTED=", Duration::from_millis(300));
    tui.send(&[3]);
    let (code, output) = tui.finish();

    assert_eq!(code, 0, "{output}");
    assert!(!output.contains("AGENT_SELECTED="), "{output}");
    assert!(!root.path().join("state/nomad/last-profile").exists());
}

#[test]
fn tiny_resize_restores_terminal_before_agent_exec() {
    let profiles = [ProfileSpec {
        id: "codex-main",
        agent: "codex",
        endpoint: "official",
        label: "Codex main",
        description: "tiny terminal",
        model: Some("codex-model"),
    }];
    let root = create_root(Some("tui"), &profiles, None, false);
    let mut tui = spawn_tui(
        &root,
        PtySize {
            rows: 5,
            cols: 22,
            pixel_width: 0,
            pixel_height: 0,
        },
        &[],
    );
    wait_for_root(&mut tui);
    tui.resize(PtySize {
        rows: 15,
        cols: 70,
        pixel_width: 0,
        pixel_height: 0,
    });
    type_text(&mut tui, "x");
    tui.send(&[127]);
    tui.send(b"\r");
    let (code, output) = tui.finish();

    assert_eq!(code, 0, "{output}");
    assert!(output.contains("AGENT_SELECTED=codex-main"), "{output}");
    assert!(output.contains("CHILD_CANONICAL=1"), "{output}");
    assert!(output.contains("CHILD_ECHO=1"), "{output}");
    assert!(output.contains("CHILD_SIZE=15 70"), "{output}");
}
