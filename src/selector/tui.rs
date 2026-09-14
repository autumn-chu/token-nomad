use super::{Item, Selection, compatible_endpoints, endpoint_is_compatible, items};
use crate::config::{Agent, Config, TuiKeybindings};
use anyhow::{Context, Result, anyhow, bail};
use crossterm::{
    cursor::{Hide, Show},
    event::{
        self, DisableBracketedPaste, EnableBracketedPaste, Event, KeyCode, KeyEvent, KeyEventKind,
        KeyModifiers,
    },
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use ratatui::{
    Frame, Terminal,
    backend::CrosstermBackend,
    layout::{Alignment, Constraint, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span, Text},
    widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph, Wrap},
};
use std::{io::Stdout, panic::AssertUnwindSafe};

const FOLLOW_PROFILE_ENDPOINT_LABEL: &str = "Follow profile default";
const MAX_QUERY_CHARS: usize = 256;

pub(super) fn select(
    config: &Config,
    last_profile: Option<&str>,
    cli_agent: Option<Agent>,
    initial_endpoint: Option<&str>,
) -> Result<Option<Selection>> {
    let all_items = items(config, None);
    if all_items.is_empty() {
        bail!("No enabled profiles");
    }
    let mut app = App::new(config, all_items, last_profile, cli_agent, initial_endpoint)?;
    let mut session = TerminalSession::enter()?;
    let run_result = std::panic::catch_unwind(AssertUnwindSafe(|| run(&mut session, &mut app)));
    let restore_result = session.restore();
    match run_result {
        Ok(Ok(selection)) => {
            restore_result.context("Cannot restore terminal after native selector")?;
            Ok(selection)
        }
        Ok(Err(error)) => {
            if let Err(restore_error) = restore_result {
                Err(error.context(format!(
                    "The native selector also failed to restore the terminal: {restore_error:#}"
                )))
            } else {
                Err(error)
            }
        }
        Err(payload) => {
            let _ = restore_result;
            std::panic::resume_unwind(payload)
        }
    }
}

fn run(session: &mut TerminalSession, app: &mut App<'_>) -> Result<Option<Selection>> {
    loop {
        session.terminal_mut()?.draw(|frame| render(frame, app))?;
        match event::read().context("Cannot read native selector input")? {
            Event::Key(key) if matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat) => {
                if let Outcome::Finish(selection) = app.handle_key(key) {
                    return Ok(selection);
                }
            }
            Event::Paste(value) => app.handle_paste(&value),
            Event::Resize(_, _) => {}
            _ => {}
        }
    }
}

struct TerminalSession {
    terminal: Option<Terminal<CrosstermBackend<Stdout>>>,
    raw: bool,
    alternate: bool,
    cursor_hidden: bool,
    paste_enabled: bool,
}

impl TerminalSession {
    fn enter() -> Result<Self> {
        let mut session = Self {
            terminal: None,
            raw: false,
            alternate: false,
            cursor_hidden: false,
            paste_enabled: false,
        };
        enable_raw_mode().context("Cannot enable raw mode for native selector")?;
        session.raw = true;
        session.alternate = true;
        execute!(std::io::stdout(), EnterAlternateScreen)
            .context("Cannot enter alternate screen for native selector")?;
        session.paste_enabled = true;
        execute!(std::io::stdout(), EnableBracketedPaste)
            .context("Cannot enable bracketed paste for native selector")?;
        session.cursor_hidden = true;
        execute!(std::io::stdout(), Hide).context("Cannot hide cursor for native selector")?;
        session.terminal = Some(
            Terminal::new(CrosstermBackend::new(std::io::stdout()))
                .context("Cannot initialize native selector terminal")?,
        );
        Ok(session)
    }

    fn terminal_mut(&mut self) -> Result<&mut Terminal<CrosstermBackend<Stdout>>> {
        self.terminal
            .as_mut()
            .context("Native selector terminal is unavailable")
    }

    fn restore(&mut self) -> Result<()> {
        let mut errors = Vec::new();
        if self.cursor_hidden {
            if let Err(error) = execute!(std::io::stdout(), Show) {
                errors.push(format!("show cursor: {error}"));
            } else {
                self.cursor_hidden = false;
            }
        }
        if self.paste_enabled {
            if let Err(error) = execute!(std::io::stdout(), DisableBracketedPaste) {
                errors.push(format!("disable bracketed paste: {error}"));
            } else {
                self.paste_enabled = false;
            }
        }
        if self.alternate {
            if let Err(error) = execute!(std::io::stdout(), LeaveAlternateScreen) {
                errors.push(format!("leave alternate screen: {error}"));
            } else {
                self.alternate = false;
            }
        }
        if self.raw {
            if let Err(error) = disable_raw_mode() {
                errors.push(format!("disable raw mode: {error}"));
            } else {
                self.raw = false;
            }
        }
        self.terminal = None;
        if errors.is_empty() {
            Ok(())
        } else {
            Err(anyhow!(errors.join("; ")))
        }
    }
}

impl Drop for TerminalSession {
    fn drop(&mut self) {
        let _ = self.restore();
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PreviewMode {
    Auto,
    Hidden,
    Below,
    Right,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum AgentPurpose {
    Filter,
    Endpoint,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Modal {
    Agent {
        purpose: AgentPurpose,
        query: String,
        selected: usize,
    },
    Endpoint {
        staged_agent: Option<Agent>,
        query: String,
        selected: usize,
        back_to_agent: bool,
    },
    Help,
}

enum Outcome {
    Continue,
    Finish(Option<Selection>),
}

struct App<'a> {
    config: &'a Config,
    items: Vec<Item>,
    query: String,
    selected_id: Option<String>,
    cursor: usize,
    scroll: usize,
    agent: Option<Agent>,
    agent_locked: bool,
    endpoint: Option<String>,
    preview: PreviewMode,
    preview_scroll: u16,
    modal: Option<Modal>,
    width: u16,
    tiny_modal_blocked: bool,
}

impl<'a> App<'a> {
    fn new(
        config: &'a Config,
        items: Vec<Item>,
        last_profile: Option<&str>,
        cli_agent: Option<Agent>,
        endpoint: Option<&str>,
    ) -> Result<Self> {
        let mut agent = cli_agent;
        if let Some(endpoint_name) = endpoint {
            let configured = config
                .endpoints
                .get(endpoint_name)
                .context("Selected endpoint is not configured")?;
            if let crate::config::Endpoint::ApiKey { protocol, .. } = configured {
                let required_agent = match protocol {
                    crate::config::Protocol::AnthropicMessages => Agent::Claude,
                    crate::config::Protocol::OpenaiResponses => Agent::Codex,
                };
                if cli_agent.is_some_and(|selected| selected != required_agent) {
                    bail!("Selected endpoint is incompatible with --agent");
                }
                agent = Some(required_agent);
            }
        }
        if agent.is_some() && !items.iter().any(|item| item_matches_agent(item, agent)) {
            bail!("No enabled profiles match the selected agent");
        }
        let selected_id = last_profile
            .filter(|last| {
                items
                    .iter()
                    .any(|item| item.id == *last && item_matches_agent(item, agent))
            })
            .map(str::to_owned)
            .or_else(|| {
                items
                    .iter()
                    .find(|item| item_matches_agent(item, agent))
                    .map(|item| item.id.clone())
            });
        let mut app = Self {
            config,
            items,
            query: String::new(),
            selected_id,
            cursor: 0,
            scroll: 0,
            agent,
            agent_locked: cli_agent.is_some(),
            endpoint: endpoint.map(str::to_owned),
            preview: PreviewMode::Auto,
            preview_scroll: 0,
            modal: None,
            width: 120,
            tiny_modal_blocked: false,
        };
        app.sync_cursor();
        Ok(app)
    }

    fn filtered_indices(&self) -> Vec<usize> {
        self.items
            .iter()
            .enumerate()
            .filter(|(_, item)| {
                item_matches_agent(item, self.agent) && fuzzy_matches(&item.display, &self.query)
            })
            .map(|(index, _)| index)
            .collect()
    }

    fn sync_cursor(&mut self) {
        self.preview_scroll = 0;
        let filtered = self.filtered_indices();
        if filtered.is_empty() {
            self.cursor = 0;
            self.scroll = 0;
            return;
        }
        self.cursor = self
            .selected_id
            .as_deref()
            .and_then(|id| {
                filtered
                    .iter()
                    .position(|index| self.items[*index].id == id)
            })
            .unwrap_or(0)
            .min(filtered.len() - 1);
        self.selected_id = Some(self.items[filtered[self.cursor]].id.clone());
        self.scroll = self.scroll.min(self.cursor);
    }

    fn move_selection(&mut self, delta: isize) {
        let filtered = self.filtered_indices();
        if filtered.is_empty() {
            return;
        }
        self.cursor = if delta < 0 {
            self.cursor.saturating_sub(delta.unsigned_abs())
        } else {
            (self.cursor + delta as usize).min(filtered.len() - 1)
        };
        self.selected_id = Some(self.items[filtered[self.cursor]].id.clone());
    }

    fn handle_key(&mut self, key: KeyEvent) -> Outcome {
        let key_name = key_name(key).unwrap_or_default();
        if key_name == "ctrl-c" {
            return Outcome::Finish(None);
        }
        if self.modal.is_some() {
            return self.handle_modal_key(key, &key_name);
        }
        let bindings = &self.config.keybindings.tui;
        if key_name == bindings.accept {
            let filtered = self.filtered_indices();
            return filtered
                .get(self.cursor)
                .map_or(Outcome::Continue, |index| {
                    Outcome::Finish(Some(Selection {
                        profile: self.items[*index].id.clone(),
                        endpoint: self.endpoint.clone(),
                    }))
                });
        }
        if key_name == bindings.cancel {
            if self.query.is_empty() {
                return Outcome::Finish(None);
            }
            self.query.clear();
            self.sync_cursor();
            return Outcome::Continue;
        }
        if key_name == bindings.previous {
            self.move_selection(-1);
        } else if key_name == bindings.next {
            self.move_selection(1);
        } else if key_name == bindings.toggle_preview {
            self.preview = if effective_preview(self.preview, self.width) == PreviewMode::Hidden {
                if self.width < 80 {
                    PreviewMode::Below
                } else {
                    PreviewMode::Auto
                }
            } else {
                PreviewMode::Hidden
            };
        } else if key_name == bindings.preview_below {
            self.preview = PreviewMode::Below;
        } else if key_name == bindings.preview_right {
            self.preview = PreviewMode::Right;
        } else if key_name == bindings.choose_agent && !self.agent_locked {
            self.modal = Some(Modal::Agent {
                purpose: AgentPurpose::Filter,
                query: String::new(),
                selected: self.agent_choice_position(true, self.agent),
            });
        } else if key_name == bindings.choose_endpoint {
            if let Some(agent) = self.agent {
                self.open_endpoint(Some(agent), false);
            } else {
                self.modal = Some(Modal::Agent {
                    purpose: AgentPurpose::Endpoint,
                    query: String::new(),
                    selected: 0,
                });
            }
        } else if key_name == bindings.help {
            self.modal = Some(Modal::Help);
        } else if key_name == "pgup"
            && !action_uses_key(bindings, "pgup")
            && effective_preview(self.preview, self.width) != PreviewMode::Hidden
        {
            self.preview_scroll = self.preview_scroll.saturating_sub(3);
        } else if key_name == "pgdn"
            && !action_uses_key(bindings, "pgdn")
            && effective_preview(self.preview, self.width) != PreviewMode::Hidden
        {
            self.preview_scroll = self.preview_scroll.saturating_add(3);
        } else {
            self.edit_root_query(key, &key_name);
        }
        Outcome::Continue
    }

    fn edit_root_query(&mut self, key: KeyEvent, key_name: &str) {
        if key_name == "backspace" && !self.query.is_empty() {
            self.query.pop();
            self.sync_cursor();
            return;
        }
        match key.code {
            KeyCode::Char(character)
                if key.modifiers.difference(KeyModifiers::SHIFT).is_empty() =>
            {
                push_query_character(&mut self.query, character);
                self.sync_cursor();
            }
            _ => {}
        }
    }

    fn handle_paste(&mut self, value: &str) {
        let value = super::display_field(value);
        match &mut self.modal {
            None => {
                push_query_text(&mut self.query, &value);
                self.sync_cursor();
            }
            Some(Modal::Agent { query, .. }) | Some(Modal::Endpoint { query, .. }) => {
                push_query_text(query, &value);
                self.clamp_modal_selection();
            }
            Some(Modal::Help) => {}
        }
    }

    fn clamp_modal_selection(&mut self) {
        let modal = self.modal.take();
        self.modal = match modal {
            Some(Modal::Agent {
                purpose,
                query,
                mut selected,
            }) => {
                selected = selected.min(
                    self.agent_choices(purpose == AgentPurpose::Filter, &query)
                        .len()
                        .saturating_sub(1),
                );
                Some(Modal::Agent {
                    purpose,
                    query,
                    selected,
                })
            }
            Some(Modal::Endpoint {
                staged_agent,
                query,
                mut selected,
                back_to_agent,
            }) => {
                selected = selected.min(
                    self.endpoint_choices(staged_agent, &query)
                        .len()
                        .saturating_sub(1),
                );
                Some(Modal::Endpoint {
                    staged_agent,
                    query,
                    selected,
                    back_to_agent,
                })
            }
            other => other,
        };
    }

    fn handle_modal_key(&mut self, key: KeyEvent, key_name: &str) -> Outcome {
        let bindings = &self.config.keybindings.tui;
        if matches!(self.modal, Some(Modal::Help)) {
            if key_name == bindings.cancel || key_name == bindings.help {
                self.modal = None;
            }
            return Outcome::Continue;
        }
        if key_name == bindings.cancel {
            self.cancel_modal();
            return Outcome::Continue;
        }
        if key_name == bindings.previous || key_name == bindings.next {
            let forward = key_name == bindings.next;
            self.move_modal_selection(forward);
            return Outcome::Continue;
        }
        if key_name == bindings.accept {
            if self.tiny_modal_blocked {
                return Outcome::Continue;
            }
            self.accept_modal();
            return Outcome::Continue;
        }
        self.edit_modal_query(key, key_name);
        Outcome::Continue
    }

    fn cancel_modal(&mut self) {
        if let Some(Modal::Endpoint {
            staged_agent: Some(agent),
            back_to_agent: true,
            ..
        }) = self.modal.take()
        {
            self.modal = Some(Modal::Agent {
                purpose: AgentPurpose::Endpoint,
                query: String::new(),
                selected: self.agent_choice_position(false, Some(agent)),
            });
        }
    }

    fn edit_modal_query(&mut self, key: KeyEvent, key_name: &str) {
        let modal = self.modal.take();
        self.modal = match modal {
            Some(Modal::Agent {
                purpose,
                mut query,
                mut selected,
            }) => {
                edit_query(&mut query, key, key_name);
                selected = selected.min(
                    self.agent_choices(purpose == AgentPurpose::Filter, &query)
                        .len()
                        .saturating_sub(1),
                );
                Some(Modal::Agent {
                    purpose,
                    query,
                    selected,
                })
            }
            Some(Modal::Endpoint {
                staged_agent,
                mut query,
                mut selected,
                back_to_agent,
            }) => {
                edit_query(&mut query, key, key_name);
                selected = selected.min(
                    self.endpoint_choices(staged_agent, &query)
                        .len()
                        .saturating_sub(1),
                );
                Some(Modal::Endpoint {
                    staged_agent,
                    query,
                    selected,
                    back_to_agent,
                })
            }
            other => other,
        };
    }

    fn move_modal_selection(&mut self, forward: bool) {
        let modal = self.modal.take();
        self.modal = match modal {
            Some(Modal::Agent {
                purpose,
                query,
                mut selected,
            }) => {
                let len = self
                    .agent_choices(purpose == AgentPurpose::Filter, &query)
                    .len();
                selected = move_index(selected, len, forward);
                Some(Modal::Agent {
                    purpose,
                    query,
                    selected,
                })
            }
            Some(Modal::Endpoint {
                staged_agent,
                query,
                mut selected,
                back_to_agent,
            }) => {
                let len = self.endpoint_choices(staged_agent, &query).len();
                selected = move_index(selected, len, forward);
                Some(Modal::Endpoint {
                    staged_agent,
                    query,
                    selected,
                    back_to_agent,
                })
            }
            other => other,
        };
    }

    fn accept_modal(&mut self) {
        let Some(modal) = self.modal.take() else {
            return;
        };
        match modal {
            Modal::Agent {
                purpose,
                query,
                selected,
            } => {
                let choices = self.agent_choices(purpose == AgentPurpose::Filter, &query);
                let Some((choice, _)) = choices.get(selected).copied() else {
                    self.modal = Some(Modal::Agent {
                        purpose,
                        query,
                        selected,
                    });
                    return;
                };
                if purpose == AgentPurpose::Endpoint {
                    self.open_endpoint(choice, true);
                } else {
                    self.apply_agent_filter(choice);
                }
            }
            Modal::Endpoint {
                staged_agent,
                query,
                selected,
                back_to_agent,
            } => {
                let choices = self.endpoint_choices(staged_agent, &query);
                let Some((endpoint, _)) = choices.get(selected) else {
                    self.modal = Some(Modal::Endpoint {
                        staged_agent,
                        query,
                        selected,
                        back_to_agent,
                    });
                    return;
                };
                if back_to_agent {
                    self.agent = staged_agent;
                }
                self.endpoint = endpoint.clone();
                self.sync_cursor();
            }
            Modal::Help => self.modal = Some(Modal::Help),
        }
    }

    fn apply_agent_filter(&mut self, agent: Option<Agent>) {
        self.agent = agent;
        if let Some(endpoint_name) = self.endpoint.as_deref()
            && !self.endpoint_compatible_with_filter(endpoint_name, agent)
        {
            self.endpoint = None;
        }
        self.sync_cursor();
    }

    fn endpoint_compatible_with_filter(&self, endpoint_name: &str, agent: Option<Agent>) -> bool {
        let Some(endpoint) = self.config.endpoints.get(endpoint_name) else {
            return false;
        };
        match agent {
            Some(agent) => endpoint_is_compatible(agent, endpoint),
            None => self
                .enabled_agents()
                .into_iter()
                .all(|agent| endpoint_is_compatible(agent, endpoint)),
        }
    }

    fn open_endpoint(&mut self, agent: Option<Agent>, back_to_agent: bool) {
        self.modal = Some(Modal::Endpoint {
            staged_agent: agent,
            query: String::new(),
            selected: self.endpoint_choice_position(agent),
            back_to_agent,
        });
    }

    fn enabled_agents(&self) -> Vec<Agent> {
        [Agent::Claude, Agent::Codex]
            .into_iter()
            .filter(|agent| {
                self.items
                    .iter()
                    .any(|item| item_matches_agent(item, Some(*agent)))
            })
            .collect()
    }

    fn agent_choices(&self, include_all: bool, query: &str) -> Vec<(Option<Agent>, &'static str)> {
        let mut choices = Vec::new();
        if include_all && fuzzy_matches("all agents", query) {
            choices.push((None, "All agents"));
        }
        for agent in self.enabled_agents() {
            let label = agent_label(agent);
            if fuzzy_matches(label, query) {
                choices.push((Some(agent), label));
            }
        }
        choices
    }

    fn agent_choice_position(&self, include_all: bool, agent: Option<Agent>) -> usize {
        self.agent_choices(include_all, "")
            .iter()
            .position(|(choice, _)| *choice == agent)
            .unwrap_or(0)
    }

    fn endpoint_choices(&self, agent: Option<Agent>, query: &str) -> Vec<(Option<String>, String)> {
        let mut choices = Vec::new();
        if fuzzy_matches(FOLLOW_PROFILE_ENDPOINT_LABEL, query) {
            choices.push((None, FOLLOW_PROFILE_ENDPOINT_LABEL.to_owned()));
        }
        choices.extend(
            compatible_endpoints(self.config, agent)
                .into_iter()
                .filter(|(id, label)| fuzzy_matches(&format!("{id} {label}"), query))
                .map(|(id, label)| (Some(id), label)),
        );
        choices
    }

    fn endpoint_choice_position(&self, agent: Option<Agent>) -> usize {
        self.endpoint_choices(agent, "")
            .iter()
            .position(|(endpoint, _)| endpoint.as_deref() == self.endpoint.as_deref())
            .unwrap_or(0)
    }
}

fn item_matches_agent(item: &Item, agent: Option<Agent>) -> bool {
    agent.is_none_or(|agent| item.agent == agent.executable())
}

fn agent_label(agent: Agent) -> &'static str {
    match agent {
        Agent::Claude => "Claude",
        Agent::Codex => "Codex",
    }
}

fn move_index(index: usize, len: usize, forward: bool) -> usize {
    if len == 0 {
        0
    } else if forward {
        (index + 1).min(len - 1)
    } else {
        index.saturating_sub(1)
    }
}

fn edit_query(query: &mut String, key: KeyEvent, key_name: &str) {
    if key_name == "backspace" {
        query.pop();
        return;
    }
    match key.code {
        KeyCode::Char(character) if key.modifiers.difference(KeyModifiers::SHIFT).is_empty() => {
            push_query_character(query, character);
        }
        _ => {}
    }
}

fn fuzzy_matches(haystack: &str, query: &str) -> bool {
    let haystack: Vec<char> = haystack.to_lowercase().chars().collect();
    query.split_whitespace().all(|term| {
        let mut position = 0;
        for needle in term.to_lowercase().chars() {
            let Some(offset) = haystack[position..]
                .iter()
                .position(|candidate| *candidate == needle)
            else {
                return false;
            };
            position += offset + 1;
        }
        true
    })
}

fn push_query_character(query: &mut String, character: char) {
    if query.chars().count() < MAX_QUERY_CHARS {
        query.push(character);
    }
}

fn push_query_text(query: &mut String, value: &str) {
    let remaining = MAX_QUERY_CHARS.saturating_sub(query.chars().count());
    query.extend(value.chars().take(remaining));
}

fn key_name(key: KeyEvent) -> Option<String> {
    let modifiers = key.modifiers.difference(KeyModifiers::SHIFT);
    let named = match key.code {
        KeyCode::Enter => Some("enter"),
        KeyCode::Esc => Some("esc"),
        KeyCode::Up => Some("up"),
        KeyCode::Down => Some("down"),
        KeyCode::Left => Some("left"),
        KeyCode::Right => Some("right"),
        KeyCode::Tab => Some("tab"),
        KeyCode::BackTab => Some("btab"),
        KeyCode::Backspace => Some("backspace"),
        KeyCode::Delete => Some("delete"),
        KeyCode::Home => Some("home"),
        KeyCode::End => Some("end"),
        KeyCode::PageUp => Some("pgup"),
        KeyCode::PageDown => Some("pgdn"),
        KeyCode::F(number @ 1..=12) => return Some(format!("f{number}")),
        _ => None,
    };
    if let Some(named) = named
        && modifiers.is_empty()
    {
        return Some(named.to_owned());
    }
    if let KeyCode::Char(character) = key.code {
        let character = character.to_ascii_lowercase();
        if modifiers == KeyModifiers::CONTROL {
            return match character {
                'm' => Some("enter".to_owned()),
                'i' => Some("tab".to_owned()),
                'h' => Some("backspace".to_owned()),
                '[' => Some("esc".to_owned()),
                '_' => Some("ctrl-/".to_owned()),
                character if character.is_ascii_lowercase() || character == '/' => {
                    Some(format!("ctrl-{character}"))
                }
                _ => None,
            };
        }
        if modifiers == KeyModifiers::ALT && (character.is_ascii_lowercase() || character == '/') {
            return Some(format!("alt-{character}"));
        }
    }
    None
}

fn render(frame: &mut Frame<'_>, app: &mut App<'_>) {
    let area = frame.area();
    app.width = area.width;
    app.tiny_modal_blocked = false;
    frame.render_widget(Clear, area);
    if area.width < 24 || area.height < 7 {
        render_tiny(frame, app, area);
        return;
    }
    let vertical = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Min(3),
        Constraint::Length(2),
    ])
    .split(area);
    render_title(frame, app, vertical[0]);
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled("Search: ", Style::default().fg(Color::DarkGray)),
            Span::raw(if app.query.is_empty() {
                "Type to filter"
            } else {
                &app.query
            }),
        ])),
        vertical[1],
    );
    let mode = effective_preview(app.preview, area.width);
    match mode {
        PreviewMode::Right => {
            let horizontal =
                Layout::horizontal([Constraint::Percentage(56), Constraint::Percentage(44)])
                    .split(vertical[2]);
            render_profiles(frame, app, horizontal[0]);
            render_preview(frame, app, horizontal[1]);
        }
        PreviewMode::Below => {
            let maximum_list_height = vertical[2].height.saturating_mul(55) / 100;
            let matching_rows = u16::try_from(app.filtered_indices().len()).unwrap_or(u16::MAX);
            let list_height = matching_rows
                .saturating_add(2)
                .max(3)
                .min(maximum_list_height.max(3));
            let body = Layout::vertical([Constraint::Length(list_height), Constraint::Min(1)])
                .split(vertical[2]);
            render_profiles(frame, app, body[0]);
            render_preview(frame, app, body[1]);
        }
        _ => render_profiles(frame, app, vertical[2]),
    }
    render_footer(frame, app, vertical[3]);
    render_modal(frame, app, area);
}

fn effective_preview(mode: PreviewMode, width: u16) -> PreviewMode {
    match mode {
        PreviewMode::Auto if width >= 110 => PreviewMode::Right,
        PreviewMode::Auto if width >= 80 => PreviewMode::Below,
        PreviewMode::Auto => PreviewMode::Hidden,
        other => other,
    }
}

fn render_tiny(frame: &mut Frame<'_>, app: &mut App<'_>, area: Rect) {
    if let Some(modal) = &app.modal {
        app.tiny_modal_blocked = true;
        let title = match modal {
            Modal::Agent { .. } => "Choose agent",
            Modal::Endpoint { .. } => "Choose endpoint",
            Modal::Help => "Help",
        };
        frame.render_widget(
            Paragraph::new(Text::from(vec![
                Line::from(format!("Nomad · {title}")),
                Line::from("Resize to continue safely"),
                Line::from(format!(
                    "{} back",
                    display_key(&app.config.keybindings.tui.cancel)
                )),
            ]))
            .wrap(Wrap { trim: true }),
            area,
        );
        return;
    }
    let selected = app
        .filtered_indices()
        .get(app.cursor)
        .map(|index| app.items[*index].label.as_str())
        .unwrap_or("No profiles match");
    frame.render_widget(
        Paragraph::new(Text::from(vec![
            Line::from("Nomad · Profiles"),
            Line::from(format!("Search: {}", app.query)),
            Line::from(selected),
            Line::from(format!(
                "{} launch · {} back",
                display_key(&app.config.keybindings.tui.accept),
                display_key(&app.config.keybindings.tui.cancel)
            )),
        ]))
        .wrap(Wrap { trim: true }),
        area,
    );
}

fn render_title(frame: &mut Frame<'_>, app: &App<'_>, area: Rect) {
    let agent = app
        .agent
        .map(|agent| {
            if app.agent_locked {
                format!("{} (fixed)", agent.executable())
            } else {
                agent.executable().to_owned()
            }
        })
        .unwrap_or_else(|| "all".to_owned());
    let endpoint = app
        .endpoint
        .as_deref()
        .map(|name| format!("{} (temporary)", super::display_field(name)))
        .unwrap_or_else(|| "profile default".to_owned());
    let context = format!("Agent: {agent} · Endpoint: {endpoint}");
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(
                "Nomad · Profiles",
                Style::default()
                    .add_modifier(Modifier::BOLD)
                    .fg(Color::Cyan),
            ),
            Span::raw("  "),
            Span::styled(context, Style::default().fg(Color::DarkGray)),
        ])),
        area,
    );
}

fn render_profiles(frame: &mut Frame<'_>, app: &mut App<'_>, area: Rect) {
    let filtered = app.filtered_indices();
    let inner_height = usize::from(area.height.saturating_sub(2)).max(1);
    if app.cursor < app.scroll {
        app.scroll = app.cursor;
    } else if app.cursor >= app.scroll + inner_height {
        app.scroll = app.cursor + 1 - inner_height;
    }
    app.scroll = app.scroll.min(filtered.len().saturating_sub(inner_height));
    let available = usize::from(area.width.saturating_sub(4));
    let show_agent = app.agent.is_none() && available >= 48;
    let agent_width = if show_agent { 8 } else { 0 };
    let effort_width = if available >= 42 { 11 } else { 0 };
    let model_width = if available >= 28 { 15 } else { 0 };
    let separators = usize::from(model_width > 0) * 3
        + usize::from(effort_width > 0) * 3
        + usize::from(agent_width > 0) * 3;
    let label_width = available
        .saturating_sub(model_width + effort_width + agent_width + separators)
        .max(1);
    let rows: Vec<ListItem<'_>> = filtered
        .iter()
        .skip(app.scroll)
        .take(inner_height)
        .map(|index| {
            let item = &app.items[*index];
            let mut row = super::pad(&super::truncate(&item.label, label_width), label_width);
            if model_width > 0 {
                row.push_str(" · ");
                row.push_str(&super::pad(
                    &super::truncate(&item.model, model_width),
                    model_width,
                ));
            }
            if effort_width > 0 {
                row.push_str(" · ");
                row.push_str(&super::pad(
                    &super::truncate(&item.reasoning, effort_width),
                    effort_width,
                ));
            }
            if show_agent {
                row.push_str(" · ");
                row.push_str(&super::truncate(&item.agent, agent_width));
            }
            ListItem::new(Line::from(row))
        })
        .collect();
    let title = format!(" Profiles · {}/{} ", filtered.len(), app.items.len());
    let block = Block::default().borders(Borders::ALL).title(title);
    if rows.is_empty() {
        frame.render_widget(
            Paragraph::new("No profiles match")
                .alignment(Alignment::Center)
                .block(block),
            area,
        );
        return;
    }
    let mut state = ListState::default().with_selected(Some(app.cursor.saturating_sub(app.scroll)));
    frame.render_stateful_widget(
        List::new(rows)
            .block(block)
            .highlight_symbol("› ")
            .highlight_style(
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ),
        area,
        &mut state,
    );
}

fn render_preview(frame: &mut Frame<'_>, app: &mut App<'_>, area: Rect) {
    let preview = app
        .filtered_indices()
        .get(app.cursor)
        .map(|index| {
            let item = &app.items[*index];
            if let Some(endpoint) = app.endpoint.as_deref() {
                item.preview.replacen(
                    &format!("Endpoint: {}", item.endpoint),
                    &format!(
                        "Endpoint: {} (temporary)\nProfile default: {}",
                        super::display_field(endpoint),
                        item.endpoint
                    ),
                    1,
                )
            } else {
                item.preview.clone()
            }
        })
        .unwrap_or_else(|| "Select a matching profile to see details.".to_owned());
    let content_width = usize::from(area.width.saturating_sub(2)).max(1);
    let content_height = usize::from(area.height.saturating_sub(2));
    let wrapped_height = preview
        .lines()
        .map(|line| super::display_width(line).max(1).div_ceil(content_width))
        .sum::<usize>();
    let max_scroll = wrapped_height.saturating_sub(content_height);
    app.preview_scroll = app
        .preview_scroll
        .min(u16::try_from(max_scroll).unwrap_or(u16::MAX));
    frame.render_widget(
        Paragraph::new(preview)
            .block(Block::default().borders(Borders::ALL).title(" Details "))
            .wrap(Wrap { trim: false })
            .scroll((app.preview_scroll, 0)),
        area,
    );
}

fn render_footer(frame: &mut Frame<'_>, app: &App<'_>, area: Rect) {
    let keys = &app.config.keybindings.tui;
    if let Some(modal) = &app.modal {
        let action = match modal {
            Modal::Help => "close",
            Modal::Agent {
                purpose: AgentPurpose::Filter,
                ..
            } => "apply filter",
            Modal::Agent {
                purpose: AgentPurpose::Endpoint,
                ..
            } => "continue",
            Modal::Endpoint { .. } => "apply temporarily",
        };
        frame.render_widget(
            Paragraph::new(format!(
                "{}/{} move · {} {action} · {} back",
                display_key(&keys.previous),
                display_key(&keys.next),
                display_key(&keys.accept),
                display_key(&keys.cancel)
            ))
            .style(Style::default().fg(Color::DarkGray)),
            area,
        );
        return;
    }
    let agent = if app.agent_locked {
        String::new()
    } else {
        format!(" · {} agent", display_key(&keys.choose_agent))
    };
    let lines = vec![
        Line::from(format!(
            "{} launch{agent} · {} endpoint · {} help",
            display_key(&keys.accept),
            display_key(&keys.choose_endpoint),
            display_key(&keys.help)
        )),
        Line::from(format!(
            "{}/{} move · {} preview · {} clear/cancel",
            display_key(&keys.previous),
            display_key(&keys.next),
            display_key(&keys.toggle_preview),
            display_key(&keys.cancel)
        )),
    ];
    frame.render_widget(
        Paragraph::new(lines).style(Style::default().fg(Color::DarkGray)),
        area,
    );
}

fn render_modal(frame: &mut Frame<'_>, app: &App<'_>, area: Rect) {
    let Some(modal) = &app.modal else { return };
    match modal {
        Modal::Help => {
            let modal_area = centered_rect(area, area.width.min(64), area.height.min(12));
            frame.render_widget(Clear, modal_area);
            render_help(frame, app, modal_area);
        }
        Modal::Agent {
            purpose,
            query,
            selected,
        } => {
            let choices = app.agent_choices(*purpose == AgentPurpose::Filter, query);
            let modal_area = choice_modal_area(area, choices.len());
            frame.render_widget(Clear, modal_area);
            let subtitle = if *purpose == AgentPurpose::Filter {
                "Filter profiles"
            } else {
                "Select agent for temporary endpoint"
            };
            render_choice_modal(
                frame,
                modal_area,
                ChoiceModal {
                    title: "Choose agent",
                    subtitle,
                    query,
                    selected: *selected,
                    labels: choices.into_iter().map(|(_, label)| label).collect(),
                },
                &app.config.keybindings.tui,
            );
        }
        Modal::Endpoint {
            staged_agent,
            query,
            selected,
            ..
        } => {
            let choices = app.endpoint_choices(*staged_agent, query);
            let modal_area = choice_modal_area(area, choices.len());
            frame.render_widget(Clear, modal_area);
            render_choice_modal(
                frame,
                modal_area,
                ChoiceModal {
                    title: "Choose endpoint",
                    subtitle: "One launch only · Follow profile default resets override",
                    query,
                    selected: *selected,
                    labels: choices.iter().map(|(_, label)| label.as_str()).collect(),
                },
                &app.config.keybindings.tui,
            );
        }
    }
}

fn choice_modal_area(area: Rect, choice_count: usize) -> Rect {
    let visible_choices = u16::try_from(choice_count.clamp(1, 9)).unwrap_or(9);
    centered_rect(
        area,
        area.width.min(64),
        area.height.min(visible_choices.saturating_add(5)),
    )
}

struct ChoiceModal<'a> {
    title: &'a str,
    subtitle: &'a str,
    query: &'a str,
    selected: usize,
    labels: Vec<&'a str>,
}

fn render_choice_modal(
    frame: &mut Frame<'_>,
    area: Rect,
    modal: ChoiceModal<'_>,
    keys: &TuiKeybindings,
) {
    let ChoiceModal {
        title,
        subtitle,
        query,
        selected,
        labels,
    } = modal;
    let chunks = Layout::vertical([
        Constraint::Length(2),
        Constraint::Min(1),
        Constraint::Length(1),
    ])
    .margin(1)
    .split(area);
    frame.render_widget(
        Block::default()
            .borders(Borders::ALL)
            .title(format!(" {title} ")),
        area,
    );
    frame.render_widget(
        Paragraph::new(vec![
            Line::from(subtitle),
            Line::from(format!("Search: {query}")),
        ]),
        chunks[0],
    );
    let visible = usize::from(chunks[1].height).max(1);
    let start = if selected >= visible {
        selected + 1 - visible
    } else {
        0
    };
    let rows: Vec<ListItem<'_>> = labels
        .into_iter()
        .skip(start)
        .take(visible)
        .map(ListItem::new)
        .collect();
    if rows.is_empty() {
        frame.render_widget(Paragraph::new("No choices match"), chunks[1]);
    } else {
        let mut state = ListState::default().with_selected(Some(selected.saturating_sub(start)));
        frame.render_stateful_widget(
            List::new(rows).highlight_symbol("› ").highlight_style(
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ),
            chunks[1],
            &mut state,
        );
    }
    frame.render_widget(
        Paragraph::new(format!(
            "{} apply · {} back",
            display_key(&keys.accept),
            display_key(&keys.cancel)
        ))
        .style(Style::default().fg(Color::DarkGray)),
        chunks[2],
    );
}

fn render_help(frame: &mut Frame<'_>, app: &App<'_>, area: Rect) {
    let keys = &app.config.keybindings.tui;
    let mut body = vec![
        Line::from(format!(
            "{} / {}   Move selection",
            display_key(&keys.previous),
            display_key(&keys.next)
        )),
        Line::from(format!(
            "{}       Launch or apply",
            display_key(&keys.accept)
        )),
        if app.agent_locked {
            Line::from("Agent       Fixed by --agent")
        } else {
            Line::from(format!(
                "{}      Filter by agent",
                display_key(&keys.choose_agent)
            ))
        },
        Line::from(format!(
            "{}      Choose temporary endpoint",
            display_key(&keys.choose_endpoint)
        )),
        Line::from(format!(
            "{}      Toggle preview",
            display_key(&keys.toggle_preview)
        )),
        Line::from(format!(
            "{} / {} Preview below / right",
            display_key(&keys.preview_below),
            display_key(&keys.preview_right)
        )),
        Line::from(format!(
            "{}         Clear search, go back, or cancel",
            display_key(&keys.cancel)
        )),
        Line::from("Ctrl-C      Cancel from anywhere"),
    ];
    let pgup_available = !action_uses_key(keys, "pgup");
    let pgdn_available = !action_uses_key(keys, "pgdn");
    if pgup_available || pgdn_available {
        let keys = match (pgup_available, pgdn_available) {
            (true, true) => "PgUp/PgDn",
            (true, false) => "PgUp",
            (false, true) => "PgDn",
            (false, false) => unreachable!(),
        };
        body.push(Line::from(format!("{keys} Scroll profile details")));
    }
    frame.render_widget(Clear, area);
    frame.render_widget(
        Paragraph::new(body)
            .block(Block::default().borders(Borders::ALL).title(" Help "))
            .wrap(Wrap { trim: false }),
        area,
    );
}

fn action_uses_key(keys: &TuiKeybindings, key: &str) -> bool {
    [
        &keys.accept,
        &keys.cancel,
        &keys.previous,
        &keys.next,
        &keys.toggle_preview,
        &keys.preview_below,
        &keys.preview_right,
        &keys.choose_agent,
        &keys.choose_endpoint,
        &keys.help,
    ]
    .into_iter()
    .any(|binding| binding == key)
}

fn centered_rect(area: Rect, width: u16, height: u16) -> Rect {
    let width = width.min(area.width).max(1);
    let height = height.min(area.height).max(1);
    Rect::new(
        area.x + area.width.saturating_sub(width) / 2,
        area.y + area.height.saturating_sub(height) / 2,
        width,
        height,
    )
}

fn display_key(key: &str) -> String {
    key.split('-')
        .map(|part| match part {
            "ctrl" => "Ctrl".to_owned(),
            "alt" => "Alt".to_owned(),
            "enter" => "Enter".to_owned(),
            "esc" => "Esc".to_owned(),
            "up" => "↑".to_owned(),
            "down" => "↓".to_owned(),
            other if other.starts_with('f') => other.to_ascii_uppercase(),
            other if other.len() == 1 && other.as_bytes()[0].is_ascii_alphabetic() => {
                other.to_ascii_uppercase()
            }
            other => other.to_owned(),
        })
        .collect::<Vec<_>>()
        .join("-")
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::{Terminal, backend::TestBackend};

    fn fixture() -> Config {
        toml::from_str("version=1\n[endpoints.official]\nauth='native'\n[endpoints.claude-api]\nauth='api-key'\nprotocol='anthropic-messages'\nbase_url='https://example.invalid/v1'\nkey_env='FIXTURE_KEY'\n[profiles.knight]\nagent='claude'\nendpoint='official'\nlabel='Knight Review'\ndescription='fork tactics'\ntags=['chess']\nmodel='sonnet'\nreasoning='high'\n[profiles.rook]\nagent='codex'\nendpoint='official'\nlabel='Rook Builder'\ndescription='hidden-file-search'\ntags=['castle']\nmodel='codex-5'\nreasoning='medium'\n").unwrap()
    }

    fn app(config: &Config) -> App<'_> {
        App::new(config, items(config, None), Some("rook"), None, None).unwrap()
    }

    fn press(code: KeyCode, modifiers: KeyModifiers) -> KeyEvent {
        KeyEvent::new(code, modifiers)
    }

    fn buffer(
        config: &Config,
        width: u16,
        height: u16,
        mutate: impl FnOnce(&mut App<'_>),
    ) -> String {
        let mut app = app(config);
        mutate(&mut app);
        draw_buffer(&mut app, width, height)
    }

    fn draw_buffer(app: &mut App<'_>, width: u16, height: u16) -> String {
        let backend = TestBackend::new(width, height);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|frame| render(frame, app)).unwrap();
        let buffer = terminal.backend().buffer();
        (0..height)
            .map(|y| {
                (0..width)
                    .map(|x| buffer[(x, y)].symbol())
                    .collect::<String>()
                    .trim_end()
                    .to_owned()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn fuzzy_search_matches_hidden_metadata_and_multiple_unicode_terms() {
        let config = fixture();
        let mut app = app(&config);
        app.query = "hfs cdx".to_owned();
        app.sync_cursor();
        assert_eq!(app.filtered_indices().len(), 1);
        assert_eq!(app.selected_id.as_deref(), Some("rook"));
        assert!(fuzzy_matches("棋子 Profile", "棋 pfi"));
    }

    #[test]
    fn root_escape_clears_query_before_cancelling() {
        let config = fixture();
        let mut app = app(&config);
        for character in "z9q".chars() {
            app.handle_key(press(KeyCode::Char(character), KeyModifiers::NONE));
        }
        assert!(app.filtered_indices().is_empty());
        assert!(matches!(
            app.handle_key(press(KeyCode::Esc, KeyModifiers::NONE)),
            Outcome::Continue
        ));
        assert!(app.query.is_empty());
        assert!(matches!(
            app.handle_key(press(KeyCode::Esc, KeyModifiers::NONE)),
            Outcome::Finish(None)
        ));
    }

    #[test]
    fn endpoint_staging_is_transactional_and_escape_restores_root_state() {
        let config = fixture();
        let mut app = app(&config);
        app.endpoint = Some("official".to_owned());
        app.handle_key(press(KeyCode::Char('e'), KeyModifiers::CONTROL));
        assert!(matches!(
            app.modal,
            Some(Modal::Agent {
                purpose: AgentPurpose::Endpoint,
                ..
            })
        ));
        app.accept_modal();
        assert!(matches!(
            app.modal,
            Some(Modal::Endpoint {
                back_to_agent: true,
                ..
            })
        ));
        assert_eq!(app.agent, None);
        assert_eq!(app.endpoint.as_deref(), Some("official"));
        app.cancel_modal();
        app.cancel_modal();
        assert_eq!(app.modal, None);
        assert_eq!(app.agent, None);
        assert_eq!(app.endpoint.as_deref(), Some("official"));
        assert_eq!(app.selected_id.as_deref(), Some("rook"));
    }

    #[test]
    fn direct_agent_filter_clears_only_incompatible_override() {
        let config = fixture();
        let mut app = app(&config);
        app.endpoint = Some("claude-api".to_owned());
        app.apply_agent_filter(Some(Agent::Codex));
        assert_eq!(app.endpoint, None);
        app.endpoint = Some("official".to_owned());
        app.apply_agent_filter(None);
        assert_eq!(app.endpoint.as_deref(), Some("official"));
    }

    #[test]
    fn cli_agent_is_fixed_and_rejects_missing_profiles() {
        let config = fixture();
        let mut app = App::new(
            &config,
            items(&config, None),
            None,
            Some(Agent::Claude),
            None,
        )
        .unwrap();
        app.handle_key(press(KeyCode::Char('l'), KeyModifiers::CONTROL));
        assert!(app.modal.is_none());
        let empty: Config = toml::from_str("version=1\n[endpoints.official]\nauth='native'\n[profiles.only]\nagent='codex'\nendpoint='official'\n").unwrap();
        assert!(App::new(&empty, items(&empty, None), None, Some(Agent::Claude), None).is_err());
    }

    #[test]
    fn api_endpoint_infers_an_unlocked_agent_and_rejects_incompatible_cli_filter() {
        let config = fixture();
        let app = App::new(
            &config,
            items(&config, None),
            None,
            None,
            Some("claude-api"),
        )
        .unwrap();
        assert_eq!(app.agent, Some(Agent::Claude));
        assert!(!app.agent_locked);
        assert_eq!(app.endpoint.as_deref(), Some("claude-api"));
        assert!(
            App::new(
                &config,
                items(&config, None),
                None,
                Some(Agent::Codex),
                Some("claude-api")
            )
            .is_err()
        );
        let no_claude: Config = toml::from_str(
            "version=1\n[endpoints.api]\nauth='api-key'\nprotocol='anthropic-messages'\nbase_url='https://example.invalid/v1'\nkey_env='FIXTURE_KEY'\n[profiles.only]\nagent='codex'\nendpoint='api'\nmodel='fixture'\n",
        )
        .unwrap();
        assert!(App::new(&no_claude, items(&no_claude, None), None, None, Some("api")).is_err());
    }

    #[test]
    fn custom_keys_and_control_aliases_map_to_real_events() {
        assert_eq!(
            key_name(press(KeyCode::F(4), KeyModifiers::NONE)).as_deref(),
            Some("f4")
        );
        assert_eq!(
            key_name(press(KeyCode::Char('_'), KeyModifiers::CONTROL)).as_deref(),
            Some("ctrl-/")
        );
        assert_eq!(
            key_name(press(KeyCode::Char('/'), KeyModifiers::ALT)).as_deref(),
            Some("alt-/")
        );
        assert_eq!(
            key_name(press(KeyCode::Enter, KeyModifiers::NONE)).as_deref(),
            Some("enter")
        );
        assert_eq!(
            key_name(press(KeyCode::Char('h'), KeyModifiers::CONTROL)).as_deref(),
            Some("backspace")
        );
        assert_eq!(
            key_name(press(KeyCode::Char('m'), KeyModifiers::CONTROL)).as_deref(),
            Some("enter")
        );
        assert_eq!(
            key_name(press(KeyCode::Char('i'), KeyModifiers::CONTROL)).as_deref(),
            Some("tab")
        );
        assert_eq!(
            key_name(press(KeyCode::Char('['), KeyModifiers::CONTROL)).as_deref(),
            Some("esc")
        );
    }

    #[test]
    fn bracketed_paste_is_sanitized_capped_and_never_accepted_as_input() {
        let config = fixture();
        let mut app = app(&config);
        app.handle_paste(&format!("rook\r\n\u{1b}[31m{}", "x".repeat(400)));
        assert_eq!(app.query.chars().count(), MAX_QUERY_CHARS);
        assert!(!app.query.chars().any(char::is_control));
        assert!(app.modal.is_none());
        let ctrl_h = press(KeyCode::Char('h'), KeyModifiers::CONTROL);
        app.handle_key(ctrl_h);
        assert_eq!(app.query.chars().count(), MAX_QUERY_CHARS - 1);
    }

    #[test]
    fn preview_scroll_clamps_and_never_steals_a_configured_action() {
        let mut config = fixture();
        config.profiles.get_mut("rook").unwrap().description = Some(
            (0..80)
                .map(|index| format!("detail-{index} "))
                .collect::<Vec<_>>()
                .join(""),
        );
        config.keybindings.tui.choose_agent = "pgdn".to_owned();
        let mut app = App::new(
            &config,
            items(&config, None),
            Some("rook"),
            Some(Agent::Codex),
            None,
        )
        .unwrap();
        app.preview = PreviewMode::Below;
        app.preview_scroll = u16::MAX;
        let screen = draw_buffer(&mut app, 80, 24);
        assert!(app.preview_scroll < u16::MAX);
        assert!(!screen.trim().is_empty());
        let before = app.preview_scroll;
        app.handle_key(press(KeyCode::PageDown, KeyModifiers::NONE));
        assert_eq!(app.preview_scroll, before);
        app.query = "no-match".to_owned();
        app.sync_cursor();
        assert_eq!(app.preview_scroll, 0);
    }

    #[test]
    fn layouts_render_context_rows_details_and_retained_modals() {
        let config = fixture();
        let eighty = buffer(&config, 80, 24, |_| {});
        assert!(eighty.contains("Nomad · Profiles"));
        assert!(eighty.contains("Rook Builder"));
        assert!(eighty.contains("Details"));
        let wide = buffer(&config, 120, 30, |_| {});
        assert!(wide.contains("Agent: all · Endpoint: profile default"));
        assert!(wide.contains("hidden-file-search"));
        let agent = buffer(&config, 160, 40, |app| {
            app.modal = Some(Modal::Agent {
                purpose: AgentPurpose::Filter,
                query: String::new(),
                selected: 0,
            })
        });
        assert!(agent.contains("Choose agent"));
        assert!(agent.contains("Nomad · Profiles"));
        let endpoint = buffer(&config, 120, 30, |app| {
            app.open_endpoint(Some(Agent::Claude), false)
        });
        assert!(endpoint.contains("Choose endpoint"));
        assert!(endpoint.contains(FOLLOW_PROFILE_ENDPOINT_LABEL));
        let tiny = buffer(&config, 10, 4, |_| {});
        assert!(tiny.contains("Nomad"));
    }

    #[test]
    fn sanitized_items_never_render_terminal_controls_or_endpoint_secrets() {
        let mut config = fixture();
        config.profiles.get_mut("rook").unwrap().description = Some("line\n\u{1b}[31m".to_owned());
        let screen = buffer(&config, 120, 30, |_| {});
        assert!(!screen.contains('\u{1b}'));
        assert!(!screen.contains("example.invalid"));
        assert!(!screen.contains("FIXTURE_KEY"));
    }

    #[test]
    fn endpoint_identifiers_are_sanitized_in_context_details_and_menu() {
        let mut config = fixture();
        let unsafe_name = "unsafe\u{1b}]8;;label\u{7}".to_owned();
        config
            .endpoints
            .insert(unsafe_name.clone(), crate::config::Endpoint::Native);
        let mut app = App::new(
            &config,
            items(&config, None),
            None,
            None,
            Some(&unsafe_name),
        )
        .unwrap();
        let screen = draw_buffer(&mut app, 120, 30);
        assert!(!screen.contains('\u{1b}'));
        assert!(!screen.contains('\u{7}'));
        app.open_endpoint(Some(Agent::Claude), false);
        let menu = draw_buffer(&mut app, 120, 30);
        assert!(!menu.contains('\u{1b}'));
        assert!(!menu.contains('\u{7}'));
        assert!(menu.contains("unsafe�]8;;label�"));
    }
}
