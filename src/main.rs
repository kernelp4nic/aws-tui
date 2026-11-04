mod aws;
mod details;
mod pty;
mod terminal;
use aws::{Client, Resource, Service};
use crossterm::event::{
    self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEvent,
    MouseEventKind,
};
use ratatui::{
    layout::{Constraint, Layout, Rect},
    style::{Color, Style},
    widgets::{Block, Clear, List, ListState, Paragraph, Row, Table, TableState, Wrap},
    Frame,
};
use std::{
    sync::mpsc::{self, Receiver},
    thread,
    time::Duration,
};

const COLUMN_NAMES: [&str; 5] = ["Name", "ID", "State", "Type", "IP / CIDR"];

enum Reply {
    Loaded(Result<Vec<Resource>, String>),
    Identity(Result<serde_json::Value, String>),
    Action(Result<serde_json::Value, String>),
}
#[derive(Clone)]
enum Action {
    Service(Service),
    Refresh,
    Details,
    DetailsFormat,
    Columns,
    ReadOnly,
    EnableActions,
    Session(usize, String),
    Start,
    Stop,
    Reboot,
    ConnectSsm,
    Quit,
}
impl Action {
    fn label(&self) -> String {
        match self {
            Self::Session(_, target) => format!("Resume SSM {target}"),
            Self::Service(s) => format!("Browse {}", s.name()),
            Self::Refresh => "Refresh resources".into(),
            Self::Columns => "Choose visible columns".into(),
            Self::Details => "Toggle details panel".into(),
            Self::DetailsFormat => "Toggle details table / raw JSON".into(),
            Self::ReadOnly => "Switch to read-only mode".into(),
            Self::EnableActions => "Enable actions and SSM".into(),
            Self::Start => "Start selected instance".into(),
            Self::Stop => "Stop selected instance".into(),
            Self::ConnectSsm => "Connect via SSM".into(),
            Self::Reboot => "Reboot selected instance".into(),
            Self::Quit => "Quit".into(),
        }
    }
}
enum Overlay {
    Palette {
        query: String,
        selected: usize,
    },
    Confirm {
        operation: String,
        id: String,
        input: String,
    },
    Columns {
        selected: usize,
    },
    Region(String),
    Help,
}
struct Session {
    target: String,
    pty: pty::PtySession,
}

#[derive(Clone)]
enum Click {
    Service(Service),
    Resource(usize),
    Command(Action),
    Column(usize),
    Palette,
    Explorer,
    Session(usize),
}
#[derive(Default)]
struct MouseLayout {
    hits: Vec<(Rect, Click)>,
    resources: Rect,
    details: Rect,
    terminal: Rect,
    menu: Rect,
}
struct App {
    mouse: MouseLayout,
    client: Client,
    service: Service,
    resources: Vec<Resource>,
    selected: usize,
    table: TableState,
    query: String,
    searching: bool,
    state_filter: usize,
    details: bool,
    raw_details: bool,
    columns: [bool; 5],
    scroll: u16,
    overlay: Option<Overlay>,
    loading: bool,
    notice: String,
    identity: String,
    tx: mpsc::Sender<Reply>,
    rx: Receiver<Reply>,
    quit: bool,
    pending_session: Option<String>,
    sessions: Vec<Session>,
    active_session: Option<usize>,
    session_prefix: bool,
}
impl App {
    fn new(client: Client) -> Self {
        let (tx, rx) = mpsc::channel();
        Self {
            client,
            mouse: MouseLayout::default(),
            service: Service::Instances,
            resources: vec![],
            selected: 0,
            table: TableState::default(),
            query: String::new(),
            searching: false,
            state_filter: 0,
            details: true,
            raw_details: false,
            columns: [true; 5],
            scroll: 0,
            overlay: None,
            loading: false,
            notice: String::new(),
            identity: "Connecting…".into(),
            tx,
            rx,
            quit: false,
            pending_session: None,
            sessions: vec![],
            active_session: None,
            session_prefix: false,
        }
    }
    fn poll_sessions(&mut self) {
        for session in &mut self.sessions {
            session.pty.poll();
        }
        // Remove in reverse order so active tab indices remain stable.
        for index in (0..self.sessions.len()).rev() {
            if self.sessions[index].pty.exit_ok == Some(true) {
                let session = self.sessions.remove(index);
                self.notice = format!("SSM session with {} ended", session.target);
                match self.active_session {
                    Some(active) if active == index => {
                        self.active_session = None;
                        self.session_prefix = false;
                    }
                    Some(active) if active > index => self.active_session = Some(active - 1),
                    _ => {}
                }
            }
        }
    }
    fn refresh(&mut self) {
        if self.loading {
            return;
        }
        self.loading = true;
        self.notice = "Loading resources…".into();
        let c = self.client.clone();
        let tx = self.tx.clone();
        let service = self.service;
        thread::spawn(move || {
            let _ = tx.send(Reply::Loaded(c.load(service)));
        });
    }
    fn visible(&self) -> Vec<usize> {
        let tokens: Vec<_> = self
            .query
            .to_lowercase()
            .split_whitespace()
            .map(str::to_owned)
            .collect();
        self.resources
            .iter()
            .enumerate()
            .filter(|(_, r)| {
                let state = match self.state_filter {
                    1 => "running",
                    2 => "stopped",
                    _ => "",
                };
                (state.is_empty() || r.state == state)
                    && tokens.iter().all(|q| r.searchable().contains(q))
            })
            .map(|(i, _)| i)
            .collect()
    }
    fn current(&self) -> Option<&Resource> {
        self.visible()
            .get(self.selected)
            .and_then(|i| self.resources.get(*i))
    }
    fn commands(&self, query: &str) -> Vec<Action> {
        let mut all: Vec<_> = Service::ALL.into_iter().map(Action::Service).collect();
        all.extend([
            Action::Refresh,
            Action::Details,
            Action::DetailsFormat,
            Action::Columns,
        ]);
        all.push(if self.client.read_only {
            Action::EnableActions
        } else {
            Action::ReadOnly
        });
        if !self.client.read_only && self.service == Service::Instances {
            all.extend([
                Action::Start,
                Action::Stop,
                Action::Reboot,
                Action::ConnectSsm,
            ]);
        }
        all.extend(
            self.sessions
                .iter()
                .enumerate()
                .map(|(i, s)| Action::Session(i, s.target.clone())),
        );
        all.push(Action::Quit);
        all.into_iter()
            .filter(|a| fuzzy(query.trim_start_matches('>').trim(), &a.label()))
            .collect()
    }
    fn execute(&mut self, action: Action) {
        match action {
            Action::Session(index, _) => self.active_session = Some(index),
            Action::Quit => self.quit = true,
            Action::Service(s) => {
                if !self.loading {
                    self.service = s;
                    self.resources.clear();
                    self.selected = 0;
                    self.query.clear();
                    self.state_filter = 0;
                    self.scroll = 0;
                    self.refresh();
                }
            }
            Action::ConnectSsm => {
                if self.client.read_only {
                    self.notice =
                        "Read-only mode: open Ctrl+P and choose Enable actions and SSM".into();
                } else if !self.loading && self.service == Service::Instances {
                    if let Some(resource) = self.current() {
                        if resource.state == "running" {
                            self.pending_session = Some(resource.id.clone());
                        } else {
                            self.notice = "SSM requires a running instance".into();
                        }
                    }
                }
            }
            Action::Refresh => self.refresh(),
            Action::Columns => self.overlay = Some(Overlay::Columns { selected: 0 }),
            Action::Details => self.details = !self.details,
            Action::DetailsFormat => {
                self.raw_details = !self.raw_details;
                self.scroll = 0;
                self.details = true;
            }
            Action::ReadOnly | Action::EnableActions => {
                if matches!(action, Action::ReadOnly) && !self.sessions.is_empty() {
                    self.notice = "Close SSM tabs before switching to read-only mode".into();
                    return;
                }
                if self.loading {
                    self.notice = "Wait for the current AWS request before changing mode".into();
                    return;
                }
                self.client.read_only = matches!(action, Action::ReadOnly);
                self.pending_session = None;
                self.notice = if self.client.read_only {
                    "Read-only mode enabled".into()
                } else {
                    "Actions enabled: SSM, start, stop and reboot are available".into()
                };
            }
            Action::Start | Action::Stop | Action::Reboot => {
                if self.client.read_only || self.loading || self.service != Service::Instances {
                    return;
                }
                if let Some(r) = self.current() {
                    let operation = match action {
                        Action::Start => "start-instances",
                        Action::Stop => "stop-instances",
                        _ => "reboot-instances",
                    };
                    self.overlay = Some(Overlay::Confirm {
                        operation: operation.into(),
                        id: r.id.clone(),
                        input: String::new(),
                    });
                }
            }
        }
    }
    fn mouse(&mut self, event: MouseEvent) {
        let position = (event.column, event.row).into();
        let delta = match event.kind {
            MouseEventKind::ScrollUp => -3isize,
            MouseEventKind::ScrollDown => 3,
            _ => 0,
        };
        if delta != 0 {
            if self.overlay.is_some() {
                if self.mouse.menu.contains(position) {
                    let code = if delta < 0 {
                        KeyCode::Up
                    } else {
                        KeyCode::Down
                    };
                    for _ in 0..3 {
                        self.key(KeyEvent::new(code, KeyModifiers::NONE));
                    }
                }
            } else if let Some(index) = self.active_session {
                if self.mouse.terminal.contains(position) {
                    self.sessions[index].pty.scroll(-delta);
                }
            } else if self.mouse.details.contains(position) {
                self.scroll = self.scroll.saturating_add_signed(delta as i16);
            } else if self.mouse.resources.contains(position) {
                self.selected = self
                    .selected
                    .saturating_add_signed(delta)
                    .min(self.visible().len().saturating_sub(1));
                self.scroll = 0;
            }
            return;
        }
        if event.kind != MouseEventKind::Down(MouseButton::Left) {
            return;
        }
        let target = self
            .mouse
            .hits
            .iter()
            .rev()
            .find(|(area, _)| area.contains(position))
            .map(|(_, target)| target.clone());
        match target {
            Some(Click::Service(service)) => self.execute(Action::Service(service)),
            Some(Click::Resource(index)) => {
                self.selected = index;
                self.scroll = 0;
            }
            Some(Click::Command(action)) => {
                self.overlay = None;
                self.execute(action);
            }
            Some(Click::Column(index)) => {
                if let Some(Overlay::Columns { selected }) = &mut self.overlay {
                    *selected = index;
                }
                self.key(KeyEvent::new(KeyCode::Char(' '), KeyModifiers::NONE));
            }
            Some(Click::Palette) => {
                self.overlay = Some(Overlay::Palette {
                    query: String::new(),
                    selected: 0,
                })
            }
            Some(Click::Explorer) => {
                self.active_session = None;
                self.session_prefix = false;
            }
            Some(Click::Session(index)) => {
                self.active_session = Some(index);
                self.session_prefix = false;
            }
            None => {}
        }
    }
    fn key(&mut self, key: KeyEvent) {
        if key.kind == KeyEventKind::Release {
            return;
        }
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        if let Some(index) = self.active_session {
            if self.session_prefix {
                self.session_prefix = false;
                match key.code {
                    KeyCode::Char('h') => self.active_session = None,
                    KeyCode::Char('n') => {
                        self.active_session = Some((index + 1) % self.sessions.len())
                    }
                    KeyCode::Char('x') => {
                        self.sessions.remove(index);
                        self.active_session = None;
                    }
                    KeyCode::Char('g') if ctrl => self.sessions[index].pty.write(&[7]),
                    _ => {}
                }
                return;
            }
            if ctrl && key.code == KeyCode::Char('g') {
                self.session_prefix = true;
                return;
            }
            let session = &mut self.sessions[index].pty;
            if key.modifiers.contains(KeyModifiers::SHIFT) && key.code == KeyCode::PageUp {
                session.scroll(10);
            } else if key.modifiers.contains(KeyModifiers::SHIFT) && key.code == KeyCode::PageDown {
                session.scroll(-10);
            } else if !session.exited {
                session.reset_scroll();
                if let Some(bytes) = pty::key_bytes(key, session.app_cursor()) {
                    session.write(&bytes);
                }
            }
            return;
        }
        if ctrl && key.code == KeyCode::Char('c') {
            self.quit = true;
            return;
        }
        if let Some(mut overlay) = self.overlay.take() {
            if key.code == KeyCode::Esc {
                return;
            }
            match &mut overlay {
                Overlay::Palette { query, selected } => match key.code {
                    KeyCode::Char(c) if !ctrl => {
                        query.push(c);
                        *selected = 0;
                    }
                    KeyCode::Backspace => {
                        query.pop();
                        *selected = 0;
                    }
                    KeyCode::Down => {
                        *selected = selected
                            .saturating_add(1)
                            .min(self.commands(query).len().saturating_sub(1))
                    }
                    KeyCode::Up => *selected = selected.saturating_sub(1),
                    KeyCode::Enter => {
                        if let Some(a) = self.commands(query).get(*selected).cloned() {
                            self.execute(a);
                        }
                        return;
                    }
                    _ => {}
                },
                Overlay::Columns { selected } => match key.code {
                    KeyCode::Down | KeyCode::Char('j') => *selected = (*selected + 1).min(4),
                    KeyCode::Up | KeyCode::Char('k') => *selected = selected.saturating_sub(1),
                    KeyCode::Char(' ') => {
                        if !self.columns[*selected]
                            || self.columns.iter().filter(|&&v| v).count() > 1
                        {
                            self.columns[*selected] = !self.columns[*selected];
                        }
                    }
                    KeyCode::Char('a') => self.columns = [true; 5],
                    KeyCode::Enter => return,
                    _ => {}
                },
                Overlay::Region(input) => match key.code {
                    KeyCode::Char(c) if !ctrl => input.push(c),
                    KeyCode::Backspace => {
                        input.pop();
                    }
                    KeyCode::Enter => {
                        if !self.loading
                            && !input.is_empty()
                            && input.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
                        {
                            self.client.region = Some(input.clone());
                            self.resources.clear();
                            self.selected = 0;
                            self.refresh();
                        }
                        return;
                    }
                    _ => {}
                },
                Overlay::Confirm {
                    operation,
                    id,
                    input,
                } => match key.code {
                    KeyCode::Char(c) if !ctrl => input.push(c),
                    KeyCode::Backspace => {
                        input.pop();
                    }
                    KeyCode::Enter if input == id => {
                        if self.client.read_only {
                            return;
                        }
                        self.loading = true;
                        self.notice = format!("Executing {operation}…");
                        let c = self.client.clone();
                        let tx = self.tx.clone();
                        let op = operation.clone();
                        let id = id.clone();
                        thread::spawn(move || {
                            let _ =
                                tx.send(Reply::Action(c.run("ec2", &op, &["--instance-ids", &id])));
                        });
                        return;
                    }
                    _ => {}
                },
                Overlay::Help => {
                    return;
                }
            }
            self.overlay = Some(overlay);
            return;
        }
        if self.searching {
            match key.code {
                KeyCode::Esc | KeyCode::Enter => self.searching = false,
                KeyCode::Backspace => {
                    self.query.pop();
                    self.selected = 0;
                }
                KeyCode::Char(c) if !ctrl => {
                    self.query.push(c);
                    self.selected = 0;
                }
                _ => {}
            }
            return;
        }
        if (ctrl && matches!(key.code, KeyCode::Char('p' | 'k')))
            || key.code == KeyCode::Char(':')
            || (key.modifiers.contains(KeyModifiers::SUPER) && key.code == KeyCode::Char('p'))
        {
            self.overlay = Some(Overlay::Palette {
                query: String::new(),
                selected: 0,
            });
            return;
        }
        match key.code {
            KeyCode::Char('S') => self.execute(Action::ConnectSsm),
            KeyCode::Char('c') => self.execute(Action::Columns),
            KeyCode::Char('q') => self.quit = true,
            KeyCode::Char('/') => self.searching = true,
            KeyCode::Esc => {
                self.query.clear();
                self.state_filter = 0;
                self.selected = 0;
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.selected = (self.selected + 1).min(self.visible().len().saturating_sub(1));
                self.scroll = 0;
            }
            KeyCode::Up | KeyCode::Char('k') => {
                self.selected = self.selected.saturating_sub(1);
                self.scroll = 0;
            }
            KeyCode::Char('g') | KeyCode::Home => self.selected = 0,
            KeyCode::Char('G') | KeyCode::End => {
                self.selected = self.visible().len().saturating_sub(1)
            }
            KeyCode::Tab | KeyCode::BackTab => {
                let i = Service::ALL
                    .iter()
                    .position(|s| *s == self.service)
                    .unwrap_or(0);
                let next = if key.code == KeyCode::BackTab {
                    (i + 4) % 5
                } else {
                    (i + 1) % 5
                };
                self.execute(Action::Service(Service::ALL[next]));
            }
            KeyCode::Char('r') => self.refresh(),
            KeyCode::Char('R') => {
                if !self.loading {
                    self.overlay = Some(Overlay::Region(String::new()));
                }
            }
            KeyCode::Char('s') if self.service == Service::Instances => {
                self.state_filter = (self.state_filter + 1) % 3;
                self.selected = 0;
            }
            KeyCode::Enter => self.details = !self.details,
            KeyCode::Char('J') => self.execute(Action::DetailsFormat),
            KeyCode::PageDown => self.scroll = self.scroll.saturating_add(10),
            KeyCode::PageUp => self.scroll = self.scroll.saturating_sub(10),
            KeyCode::Char('?') => self.overlay = Some(Overlay::Help),
            _ => {}
        }
    }
}
fn fuzzy(query: &str, label: &str) -> bool {
    let label = label.to_lowercase();
    let mut chars = label.chars();
    query
        .to_lowercase()
        .chars()
        .all(|q| chars.by_ref().any(|c| c == q))
}
fn panel(title: impl Into<String>) -> Block<'static> {
    Block::bordered()
        .title(title.into())
        .border_style(Style::default().fg(Color::DarkGray))
}
fn popup(f: &mut Frame, width: u16, height: u16) -> Rect {
    let a = f.area();
    let w = width.min(a.width);
    let h = height.min(a.height);
    let r = Rect::new((a.width - w) / 2, (a.height - h) / 2, w, h);
    f.render_widget(Clear, r);
    r
}
fn session_layout(area: Rect) -> std::rc::Rc<[Rect]> {
    Layout::vertical([
        Constraint::Length(2),
        Constraint::Min(1),
        Constraint::Length(1),
    ])
    .margin(1)
    .split(area)
}
fn session_size(area: Rect) -> (u16, u16) {
    let content = panel("Terminal").inner(session_layout(area)[1]);
    (content.height.max(1), content.width.max(1))
}
fn draw(f: &mut Frame, a: &mut App) {
    a.mouse = MouseLayout::default();
    let base = Style::default()
        .bg(Color::Rgb(20, 23, 29))
        .fg(Color::Rgb(218, 224, 232));
    f.render_widget(Block::default().style(base), f.area());
    if let Some(index) = a.active_session {
        let session = &a.sessions[index];
        let areas = session_layout(f.area());
        let mut tab_x = areas[0].x + 13;
        a.mouse.hits.push((
            Rect::new(areas[0].x, areas[0].y, 12.min(areas[0].width), 1),
            Click::Explorer,
        ));
        for (i, session) in a.sessions.iter().enumerate() {
            let label = format!(
                "{} {}: {}",
                if i == index { "›" } else { " " },
                i + 1,
                session.target
            );
            let width = ratatui::text::Line::from(label).width() as u16;
            if tab_x < areas[0].right() {
                a.mouse.hits.push((
                    Rect::new(tab_x, areas[0].y, width.min(areas[0].right() - tab_x), 1),
                    Click::Session(i),
                ));
            }
            tab_x = tab_x.saturating_add(width + 3);
        }
        a.mouse.terminal = areas[1];
        let tabs = a
            .sessions
            .iter()
            .enumerate()
            .map(|(i, s)| {
                format!(
                    "{} {}: {}",
                    if i == index { "›" } else { " " },
                    i + 1,
                    s.target
                )
            })
            .collect::<Vec<_>>()
            .join("  |  ");
        f.render_widget(
            Paragraph::new(format!(" [Instances] {tabs}")).style(Style::default().fg(Color::Cyan)),
            areas[0],
        );
        let status = if session.pty.exited {
            "ended · Ctrl+G x to close"
        } else {
            "connected / connecting"
        };
        if areas[0].height > 1 {
            f.render_widget(
                Paragraph::new(format!(
                    " {} · {} · {status}",
                    a.client.profile,
                    a.client.region.as_deref().unwrap_or("configured region")
                ))
                .style(Style::default().fg(Color::DarkGray)),
                Rect::new(areas[0].x, areas[0].y + 1, areas[0].width, 1),
            );
        }
        let parser = session.pty.parser.lock().unwrap();
        f.render_widget(
            tui_term::widget::PseudoTerminal::new(parser.screen())
                .block(panel(" Terminal ").style(base)),
            areas[1],
        );
        f.render_widget(Paragraph::new(if a.session_prefix { " Ctrl+G: h instances · n next session · x close tab · Ctrl+G send literal" } else { " Ctrl+G h instances · Ctrl+G n next · Ctrl+G x close · Shift+PgUp/PgDn scroll · exit closes tab" }), areas[2]);
        return;
    }

    let outer = Layout::vertical([
        Constraint::Length(2),
        Constraint::Min(1),
        Constraint::Length(3),
        Constraint::Length(1),
    ])
    .split(f.area());
    let mode = if a.client.read_only {
        "READ ONLY"
    } else {
        "ACTIONS ENABLED"
    };
    f.render_widget(
        Paragraph::new(format!(
            " AWS  /  {}    {}    {} · {} · {}",
            a.service.name(),
            mode,
            a.client.profile,
            a.client.region.as_deref().unwrap_or("configured region"),
            a.identity
        ))
        .style(Style::default().fg(Color::Cyan)),
        outer[0],
    );
    if outer[0].height > 1 {
        let y = outer[0].y + 1;
        f.render_widget(
            Paragraph::new(" [Commands]  [Columns] ").style(Style::default().fg(Color::Cyan)),
            Rect::new(outer[0].x, y, outer[0].width, 1),
        );
        for (x, width, target) in [
            (1, 10, Click::Palette),
            (13, 9, Click::Command(Action::Columns)),
        ] {
            if outer[0].width > x {
                a.mouse
                    .hits
                    .push((Rect::new(x, y, width.min(outer[0].width - x), 1), target));
            }
        }
    }
    let cols = Layout::horizontal([Constraint::Length(21), Constraint::Min(1)]).split(outer[1]);
    let items: Vec<_> = Service::ALL.iter().map(|s| s.name()).collect();
    let mut state =
        ListState::default().with_selected(Service::ALL.iter().position(|s| *s == a.service));
    f.render_stateful_widget(
        List::new(items)
            .block(panel(" Services "))
            .highlight_style(Style::default().fg(Color::Cyan).bg(Color::Rgb(35, 44, 56)))
            .highlight_symbol("› "),
        cols[0],
        &mut state,
    );
    for (index, service) in Service::ALL.iter().enumerate() {
        let y = cols[0].y + 1 + index as u16;
        if y < cols[0].bottom().saturating_sub(1) {
            a.mouse.hits.push((
                Rect::new(cols[0].x + 1, y, cols[0].width.saturating_sub(2), 1),
                Click::Service(*service),
            ));
        }
    }
    let body = if a.details && cols[1].width >= 65 {
        Layout::vertical([Constraint::Percentage(55), Constraint::Percentage(45)])
            .split(cols[1])
            .to_vec()
    } else {
        vec![cols[1]]
    };
    let visible = a.visible();
    a.selected = a.selected.min(visible.len().saturating_sub(1));
    let rows: Vec<_> = visible
        .iter()
        .map(|i| {
            let r = &a.resources[*i];
            let values = [&r.name, &r.id, &r.state, &r.kind, &r.address];
            Row::new(
                values
                    .into_iter()
                    .enumerate()
                    .filter(|(index, _)| a.columns[*index])
                    .map(|(_, value)| value.clone())
                    .collect::<Vec<_>>(),
            )
            .style(Style::default().fg(match r.state.as_str() {
                "running" => Color::Green,
                "stopped" => Color::Yellow,
                _ => Color::White,
            }))
        })
        .collect();
    a.table.select(if visible.is_empty() {
        None
    } else {
        Some(a.selected)
    });
    let title = format!(
        " {}  {}/{}{} ",
        a.service.name(),
        visible.len(),
        a.resources.len(),
        if a.loading { " · loading…" } else { "" }
    );
    f.render_stateful_widget(
        Table::new(
            rows,
            [24, 28, 14, 14, 20]
                .into_iter()
                .enumerate()
                .filter(|(index, _)| a.columns[*index])
                .map(|(_, weight)| Constraint::Fill(weight))
                .collect::<Vec<_>>(),
        )
        .header(
            Row::new(
                COLUMN_NAMES
                    .into_iter()
                    .enumerate()
                    .filter(|(index, _)| a.columns[*index])
                    .map(|(_, name)| name)
                    .collect::<Vec<_>>(),
            )
            .style(Style::default().fg(Color::Cyan)),
        )
        .block(panel(title))
        .row_highlight_style(Style::default().bg(Color::Rgb(35, 44, 56)))
        .highlight_symbol("› "),
        body[0],
        &mut a.table,
    );
    a.mouse.resources = body[0];
    for row in 0..body[0].height.saturating_sub(3) as usize {
        let index = a.table.offset() + row;
        if index >= visible.len() {
            break;
        }
        a.mouse.hits.push((
            Rect::new(
                body[0].x + 1,
                body[0].y + 2 + row as u16,
                body[0].width.saturating_sub(2),
                1,
            ),
            Click::Resource(index),
        ));
    }
    if visible.is_empty() && !a.loading {
        let inner = body[0].inner(ratatui::layout::Margin::new(2, 3));
        f.render_widget(
            Paragraph::new(if a.resources.is_empty() {
                "No resources loaded. Press r to refresh; errors appear below."
            } else {
                "No matches. Esc clears all filters."
            })
            .wrap(Wrap { trim: false }),
            inner,
        );
    }
    if body.len() > 1 {
        a.mouse.details = body[1];
        if let Some(resource) = a.current() {
            if a.raw_details {
                let json = serde_json::to_string_pretty(&resource.raw).unwrap_or_default();
                f.render_widget(
                    Paragraph::new(json)
                        .block(panel(" Raw JSON · J table · PgUp/PgDn "))
                        .scroll((a.scroll, 0)),
                    body[1],
                );
            } else {
                let fields = details::fields(a.service, resource);
                let viewport = body[1].height.saturating_sub(2) as usize;
                let start = (a.scroll as usize).min(fields.len().saturating_sub(viewport));
                a.scroll = start as u16;
                let rows: Vec<_> = fields
                    .iter()
                    .skip(start)
                    .map(|field| {
                        let row = Row::new([field.label.clone(), field.value.clone()]);
                        if field.section {
                            row.style(Style::default().fg(Color::Cyan).bg(Color::Rgb(35, 44, 56)))
                        } else {
                            row
                        }
                    })
                    .collect();
                f.render_widget(
                    Table::new(rows, [Constraint::Length(24), Constraint::Min(1)])
                        .column_spacing(2)
                        .block(panel(format!(
                            " Details · {}–{}/{} · J raw JSON · PgUp/PgDn ",
                            start + 1,
                            (start + viewport).min(fields.len()),
                            fields.len()
                        ))),
                    body[1],
                );
            }
        } else {
            f.render_widget(
                Paragraph::new("Select a resource to inspect its details and tags.")
                    .block(panel(" Details ")),
                body[1],
            );
        }
    }
    let filter = match a.state_filter {
        1 => "running",
        2 => "stopped",
        _ => "all",
    };
    f.render_widget(
        Paragraph::new(format!(
            "{}{}   state: {}   | {}",
            if a.searching { "/ " } else { "Filter: " },
            a.query,
            filter,
            a.notice
        ))
        .wrap(Wrap { trim: false })
        .block(panel(" Status ")),
        outer[2],
    );
    f.render_widget(Paragraph::new(" / filter  s state  c columns  Tab service  Enter details  J JSON  r refresh  R region  S SSM  Ctrl+P commands  ? help  q quit").style(Style::default().fg(Color::DarkGray)),outer[3]);
    if let Some(o) = &a.overlay {
        a.mouse.hits.clear();
        match o {
            Overlay::Palette { query, selected } => {
                let commands = a.commands(query);
                let r = popup(f, 72, 18);
                let chunks = Layout::vertical([Constraint::Length(3), Constraint::Min(1)]).split(r);
                f.render_widget(
                    Paragraph::new(format!("> {query}"))
                        .block(panel(" Command palette · Esc close ")),
                    chunks[0],
                );
                let mut state = ListState::default().with_selected(Some(*selected));
                let items: Vec<_> = commands.iter().map(Action::label).collect();
                f.render_stateful_widget(
                    List::new(items)
                        .block(panel(" Enter to run "))
                        .highlight_symbol("› ")
                        .highlight_style(
                            Style::default().fg(Color::Cyan).bg(Color::Rgb(35, 44, 56)),
                        ),
                    chunks[1],
                    &mut state,
                );
                a.mouse.menu = r;
                for row in 0..chunks[1].height.saturating_sub(2) as usize {
                    if let Some(action) = commands.get(state.offset() + row) {
                        a.mouse.hits.push((
                            Rect::new(
                                chunks[1].x + 1,
                                chunks[1].y + 1 + row as u16,
                                chunks[1].width.saturating_sub(2),
                                1,
                            ),
                            Click::Command(action.clone()),
                        ));
                    }
                }
            }
            Overlay::Confirm {
                operation,
                id,
                input,
            } => {
                let r = popup(f, 72, 9);
                f.render_widget(Paragraph::new(format!("{operation}\nInstance: {id}\nProfile: {} · Region: {}\n\nType the instance ID and press Enter:\n{input}",a.client.profile,a.client.region.as_deref().unwrap_or("configured region"))).wrap(Wrap{trim:false}).block(panel(" Confirm AWS action · Esc cancel ")).style(Style::default().fg(Color::Yellow)),r);
            }
            Overlay::Columns { selected } => {
                let r = popup(f, 58, 9);
                let items: Vec<_> = COLUMN_NAMES
                    .iter()
                    .enumerate()
                    .map(|(index, name)| {
                        format!("[{}] {name}", if a.columns[index] { "x" } else { " " })
                    })
                    .collect();
                let mut state = ListState::default().with_selected(Some(*selected));
                f.render_stateful_widget(
                    List::new(items)
                        .block(panel(" Columns · Space toggle · a all · Enter close "))
                        .highlight_symbol("› ")
                        .highlight_style(
                            Style::default().fg(Color::Cyan).bg(Color::Rgb(35, 44, 56)),
                        ),
                    r,
                    &mut state,
                );
                a.mouse.menu = r;
                for row in 0..r.height.saturating_sub(2) as usize {
                    let index = state.offset() + row;
                    if index < COLUMN_NAMES.len() {
                        a.mouse.hits.push((
                            Rect::new(r.x + 1, r.y + 1 + row as u16, r.width.saturating_sub(2), 1),
                            Click::Column(index),
                        ));
                    }
                }
            }
            Overlay::Region(input) => {
                let r = popup(f, 60, 5);
                f.render_widget(
                    Paragraph::new(format!("Region (for example us-east-1):\n{input}"))
                        .block(panel(" Switch region · Enter apply · Esc cancel ")),
                    r,
                );
            }
            Overlay::Help => {
                let r = popup(f, 76, 16);
                f.render_widget(Paragraph::new("j/k or arrows  Move selection\ng/G           First / last resource\n/             Filter by name, ID, IP, tags, or any JSON value\ns             Cycle EC2 state: all / running / stopped\nTab/Shift+Tab Change service\nEnter         Toggle details panel\nJ             Switch details table / raw JSON\nPgUp/PgDn     Scroll details\nc             Choose visible columns\nS             Connect via SSM (requires actions enabled)\nr / R         Refresh / change region\nCtrl+P/Ctrl+K Command palette (also :)\nEsc           Clear filters / dismiss overlay\nq / Ctrl+C    Quit\n\nChange read-only / actions mode from the command palette.\nActions require typing the exact instance ID.").block(panel(" Keyboard shortcuts · any key closes ")),r);
            }
        }
    }
}
fn options() -> Result<Option<Client>, String> {
    let mut c = Client {
        profile: "default".into(),
        region: None,
        read_only: true,
    };
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--read-only" => c.read_only = true,
            "--allow-actions" => c.read_only = false,
            "--profile" => c.profile = args.next().ok_or("Missing --profile value")?,
            "--region" => c.region = Some(args.next().ok_or("Missing --region value")?),
            "--version" | "-V" => {
                println!("aws-tui {}", env!("CARGO_PKG_VERSION"));
                return Ok(None);
            }
            "--help" | "-h" => {
                println!("aws-tui [--read-only | --allow-actions] [--profile NAME] [--region REGION]\n\nUses AWS CLI v2. Defaults to profile default and read-only.\nCtrl+P opens commands. Actions: start, stop, reboot with typed confirmation.");
                return Ok(None);
            }
            _ => return Err(format!("Unknown option: {arg}")),
        }
    }
    Ok(Some(c))
}
fn run<B: ratatui::backend::Backend<Error = std::io::Error>>(
    terminal: &mut ratatui::Terminal<B>,
    mut app: App,
) -> std::io::Result<()> {
    let _mouse_capture = terminal::MouseCapture::enable()?;
    let c = app.client.clone();
    let tx = app.tx.clone();
    thread::spawn(move || {
        let _ = tx.send(Reply::Identity(c.run("sts", "get-caller-identity", &[])));
    });
    app.refresh();
    let mut drawn_session = None;
    while !app.quit {
        while let Ok(reply) = app.rx.try_recv() {
            match reply {
                Reply::Loaded(result) => {
                    app.loading = false;
                    match result {
                        Ok(rows) => {
                            app.resources = rows;
                            app.selected = 0;
                            app.notice = "Resources updated".into();
                        }
                        Err(e) => app.notice = e,
                    }
                }
                Reply::Identity(result) => {
                    app.identity = match result {
                        Ok(v) => format!(
                            "{} · {}",
                            v["Account"].as_str().unwrap_or("?"),
                            v["Arn"].as_str().unwrap_or("?")
                        ),
                        Err(e) => format!("Identity unavailable: {e}"),
                    }
                }
                Reply::Action(result) => {
                    app.loading = false;
                    match result {
                        Ok(_) => app.refresh(),
                        Err(e) => app.notice = e,
                    }
                }
            }
        }
        if let Some(target) = app.pending_session.take() {
            if let Some(index) = app
                .sessions
                .iter()
                .position(|s| s.target == target && !s.pty.exited)
            {
                app.active_session = Some(index);
            } else {
                let result = app.client.session_command(&target).and_then(|command| {
                    let args: Vec<_> = command
                        .get_args()
                        .map(|arg| arg.to_str().unwrap_or_default())
                        .collect();
                    let size = terminal.size().map_err(|e| e.to_string())?;
                    pty::PtySession::spawn(
                        "aws",
                        &args,
                        session_size(Rect::new(0, 0, size.width, size.height)),
                    )
                });
                match result {
                    Ok(pty) => {
                        app.sessions.push(Session { target, pty });
                        app.active_session = Some(app.sessions.len() - 1);
                    }
                    Err(error) => app.notice = error,
                }
            }
        }
        app.poll_sessions();
        if let Some(index) = app.active_session {
            let size = terminal.size()?;
            let (rows, cols) = session_size(Rect::new(0, 0, size.width, size.height));
            app.sessions[index].pty.resize(rows, cols);
        }
        if drawn_session != app.active_session {
            terminal.clear()?;
            drawn_session = app.active_session;
        }
        terminal.draw(|f| draw(f, &mut app))?;
        if event::poll(Duration::from_millis(80))? {
            match event::read()? {
                Event::Key(key) => app.key(key),
                Event::Mouse(mouse) => app.mouse(mouse),
                _ => {}
            }
        }
    }
    Ok(())
}
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let Some(client) = options().map_err(std::io::Error::other)? else {
        return Ok(());
    };
    let mut setup = ratatui::init();
    let result = ratatui::Terminal::new(terminal::TextRuns(setup.backend_mut()))
        .and_then(|mut terminal| run(&mut terminal, App::new(client)));
    ratatui::restore();
    result?;
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn shell_layout_preserves_first_output_line_and_matches_pty_dimensions() {
        use ratatui::{backend::TestBackend, Terminal};
        let mut app = App::new(Client {
            profile: "default".into(),
            region: None,
            read_only: false,
        });
        let pty = pty::PtySession::spawn("/bin/sh", &["-c", "sleep 30"], (20, 80)).unwrap();
        app.sessions.push(Session {
            target: "i-layout-test".into(),
            pty,
        });
        app.active_session = Some(0);
        for (width, height) in [(120, 40), (60, 16), (40, 10)] {
            let area = Rect::new(0, 0, width, height);
            let (rows, cols) = session_size(area);
            app.sessions[0].pty.resize(rows, cols);
            app.sessions[0]
                .pty
                .parser
                .lock()
                .unwrap()
                .process(b"\x1b[2J\x1b[HFIRST-LINE\r\nPROMPT> ");
            let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
            terminal.draw(|frame| draw(frame, &mut app)).unwrap();
            let inner = panel("Terminal").inner(session_layout(area)[1]);
            assert_eq!((rows, cols), (inner.height, inner.width));
            let buffer = terminal.backend().buffer();
            let line: String = (inner.x..inner.x + 10)
                .map(|x| buffer[(x, inner.y)].symbol())
                .collect();
            assert_eq!(line, "FIRST-LINE");
            assert!(inner.y > session_layout(area)[0].bottom());
        }
    }
    #[test]
    fn mouse_uses_rendered_rows_and_modal_menus() {
        use ratatui::{backend::TestBackend, Terminal};
        let mut app = App::new(Client {
            profile: "default".into(),
            region: None,
            read_only: true,
        });
        for index in 0..50 {
            app.resources.push(Resource {
                id: format!("i-{index}"),
                name: format!("web-{index}"),
                state: "running".into(),
                kind: "t3.micro".into(),
                address: "—".into(),
                raw: serde_json::json!({}),
            });
        }
        let mut terminal = Terminal::new(TestBackend::new(120, 40)).unwrap();
        let mouse = |kind, column, row| MouseEvent {
            kind,
            column,
            row,
            modifiers: KeyModifiers::NONE,
        };
        terminal.draw(|f| draw(f, &mut app)).unwrap();
        let list = app.mouse.resources;
        app.mouse(mouse(MouseEventKind::ScrollDown, list.x + 2, list.y + 3));
        assert_eq!(app.selected, 3);
        app.selected = 35;
        terminal.draw(|f| draw(f, &mut app)).unwrap();
        let offset = app.table.offset();
        app.mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            list.x + 2,
            list.y + 2,
        ));
        assert_eq!(
            app.selected, offset,
            "Clicks must include table scroll offset"
        );
        app.execute(Action::Columns);
        terminal.draw(|f| draw(f, &mut app)).unwrap();
        let (area, _) = app
            .mouse
            .hits
            .iter()
            .find(|(_, target)| matches!(target, Click::Column(1)))
            .unwrap();
        app.mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            area.x,
            area.y,
        ));
        assert!(!app.columns[1]);
        assert!(app.overlay.is_some());
        app.overlay = Some(Overlay::Palette {
            query: "columns".into(),
            selected: 0,
        });
        terminal.draw(|f| draw(f, &mut app)).unwrap();
        let (area, _) = app.mouse.hits[0].clone();
        app.mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            area.x,
            area.y,
        ));
        assert!(matches!(app.overlay, Some(Overlay::Columns { .. })));
    }
    #[test]
    fn embedded_session_routes_keys_and_can_return_to_resources() {
        use ratatui::{backend::TestBackend, Terminal};
        let mut app = App::new(Client {
            profile: "default".into(),
            region: None,
            read_only: false,
        });
        let pty = pty::PtySession::spawn("/bin/sh", &["-c", "cat"], (20, 80)).unwrap();
        app.sessions.push(Session {
            target: "i-local-test".into(),
            pty,
        });
        app.active_session = Some(0);
        app.key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL));
        assert!(!app.quit, "Ctrl+C belongs to the embedded shell");
        for (width, height) in [(100, 30), (15, 5)] {
            let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
            terminal.draw(|frame| draw(frame, &mut app)).unwrap();
        }
        app.key(KeyEvent::new(KeyCode::Char('g'), KeyModifiers::CONTROL));
        app.key(KeyEvent::new(KeyCode::Char('h'), KeyModifiers::NONE));
        assert!(app.active_session.is_none());
        assert_eq!(app.sessions.len(), 1);
        app.execute(Action::ReadOnly);
        assert!(
            !app.client.read_only,
            "A live session cannot be hidden by read-only mode"
        );
        app.execute(Action::Session(0, "i-local-test".into()));
        app.key(KeyEvent::new(KeyCode::Char('g'), KeyModifiers::CONTROL));
        app.key(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE));
        assert!(app.sessions.is_empty());
    }
    #[test]
    fn successful_sessions_close_and_failed_sessions_keep_their_output() {
        let mut app = App::new(Client {
            profile: "default".into(),
            region: None,
            read_only: false,
        });
        for (target, success) in [("i-success", true), ("i-failed", false)] {
            let mut pty = pty::PtySession::spawn("/bin/sh", &["-c", "exit"], (20, 80)).unwrap();
            pty.exited = true;
            pty.exit_ok = Some(success);
            app.sessions.push(Session {
                target: target.into(),
                pty,
            });
        }
        app.active_session = Some(1);
        app.poll_sessions();
        assert_eq!(app.sessions.len(), 1);
        assert_eq!(app.sessions[0].target, "i-failed");
        assert_eq!(app.active_session, Some(0));
        app.sessions[0].pty.exit_ok = Some(true);
        app.session_prefix = true;
        app.poll_sessions();
        assert!(app.sessions.is_empty());
        assert!(app.active_session.is_none());
        assert!(!app.session_prefix);
    }
    #[test]
    fn action_mode_can_be_changed_from_the_palette() {
        let mut app = App::new(Client {
            profile: "default".into(),
            region: None,
            read_only: true,
        });
        assert!(app
            .commands("")
            .iter()
            .any(|a| matches!(a, Action::EnableActions)));
        app.execute(Action::EnableActions);
        assert!(!app.client.read_only);
        assert!(app
            .commands("")
            .iter()
            .any(|a| matches!(a, Action::ConnectSsm)));
        app.execute(Action::ReadOnly);
        assert!(app.client.read_only);
        assert!(!app
            .commands("")
            .iter()
            .any(|a| matches!(a, Action::ConnectSsm | Action::Stop)));
        app.loading = true;
        app.execute(Action::EnableActions);
        assert!(app.client.read_only);
    }
    #[test]
    fn palette_fuzzy_matches() {
        assert!(fuzzy("brscg", "Browse Security Groups"));
        assert!(!fuzzy("xyz", "Refresh"));
    }
    #[test]
    fn readonly_hides_actions_and_cannot_execute() {
        let mut a = App::new(Client {
            profile: "default".into(),
            region: None,
            read_only: true,
        });
        assert!(!a
            .commands("")
            .iter()
            .any(|a| matches!(a, Action::Stop | Action::Start | Action::Reboot)));
        a.execute(Action::Stop);
        assert!(a.overlay.is_none());
    }
    #[test]
    fn table_preserves_emoji_graphemes_when_selection_changes() {
        use ratatui::{backend::TestBackend, Terminal};
        let mut app = App::new(Client {
            profile: "default".into(),
            region: None,
            read_only: true,
        });
        app.details = false;
        for (index, emoji) in ["🇨🇱", "🇺🇾", "🇦🇷", "👩‍💻", "⚖️", "🚀"].iter().enumerate()
        {
            app.resources.push(Resource {
                id: format!("i-{index}"),
                name: format!("[web {emoji}]"),
                state: "running".into(),
                kind: "t3.micro".into(),
                address: "—".into(),
                raw: serde_json::json!({}),
            });
        }
        let mut terminal = Terminal::new(TestBackend::new(150, 30)).unwrap();
        for selection in [0, 1, 5, 0] {
            app.selected = selection;
            terminal.draw(|frame| draw(frame, &mut app)).unwrap();
            let buffer = terminal.backend().buffer();
            for emoji in ["🇨🇱", "🇺🇾", "🇦🇷", "👩‍💻", "⚖️", "🚀"] {
                assert!(
                    buffer.content.iter().any(|cell| cell.symbol() == emoji),
                    "Emoji must remain a single grapheme: {emoji}"
                );
            }
        }
    }
    #[test]
    fn renders_small_and_large_terminal() {
        use ratatui::{backend::TestBackend, Terminal};
        let mut a = App::new(Client {
            profile: "default".into(),
            region: None,
            read_only: true,
        });
        for (w, h) in [(120, 40), (40, 12), (10, 4)] {
            let mut t = Terminal::new(TestBackend::new(w, h)).unwrap();
            t.draw(|f| draw(f, &mut a)).unwrap();
            a.overlay = Some(Overlay::Palette {
                query: String::new(),
                selected: 0,
            });
            t.draw(|f| draw(f, &mut a)).unwrap();
            a.overlay = None;
        }
    }
}
