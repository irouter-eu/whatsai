//! `whatsai ui`: a terminal client that talks to the daemon directly. Every key does one fixed
//! thing, every screen is a fixed table, and it refreshes on its own. No model in the loop.
use anyhow::Result;
use crossterm::{
    event::{self, Event, KeyCode, KeyEvent, KeyModifiers},
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use ratatui::{
    Frame, Terminal,
    backend::Backend,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph, Wrap},
};
use serde_json::{Value, json};
use std::{
    path::PathBuf,
    time::{Duration, Instant},
};
use whatsai_core::view;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Tab {
    Members,
    Inbox,
    Requests,
    Agents,
    Outbox,
}
impl Tab {
    const ALL: [Tab; 5] = [
        Tab::Members,
        Tab::Inbox,
        Tab::Requests,
        Tab::Agents,
        Tab::Outbox,
    ];
    fn title(self) -> &'static str {
        match self {
            Tab::Members => "Members",
            Tab::Inbox => "Inbox",
            Tab::Requests => "Requests",
            Tab::Agents => "Agents",
            Tab::Outbox => "Outbox",
        }
    }
}
/// What the input line is collecting, when it is open.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Input {
    Recipient(String),
    Message { to: String, text: String },
}
pub struct App {
    pub state: PathBuf,
    pub identity: Value,
    pub version: Value,
    pub teams: Vec<Value>,
    pub selected_team: usize,
    pub tab: Tab,
    pub cursor: usize,
    /// Cached results per tab for the selected team.
    pub team: Value,
    pub inbox: Value,
    pub requests: Value,
    pub agents: Value,
    pub outbox: Value,
    pub status: String,
    pub input: Option<Input>,
    pub popup: Option<Vec<String>>,
    pub last_refresh: Option<Instant>,
    pub sync_error: String,
}
impl App {
    pub fn new(state: PathBuf) -> Self {
        Self {
            state,
            identity: Value::Null,
            version: Value::Null,
            teams: vec![],
            selected_team: 0,
            tab: Tab::Members,
            cursor: 0,
            team: Value::Null,
            inbox: json!([]),
            requests: json!([]),
            agents: json!([]),
            outbox: json!([]),
            status: "connecting to the daemon…".into(),
            input: None,
            popup: None,
            last_refresh: None,
            sync_error: String::new(),
        }
    }
    fn team_id(&self) -> Option<String> {
        self.teams.get(self.selected_team)?["id"]
            .as_str()
            .map(str::to_owned)
    }
    fn request(&self, mut command: Value) -> Result<Value> {
        if let Some(team) = self.team_id()
            && command["team"].is_null()
        {
            command["team"] = json!(team);
        }
        let state = self.state.clone();
        tokio::runtime::Handle::current()
            .block_on(async move { whatsai_core::daemon::request(&state, command).await })
    }
    /// Pull everything the screens show. Errors land in the status line, never in a crash.
    pub fn refresh(&mut self) {
        match self.request(json!({"action":"health"})) {
            Ok(h) => {
                self.identity = h["member"].clone();
                self.sync_error = h["last_sync_error"].as_str().unwrap_or("").to_owned();
                self.teams = h["teams"]
                    .as_array()
                    .map(|t| {
                        t.iter()
                            .filter(|x| x["state"] == "member")
                            .cloned()
                            .collect()
                    })
                    .unwrap_or_default();
                if self.selected_team >= self.teams.len() {
                    self.selected_team = 0;
                }
            }
            Err(e) => {
                self.status = format!("daemon: {e}");
                return;
            }
        }
        if let Ok(v) = self.request(json!({"action":"version"})) {
            self.version = v;
        }
        self.agents = self
            .request(json!({"action":"agents"}))
            .unwrap_or(json!([]));
        if self.team_id().is_some() {
            match self.request(json!({"action":"list"})) {
                Ok(t) => self.team = t,
                Err(e) => self.status = format!("list: {e}"),
            }
            self.inbox = self.request(json!({"action":"inbox"})).unwrap_or(json!([]));
            self.requests = self
                .request(json!({"action":"requests"}))
                .unwrap_or(json!([]));
            self.outbox = self
                .request(json!({"action":"outbox"}))
                .unwrap_or(json!([]));
        } else {
            self.team = Value::Null;
        }
        self.last_refresh = Some(Instant::now());
    }
    pub fn lines(&self) -> Vec<String> {
        match self.tab {
            Tab::Members => {
                if self.team.is_null() {
                    vec![
                        "No team selected. Create one with `whatsai create` or join with a key."
                            .into(),
                    ]
                } else {
                    view::members(&self.team)
                }
            }
            Tab::Inbox => view::inbox(&self.inbox, &self.team),
            Tab::Requests => view::requests(&self.requests),
            Tab::Agents => view::agents(&self.agents),
            Tab::Outbox => {
                let rows: Vec<Vec<String>> = self
                    .outbox
                    .as_array()
                    .map(|a| {
                        a.iter()
                            .map(|o| {
                                let delivered = o["receipts"]["recipients"]
                                    .as_object()
                                    .map(|r| {
                                        format!(
                                            "{}/{}",
                                            r.values().filter(|v| **v == true).count(),
                                            r.len()
                                        )
                                    })
                                    .unwrap_or_default();
                                vec![
                                    o["kind"].as_str().unwrap_or("").to_owned(),
                                    o["state"].as_str().unwrap_or("").to_owned(),
                                    delivered,
                                    o["error"].as_str().unwrap_or("").to_owned(),
                                    view::short(o["id"].as_str().unwrap_or("")),
                                ]
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                if rows.is_empty() {
                    vec!["Nothing sent yet.".into()]
                } else {
                    view::table(&["KIND", "STATE", "DELIVERED", "ERROR", "EVENT"], &rows)
                }
            }
        }
    }
    /// The item the cursor is on, for tabs where rows are things you can act on.
    fn selected_row(&self) -> Option<Value> {
        let list = match self.tab {
            Tab::Requests => self.requests.as_array()?,
            Tab::Agents => self.agents.as_array()?,
            Tab::Inbox => self.inbox.as_array()?,
            _ => return None,
        };
        list.get(self.cursor).cloned()
    }
    pub fn key(&mut self, key: KeyEvent) -> bool {
        if self.popup.is_some() {
            self.popup = None;
            return true;
        }
        if let Some(input) = self.input.take() {
            return self.input_key(input, key);
        }
        match key.code {
            KeyCode::Char('q') => return false,
            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => return false,
            KeyCode::Tab => {
                let i = Tab::ALL.iter().position(|t| *t == self.tab).unwrap_or(0);
                self.tab = Tab::ALL[(i + 1) % Tab::ALL.len()];
                self.cursor = 0;
            }
            KeyCode::BackTab => {
                let i = Tab::ALL.iter().position(|t| *t == self.tab).unwrap_or(0);
                self.tab = Tab::ALL[(i + Tab::ALL.len() - 1) % Tab::ALL.len()];
                self.cursor = 0;
            }
            KeyCode::Left | KeyCode::Char('[') if !self.teams.is_empty() => {
                self.selected_team = (self.selected_team + self.teams.len() - 1) % self.teams.len();
                self.cursor = 0;
                self.refresh();
            }
            KeyCode::Right | KeyCode::Char(']') if !self.teams.is_empty() => {
                self.selected_team = (self.selected_team + 1) % self.teams.len();
                self.cursor = 0;
                self.refresh();
            }
            KeyCode::Down | KeyCode::Char('j') => {
                let n = self.actionable_len();
                if n > 0 {
                    self.cursor = (self.cursor + 1).min(n - 1);
                }
            }
            KeyCode::Up | KeyCode::Char('k') => self.cursor = self.cursor.saturating_sub(1),
            KeyCode::Char('r') => {
                self.status = match self.request(json!({"action":"sync"})) {
                    Ok(v) => format!("synced: {}", v["state"].as_str().unwrap_or("")),
                    Err(e) => format!("sync failed: {e}"),
                };
                self.refresh();
            }
            KeyCode::Char('s') if self.team_id().is_some() => {
                self.input = Some(Input::Recipient(String::new()));
                self.status =
                    "to: a person (bob), a session (bob/claude), or empty for everyone".into();
            }
            KeyCode::Char('i') if self.team_id().is_some() => {
                match self.request(json!({"action":"invite"})) {
                    Ok(v) => {
                        self.popup = Some(vec![
                            format!(
                                "Join key for {} (relay: {})",
                                v["workspace"].as_str().unwrap_or(""),
                                v["relay"]
                            ),
                            String::new(),
                            v["join"].as_str().unwrap_or("").to_owned(),
                            String::new(),
                            v["notice"].as_str().unwrap_or("").to_owned(),
                            "Any key to close.".into(),
                        ]);
                    }
                    Err(e) => self.status = format!("invite: {e}"),
                }
            }
            KeyCode::Char('a') if self.tab == Tab::Requests => {
                if let Some(r) = self.selected_row() {
                    let id = r["member"]["id"].as_str().unwrap_or("").to_owned();
                    self.status = match self.request(json!({"action":"approve","member":id})) {
                        Ok(_) => format!("approved {}", r["member"]["name"].as_str().unwrap_or("")),
                        Err(e) => format!("approve: {e}"),
                    };
                    self.refresh();
                }
            }
            KeyCode::Char('x') if self.tab == Tab::Requests => {
                if let Some(r) = self.selected_row() {
                    let id = r["member"]["id"].as_str().unwrap_or("").to_owned();
                    self.status = match self.request(json!({"action":"reject","member":id})) {
                        Ok(_) => format!("rejected {}", r["member"]["name"].as_str().unwrap_or("")),
                        Err(e) => format!("reject: {e}"),
                    };
                    self.refresh();
                }
            }
            KeyCode::Char('p') if self.tab == Tab::Agents => {
                if let Some(a) = self.selected_row() {
                    let label = a["label"].as_str().unwrap_or("").to_owned();
                    let op = if a["published"] == true {
                        "unpublish"
                    } else {
                        "publish"
                    };
                    self.status = match self
                        .request(json!({"action":"agent","operation":op,"agent":label}))
                    {
                        Ok(v) => format!(
                            "{label}: {}",
                            if v["published"] == true {
                                "published"
                            } else {
                                "private"
                            }
                        ),
                        Err(e) => format!("{op}: {e}"),
                    };
                    self.refresh();
                }
            }
            KeyCode::Char('e') if self.tab == Tab::Agents => {
                if let Some(a) = self.selected_row() {
                    let label = a["label"].as_str().unwrap_or("").to_owned();
                    let op = if a["enrolled"] == true {
                        "unenroll"
                    } else {
                        "enroll"
                    };
                    let team = self.team_id();
                    self.status = match self
                        .request(json!({"action":"agent","operation":op,"agent":label,"team":team}))
                    {
                        Ok(v) => format!(
                            "{label}: {}",
                            if v["enrolled"] == true {
                                format!("enrolled in {}", v["team_name"].as_str().unwrap_or(""))
                            } else {
                                "not enrolled".into()
                            }
                        ),
                        Err(e) => format!("{op}: {e}"),
                    };
                    self.refresh();
                }
            }
            KeyCode::Char('m') if self.tab == Tab::Agents => {
                if let Some(a) = self.selected_row() {
                    let label = a["label"].as_str().unwrap_or("").to_owned();
                    self.status = match self
                        .request(json!({"action":"agent","operation":"mark-read","agent":label}))
                    {
                        Ok(_) => format!("{label}: marked read"),
                        Err(e) => format!("mark-read: {e}"),
                    };
                    self.refresh();
                }
            }
            KeyCode::Enter if self.tab == Tab::Inbox => {
                if let Some(m) = self.selected_row() {
                    self.popup = Some(vec![
                        format!(
                            "From {} {}",
                            view::short(m["sender"].as_str().unwrap_or("")),
                            m["event"]["agent"].as_str().unwrap_or("")
                        ),
                        String::new(),
                        m["event"]["text"].as_str().unwrap_or("").to_owned(),
                        String::new(),
                        format!(
                            "event {}  reply with s and the sender as recipient",
                            m["id"].as_str().unwrap_or("")
                        ),
                    ]);
                }
            }
            _ => {}
        }
        true
    }
    fn actionable_len(&self) -> usize {
        match self.tab {
            Tab::Requests => self.requests.as_array().map(Vec::len).unwrap_or(0),
            Tab::Agents => self.agents.as_array().map(Vec::len).unwrap_or(0),
            Tab::Inbox => self.inbox.as_array().map(Vec::len).unwrap_or(0),
            _ => 0,
        }
    }
    fn input_key(&mut self, input: Input, key: KeyEvent) -> bool {
        match (input, key.code) {
            (_, KeyCode::Esc) => self.status = "cancelled".into(),
            (Input::Recipient(mut to), KeyCode::Char(c)) => {
                to.push(c);
                self.input = Some(Input::Recipient(to));
            }
            (Input::Recipient(mut to), KeyCode::Backspace) => {
                to.pop();
                self.input = Some(Input::Recipient(to));
            }
            (Input::Recipient(to), KeyCode::Enter) => {
                self.input = Some(Input::Message {
                    to,
                    text: String::new(),
                });
                self.status = "message text, Enter to send".into();
            }
            (Input::Message { to, mut text }, KeyCode::Char(c)) => {
                text.push(c);
                self.input = Some(Input::Message { to, text });
            }
            (Input::Message { to, mut text }, KeyCode::Backspace) => {
                text.pop();
                self.input = Some(Input::Message { to, text });
            }
            (Input::Message { to, text }, KeyCode::Enter) => {
                if text.trim().is_empty() {
                    self.status = "nothing sent: empty message".into();
                } else {
                    let mut cmd = json!({"action":"send","text":text});
                    if !to.trim().is_empty() {
                        cmd["to"] = json!(to.trim());
                    }
                    self.status = match self.request(cmd) {
                        Ok(_) => {
                            let _ = self.request(json!({"action":"sync"}));
                            "sent".into()
                        }
                        Err(e) => format!("send: {e}"),
                    };
                    self.refresh();
                }
            }
            (input, _) => self.input = Some(input),
        }
        true
    }
}
pub fn draw(frame: &mut Frame, app: &App) {
    let area = frame.area();
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Min(5),
            Constraint::Length(4),
        ])
        .split(area);
    let name = app.identity["name"].as_str().unwrap_or("?");
    let fp = view::short(app.identity["id"].as_str().unwrap_or(""));
    let release = app.version["daemon"].as_str().unwrap_or("?");
    let sync = if app.sync_error.is_empty() {
        Span::styled("synced", Style::default().fg(Color::Green))
    } else {
        Span::styled(
            format!("sync: {}", app.sync_error),
            Style::default().fg(Color::Red),
        )
    };
    let header = Paragraph::new(Line::from(vec![
        Span::styled(
            format!(" whatsai {release} "),
            Style::default().add_modifier(Modifier::BOLD),
        ),
        Span::raw(format!("| {name} {fp} | ")),
        sync,
    ]))
    .block(Block::default().borders(Borders::ALL));
    frame.render_widget(header, chunks[0]);
    let body = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Length(26), Constraint::Min(20)])
        .split(chunks[1]);
    let items: Vec<ListItem> = if app.teams.is_empty() {
        vec![ListItem::new("no teams")]
    } else {
        app.teams
            .iter()
            .map(|t| {
                let role = if t["role"] == "admin" { "*" } else { " " };
                ListItem::new(format!(
                    "{role}{} ({})",
                    t["workspace"].as_str().unwrap_or(""),
                    t["members"].as_u64().unwrap_or(0)
                ))
            })
            .collect()
    };
    let mut state = ListState::default();
    if !app.teams.is_empty() {
        state.select(Some(app.selected_team));
    }
    frame.render_stateful_widget(
        List::new(items)
            .block(Block::default().borders(Borders::ALL).title(" Teams ←/→ "))
            .highlight_style(Style::default().add_modifier(Modifier::REVERSED)),
        body[0],
        &mut state,
    );
    let titles: Vec<Span> = Tab::ALL
        .iter()
        .map(|t| {
            let mut label = format!(" {} ", t.title());
            if *t == Tab::Requests
                && app
                    .requests
                    .as_array()
                    .is_some_and(|r| r.iter().any(|x| x["state"] == "pending"))
            {
                label = format!(" {}! ", t.title());
            }
            if *t == app.tab {
                Span::styled(label, Style::default().add_modifier(Modifier::REVERSED))
            } else {
                Span::raw(label)
            }
        })
        .collect();
    let lines = app.lines();
    let actionable = app.actionable_len();
    let text: Vec<Line> = lines
        .iter()
        .enumerate()
        .map(|(i, l)| {
            // Rows start after the header and rule for tabs with selectable items.
            if actionable > 0 && i >= 2 && i - 2 == app.cursor && app.tab != Tab::Members {
                Line::from(Span::styled(
                    l.clone(),
                    Style::default().add_modifier(Modifier::REVERSED),
                ))
            } else {
                Line::from(l.clone())
            }
        })
        .collect();
    frame.render_widget(
        Paragraph::new(text).block(
            Block::default()
                .borders(Borders::ALL)
                .title(Line::from(titles)),
        ),
        body[1],
    );
    let footer = match &app.input {
        Some(Input::Recipient(to)) => format!("to> {to}▏   (Enter for everyone, Esc to cancel)"),
        Some(Input::Message { to, text }) => format!(
            "to {}> {text}▏",
            if to.is_empty() { "everyone" } else { to }
        ),
        None => format!(
            "{}\nTab tabs  ←/→ teams  ↑/↓ rows  r sync  s send  i key  a/x approve/reject  p publish  e enroll  m read  q quit",
            app.status
        ),
    };
    frame.render_widget(
        Paragraph::new(footer)
            .wrap(Wrap { trim: false })
            .block(Block::default().borders(Borders::ALL)),
        chunks[2],
    );
    if let Some(popup) = &app.popup {
        let width = area.width.saturating_sub(6).min(100);
        let height = (popup.len() as u16 + 4).min(area.height.saturating_sub(2));
        let rect = Rect::new(
            area.x + (area.width - width) / 2,
            area.y + (area.height - height) / 2,
            width,
            height,
        );
        frame.render_widget(Clear, rect);
        frame.render_widget(
            Paragraph::new(
                popup
                    .iter()
                    .map(|l| Line::from(l.clone()))
                    .collect::<Vec<_>>(),
            )
            .wrap(Wrap { trim: false })
            .block(Block::default().borders(Borders::ALL).title(" whatsai ")),
            rect,
        );
    }
}
pub fn run<B: Backend>(
    terminal: &mut Terminal<B>,
    app: &mut App,
    interval: Duration,
) -> Result<()> {
    app.refresh();
    loop {
        terminal.draw(|f| draw(f, app))?;
        if event::poll(Duration::from_millis(200))?
            && let Event::Key(key) = event::read()?
            && key.kind == event::KeyEventKind::Press
            && !app.key(key)
        {
            return Ok(());
        }
        if app.last_refresh.is_none_or(|t| t.elapsed() >= interval) && app.input.is_none() {
            app.refresh();
        }
    }
}
/// Take over the terminal, run the client, and restore the terminal whatever happens.
pub fn main(state: PathBuf) -> Result<()> {
    enable_raw_mode()?;
    let mut stdout = std::io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = ratatui::backend::CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;
    let mut app = App::new(state);
    let result = run(&mut terminal, &mut app, Duration::from_secs(2));
    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::backend::TestBackend;
    fn fixture() -> App {
        let mut app = App::new(PathBuf::from("/nowhere"));
        app.identity = json!({"name":"aurelien","id":"7d7dc33e0000"});
        app.version = json!({"daemon":"0.8.0"});
        app.teams = vec![
            json!({"id":"11111111-2222-4333-8444-555555555555","workspace":"whatsai","role":"admin","members":2,"state":"member"}),
        ];
        app.team = {
            use whatsai_core::{crypto::Identity, governance::replay, protocol::*};
            let a = Identity::generate("aurelien");
            let b = Identity::generate("bob");
            let me = a.member().unwrap();
            let create = a
                .sign(Governance {
                    team: "11111111-2222-4333-8444-555555555555".into(),
                    revision: 0,
                    previous: String::new(),
                    action: "create".into(),
                    member: Some(me.clone()),
                    target: None,
                    repository: None,
                    workspace: Some("whatsai".into()),
                })
                .unwrap();
            let previous = digest(&serde_json::to_vec(&create).unwrap());
            let admit = a
                .sign(Governance {
                    team: "11111111-2222-4333-8444-555555555555".into(),
                    revision: 1,
                    previous,
                    action: "admit".into(),
                    member: Some(b.member().unwrap()),
                    target: None,
                    repository: None,
                    workspace: None,
                })
                .unwrap();
            let mut team =
                replay(&[create, admit], &me.id).unwrap_or_else(|e| panic!("fixture: {e:#}"));
            team.agents.insert(me.id.clone(), json!([{"label":"codex@whatsai","harness":"codex","online":true,"workspace":"whatsai"}]));
            serde_json::to_value(&team).unwrap()
        };
        app.requests = json!([{"member":{"name":"carol","id":"cccc"},"state":"pending","expires":whatsai_core::protocol::now()+3600}]);
        app.agents = json!([{"label":"claude@whatsai","team_name":"whatsai","published":false,"enrolled":true,"online":true,"sessions":1,"unread":{"addressed":2,"shared":0},"worker":null,"workspace":"/w","last_seen":0}]);
        app.status = "ready".into();
        app
    }
    fn screen(app: &App) -> String {
        let backend = TestBackend::new(120, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|f| draw(f, app)).unwrap();
        let buffer = terminal.backend().buffer().clone();
        (0..buffer.area.height)
            .map(|y| {
                (0..buffer.area.width)
                    .map(|x| buffer[(x, y)].symbol().to_string())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }
    #[test]
    fn the_screen_shows_identity_teams_and_members_without_a_daemon() {
        let app = fixture();
        let s = screen(&app);
        assert!(s.contains("whatsai 0.8.0") && s.contains("aurelien 7d7dc33e"));
        assert!(
            s.contains("*whatsai (2)"),
            "team list marks admin role and member count"
        );
        assert!(
            s.contains("founder, admin") && s.contains("aurelien/codex") && s.contains("bob"),
            "{s}"
        );
        assert!(
            s.contains("Requests!"),
            "pending requests are flagged in the tab bar"
        );
        assert!(s.contains("q quit"));
    }
    #[test]
    fn keys_move_between_tabs_and_open_the_send_prompt() {
        let mut app = fixture();
        let tab = |code| KeyEvent::new(code, KeyModifiers::NONE);
        assert!(app.key(tab(KeyCode::Tab)));
        assert_eq!(app.tab, Tab::Inbox);
        app.key(tab(KeyCode::BackTab));
        assert_eq!(app.tab, Tab::Members);
        app.key(tab(KeyCode::Char('s')));
        assert_eq!(app.input, Some(Input::Recipient(String::new())));
        for c in "bob".chars() {
            app.key(tab(KeyCode::Char(c)));
        }
        app.key(tab(KeyCode::Enter));
        assert_eq!(
            app.input,
            Some(Input::Message {
                to: "bob".into(),
                text: String::new()
            })
        );
        let s = screen(&app);
        assert!(s.contains("to bob>"));
        app.key(tab(KeyCode::Esc));
        assert_eq!(app.input, None);
        assert!(!app.key(tab(KeyCode::Char('q'))), "q quits");
    }
    #[test]
    fn agents_tab_renders_visibility_and_unread() {
        let mut app = fixture();
        app.tab = Tab::Agents;
        let s = screen(&app);
        assert!(s.contains("claude@whatsai") && s.contains("enrolled") && s.contains("2+0"));
    }
}
