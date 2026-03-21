//! TUI dashboard for human oversight of ai-agent vai activity.
//!
//! Displays real-time agent activity, workspace status, conflicts, issues,
//! and version history in an interactive terminal UI powered by ratatui.
//!
//! ## Views
//!
//! - **Overview**: five panels showing active work, conflicts, issues, recent
//!   versions, and system health.
//! - **DrillDown**: full detail view for a selected workspace, escalation,
//!   or issue.  Enter to open, Esc to return.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crossterm::{
    event::{self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEventKind},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{
    layout::{Alignment, Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Cell, List, ListItem, ListState, Paragraph, Row, Table, Tabs},
    Frame, Terminal,
};
use thiserror::Error;
use uuid::Uuid;

use crate::escalation::{Escalation, EscalationStatus, EscalationStore, ResolutionOption};
use crate::graph::GraphSnapshot;
use crate::issue::{Issue, IssueFilter, IssueStatus, IssueStore};
use crate::version;
use crate::workspace;

/// Errors that can occur in the dashboard.
#[derive(Debug, Error)]
pub enum DashboardError {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("Repository not initialised — run `vai init` first")]
    NotInitialised,

    #[error("Workspace error: {0}")]
    Workspace(#[from] crate::workspace::WorkspaceError),

    #[error("Version error: {0}")]
    Version(#[from] crate::version::VersionError),

    #[error("Graph error: {0}")]
    Graph(#[from] crate::graph::GraphError),

    #[error("Issue error: {0}")]
    Issue(#[from] crate::issue::IssueError),

    #[error("Escalation error: {0}")]
    Escalation(#[from] crate::escalation::EscalationError),
}

// ── Panel ─────────────────────────────────────────────────────────────────────

/// The five dashboard panels, cycled with Tab.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Panel {
    ActiveWork = 0,
    Conflicts = 1,
    Issues = 2,
    RecentVersions = 3,
    SystemHealth = 4,
}

impl Panel {
    const ALL: [Panel; 5] = [
        Panel::ActiveWork,
        Panel::Conflicts,
        Panel::Issues,
        Panel::RecentVersions,
        Panel::SystemHealth,
    ];

    fn title(self) -> &'static str {
        match self {
            Panel::ActiveWork => "Active Work",
            Panel::Conflicts => "Conflicts",
            Panel::Issues => "Issues",
            Panel::RecentVersions => "Recent Versions",
            Panel::SystemHealth => "System Health",
        }
    }

    fn next(self) -> Panel {
        let idx = (self as usize + 1) % Panel::ALL.len();
        Panel::ALL[idx]
    }

    fn prev(self) -> Panel {
        let idx = (self as usize + Panel::ALL.len() - 1) % Panel::ALL.len();
        Panel::ALL[idx]
    }
}

// ── DrillDown state ───────────────────────────────────────────────────────────

/// Content shown in the drill-down view.
#[derive(Debug, Clone)]
pub enum DrillDownContent {
    /// Full workspace detail: metadata + overlay file list.
    Workspace {
        meta: workspace::WorkspaceMeta,
        /// Changed files relative to the overlay directory.
        files: Vec<String>,
    },
    /// Full escalation detail with interactive resolution picker.
    Escalation {
        escalation: Escalation,
        /// Currently highlighted resolution option index.
        resolution_cursor: usize,
        /// Set once the user confirms a resolution.
        chosen: Option<ResolutionOption>,
    },
    /// Full issue detail: description + linked workspace IDs.
    Issue {
        issue: Issue,
        linked_workspaces: Vec<Uuid>,
        scroll: u16,
    },
}

// ── App state ─────────────────────────────────────────────────────────────────

/// Snapshot of repository state shown in the dashboard.
#[derive(Default)]
struct DashboardData {
    workspaces: Vec<workspace::WorkspaceMeta>,
    pending_escalations: Vec<crate::escalation::Escalation>,
    issue_counts: IssueCounts,
    recent_versions: Vec<crate::version::VersionMeta>,
    graph_stats: Option<crate::graph::GraphStats>,
}

#[derive(Default)]
struct IssueCounts {
    open: usize,
    in_progress: usize,
    resolved: usize,
    closed: usize,
}

/// Current view — overview or a drill-down panel.
enum View {
    Overview,
    DrillDown(DrillDownContent),
}

/// Application state for the TUI.
struct App {
    vai_dir: PathBuf,
    selected_panel: Panel,
    data: DashboardData,
    last_refresh: Instant,
    /// Scroll / selection offset for the focused list.
    list_state: ListState,
    /// Current view mode.
    view: View,
    /// Active filter text for the Active Work panel (empty = no filter).
    filter: String,
    /// Whether filter-input mode is active (user is typing to filter).
    filter_active: bool,
    should_quit: bool,
    /// Feedback message shown at the bottom (e.g. "Resolved ✓").
    status_message: Option<String>,
}

impl App {
    fn new(vai_dir: PathBuf) -> Self {
        let mut app = App {
            vai_dir,
            selected_panel: Panel::ActiveWork,
            data: DashboardData::default(),
            last_refresh: Instant::now() - Duration::from_secs(60),
            list_state: ListState::default(),
            view: View::Overview,
            filter: String::new(),
            filter_active: false,
            should_quit: false,
            status_message: None,
        };
        app.list_state.select(Some(0));
        app
    }

    /// Reload all data from disk.
    fn refresh(&mut self) {
        self.last_refresh = Instant::now();
        let vai_dir = &self.vai_dir;

        // Workspaces — all
        self.data.workspaces = workspace::list_all(vai_dir).unwrap_or_default();

        // Escalations — pending only
        if let Ok(store) = EscalationStore::open(vai_dir) {
            self.data.pending_escalations = store
                .list(Some(&EscalationStatus::Pending))
                .unwrap_or_default();
        }

        // Issue counts
        if let Ok(store) = IssueStore::open(vai_dir) {
            let all = store.list(&IssueFilter::default()).unwrap_or_default();
            let mut counts = IssueCounts::default();
            for issue in &all {
                match issue.status {
                    IssueStatus::Open => counts.open += 1,
                    IssueStatus::InProgress => counts.in_progress += 1,
                    IssueStatus::Resolved => counts.resolved += 1,
                    IssueStatus::Closed => counts.closed += 1,
                }
            }
            self.data.issue_counts = counts;
        }

        // Recent versions — last 10
        self.data.recent_versions = version::list_versions(vai_dir)
            .unwrap_or_default()
            .into_iter()
            .rev()
            .take(10)
            .collect();

        // Graph stats
        let graph_db = vai_dir.join("graph").join("snapshot.db");
        if graph_db.exists() {
            if let Ok(snap) = GraphSnapshot::open(&graph_db) {
                self.data.graph_stats = snap.stats().ok();
            }
        }
    }

    /// Filtered workspace list for the Active Work panel.
    fn filtered_workspaces(&self) -> Vec<&workspace::WorkspaceMeta> {
        if self.filter.is_empty() {
            self.data.workspaces.iter().collect()
        } else {
            let lower = self.filter.to_lowercase();
            self.data
                .workspaces
                .iter()
                .filter(|ws| ws.intent.to_lowercase().contains(&lower))
                .collect()
        }
    }

    fn handle_key(&mut self, key: KeyCode) {
        // Drill-down view has its own key handling.
        if matches!(self.view, View::DrillDown(_)) {
            self.handle_key_drilldown(key);
            return;
        }

        // Filter-input mode for Active Work panel.
        if self.filter_active {
            match key {
                KeyCode::Esc => {
                    self.filter_active = false;
                    self.filter.clear();
                    self.list_state.select(Some(0));
                }
                KeyCode::Enter => {
                    self.filter_active = false;
                }
                KeyCode::Backspace => {
                    self.filter.pop();
                }
                KeyCode::Char(c) => {
                    self.filter.push(c);
                    self.list_state.select(Some(0));
                }
                _ => {}
            }
            return;
        }

        match key {
            KeyCode::Char('q') => self.should_quit = true,
            KeyCode::Esc => {
                // Clear filter if present.
                if !self.filter.is_empty() {
                    self.filter.clear();
                    self.list_state.select(Some(0));
                } else {
                    self.should_quit = true;
                }
            }
            KeyCode::Tab => {
                self.selected_panel = self.selected_panel.next();
                self.filter.clear();
                self.filter_active = false;
                self.list_state.select(Some(0));
            }
            KeyCode::BackTab => {
                self.selected_panel = self.selected_panel.prev();
                self.filter.clear();
                self.filter_active = false;
                self.list_state.select(Some(0));
            }
            KeyCode::Down | KeyCode::Char('j') => self.scroll_down(),
            KeyCode::Up | KeyCode::Char('k') => self.scroll_up(),
            KeyCode::Char('r') => self.refresh(),
            // '/' starts filter input on the Active Work panel.
            KeyCode::Char('/') if self.selected_panel == Panel::ActiveWork => {
                self.filter_active = true;
                self.filter.clear();
            }
            // Enter drills into the selected item.
            KeyCode::Enter => self.enter_drilldown(),
            _ => {}
        }
    }

    fn handle_key_drilldown(&mut self, key: KeyCode) {
        match &mut self.view {
            View::DrillDown(DrillDownContent::Escalation {
                escalation,
                resolution_cursor,
                chosen,
            }) => {
                let opts = &escalation.resolution_options;
                let already_chosen = chosen.is_some();
                match key {
                    KeyCode::Esc => {
                        self.view = View::Overview;
                        self.refresh();
                    }
                    // After a resolution is chosen, Enter returns to overview.
                    KeyCode::Enter if already_chosen => {
                        self.view = View::Overview;
                        self.refresh();
                    }
                    KeyCode::Up | KeyCode::Char('k') => {
                        if !opts.is_empty() {
                            *resolution_cursor = resolution_cursor.saturating_sub(1);
                        }
                    }
                    KeyCode::Down | KeyCode::Char('j') => {
                        if !opts.is_empty() {
                            *resolution_cursor =
                                (*resolution_cursor + 1).min(opts.len() - 1);
                        }
                    }
                    KeyCode::Enter => {
                        // Confirm resolution.
                        if let Some(opt) = opts.get(*resolution_cursor) {
                            *chosen = Some(opt.clone());
                            let esc_id = escalation.id;
                            let opt_clone = opt.clone();
                            if let Ok(store) = EscalationStore::open(&self.vai_dir) {
                                let event_log_dir = self.vai_dir.join("event_log");
                                if let Ok(mut event_log) =
                                    crate::event_log::EventLog::open(&event_log_dir)
                                {
                                    let _ = store.resolve(
                                        esc_id,
                                        opt_clone,
                                        "dashboard".to_string(),
                                        &mut event_log,
                                    );
                                }
                            }
                            self.status_message = Some("Escalation resolved ✓".to_string());
                        }
                    }
                    _ => {}
                }
            }
            View::DrillDown(DrillDownContent::Issue { scroll, .. }) => match key {
                KeyCode::Esc | KeyCode::Char('q') => {
                    self.view = View::Overview;
                }
                KeyCode::Down | KeyCode::Char('j') => {
                    *scroll = scroll.saturating_add(1);
                }
                KeyCode::Up | KeyCode::Char('k') => {
                    *scroll = scroll.saturating_sub(1);
                }
                _ => {}
            },
            View::DrillDown(_) => match key {
                KeyCode::Esc | KeyCode::Char('q') => {
                    self.view = View::Overview;
                }
                KeyCode::Down | KeyCode::Char('j') => self.scroll_down(),
                KeyCode::Up | KeyCode::Char('k') => self.scroll_up(),
                _ => {}
            },
            View::Overview => unreachable!(),
        }
    }

    /// Open a drill-down for the currently selected item.
    fn enter_drilldown(&mut self) {
        let idx = match self.list_state.selected() {
            Some(i) => i,
            None => return,
        };

        match self.selected_panel {
            Panel::ActiveWork => {
                let workspaces = self.filtered_workspaces();
                if let Some(meta) = workspaces.get(idx) {
                    let meta = (*meta).clone();
                    let files = overlay_files(&self.vai_dir, &meta.id.to_string());
                    self.view = View::DrillDown(DrillDownContent::Workspace { meta, files });
                }
            }
            Panel::Conflicts => {
                if let Some(esc) = self.data.pending_escalations.get(idx) {
                    let escalation = esc.clone();
                    self.view = View::DrillDown(DrillDownContent::Escalation {
                        escalation,
                        resolution_cursor: 0,
                        chosen: None,
                    });
                }
            }
            Panel::Issues => {
                // Load issues fresh and drill into the nth one.
                if let Ok(store) = IssueStore::open(&self.vai_dir) {
                    let all = store.list(&IssueFilter::default()).unwrap_or_default();
                    if let Some(issue) = all.into_iter().nth(idx) {
                        let linked_workspaces =
                            store.linked_workspaces(issue.id).unwrap_or_default();
                        self.view = View::DrillDown(DrillDownContent::Issue {
                            issue,
                            linked_workspaces,
                            scroll: 0,
                        });
                    }
                }
            }
            Panel::RecentVersions | Panel::SystemHealth => {}
        }
    }

    fn scroll_down(&mut self) {
        let len = self.current_list_len();
        if len == 0 {
            return;
        }
        let i = self.list_state.selected().unwrap_or(0);
        self.list_state.select(Some((i + 1).min(len - 1)));
    }

    fn scroll_up(&mut self) {
        let i = self.list_state.selected().unwrap_or(0);
        self.list_state.select(Some(i.saturating_sub(1)));
    }

    fn current_list_len(&self) -> usize {
        match self.selected_panel {
            Panel::ActiveWork => self.filtered_workspaces().len(),
            Panel::Conflicts => self.data.pending_escalations.len(),
            Panel::Issues => {
                self.data.issue_counts.open
                    + self.data.issue_counts.in_progress
                    + self.data.issue_counts.resolved
                    + self.data.issue_counts.closed
            }
            Panel::RecentVersions => self.data.recent_versions.len(),
            Panel::SystemHealth => 0,
        }
    }
}

// ── Entry point ───────────────────────────────────────────────────────────────

/// Launch the TUI dashboard in local mode, polling `.vai/` for changes.
pub fn run(vai_dir: &Path) -> Result<(), DashboardError> {
    if !vai_dir.exists() {
        return Err(DashboardError::NotInitialised);
    }

    enable_raw_mode()?;
    let mut stdout = std::io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
    let backend = ratatui::backend::CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let mut app = App::new(vai_dir.to_path_buf());
    let refresh_interval = Duration::from_secs(2);
    let tick_rate = Duration::from_millis(100);

    let result = run_loop(&mut terminal, &mut app, refresh_interval, tick_rate);

    disable_raw_mode()?;
    execute!(
        terminal.backend_mut(),
        LeaveAlternateScreen,
        DisableMouseCapture
    )?;
    terminal.show_cursor()?;

    result
}

fn run_loop<B: ratatui::backend::Backend>(
    terminal: &mut Terminal<B>,
    app: &mut App,
    refresh_interval: Duration,
    tick_rate: Duration,
) -> Result<(), DashboardError> {
    loop {
        if app.last_refresh.elapsed() >= refresh_interval {
            app.refresh();
        }

        terminal.draw(|f| render(f, app))?;

        if event::poll(tick_rate)? {
            if let Event::Key(key) = event::read()? {
                if key.kind == KeyEventKind::Press {
                    app.handle_key(key.code);
                }
            }
        }

        if app.should_quit {
            break;
        }
    }
    Ok(())
}

// ── Rendering — overview ──────────────────────────────────────────────────────

/// Render the full dashboard.
fn render(f: &mut Frame, app: &App) {
    match &app.view {
        View::Overview => render_overview(f, app),
        View::DrillDown(content) => render_drilldown(f, app, content),
    }
}

fn render_overview(f: &mut Frame, app: &App) {
    let size = f.area();

    let outer = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(3), Constraint::Length(3)])
        .split(size);

    let main_area = outer[0];
    let status_bar_area = outer[1];

    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Percentage(55), Constraint::Percentage(45)])
        .split(main_area);

    let top_cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(60), Constraint::Percentage(40)])
        .split(rows[0]);

    let bot_cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(35), Constraint::Percentage(65)])
        .split(rows[1]);

    render_active_work(f, app, top_cols[0]);
    render_conflicts(f, app, top_cols[1]);
    render_issues(f, app, bot_cols[0]);
    render_recent_versions(f, app, bot_cols[1]);
    render_status_bar(f, app, status_bar_area);
}

fn panel_block(title: &str, selected: bool) -> Block<'_> {
    let style = if selected {
        Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(Color::Gray)
    };
    Block::default()
        .borders(Borders::ALL)
        .border_style(style)
        .title(Span::styled(format!(" {title} "), style))
}

fn render_active_work(f: &mut Frame, app: &App, area: Rect) {
    let selected = app.selected_panel == Panel::ActiveWork;
    let filter_label = if app.filter_active {
        format!(" Active Work [/{}▌] ", app.filter)
    } else if !app.filter.is_empty() {
        format!(" Active Work [/{}] ", app.filter)
    } else {
        " Active Work ".to_string()
    };
    let style = if selected {
        Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(Color::Gray)
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(style)
        .title(Span::styled(filter_label, style));

    let inner = block.inner(area);
    f.render_widget(block, area);

    let workspaces = app.filtered_workspaces();

    if workspaces.is_empty() {
        let msg = Paragraph::new(if app.filter.is_empty() {
            "No active workspaces"
        } else {
            "No matches"
        })
        .style(Style::default().fg(Color::DarkGray))
        .alignment(Alignment::Center);
        f.render_widget(msg, inner);
        return;
    }

    let header = Row::new(vec![
        Cell::from("ID").style(Style::default().add_modifier(Modifier::BOLD)),
        Cell::from("Intent").style(Style::default().add_modifier(Modifier::BOLD)),
        Cell::from("Status").style(Style::default().add_modifier(Modifier::BOLD)),
    ])
    .style(Style::default().fg(Color::Yellow));

    let rows: Vec<Row> = workspaces
        .iter()
        .enumerate()
        .map(|(i, ws)| {
            let id_str = ws.id.to_string();
            let id_short = &id_str[..id_str.len().min(8)];
            let intent = truncate(&ws.intent, inner.width.saturating_sub(20) as usize);
            let status = ws.status.as_str();

            let row_style = if selected && app.list_state.selected() == Some(i) {
                Style::default().bg(Color::DarkGray)
            } else {
                Style::default()
            };

            Row::new(vec![
                Cell::from(id_short.to_string()),
                Cell::from(intent),
                Cell::from(status).style(Style::default().fg(status_color(status))),
            ])
            .style(row_style)
        })
        .collect();

    let table = Table::new(
        rows,
        [
            Constraint::Length(8),
            Constraint::Min(10),
            Constraint::Length(10),
        ],
    )
    .header(header)
    .row_highlight_style(Style::default().add_modifier(Modifier::BOLD));

    f.render_widget(table, inner);
}

fn render_conflicts(f: &mut Frame, app: &App, area: Rect) {
    let selected = app.selected_panel == Panel::Conflicts;
    let block = panel_block("Conflicts", selected);
    let inner = block.inner(area);
    f.render_widget(block, area);

    if app.data.pending_escalations.is_empty() {
        let msg = Paragraph::new("No pending conflicts")
            .style(Style::default().fg(Color::Green))
            .alignment(Alignment::Center);
        f.render_widget(msg, inner);
        return;
    }

    let items: Vec<ListItem> = app
        .data
        .pending_escalations
        .iter()
        .enumerate()
        .map(|(i, esc)| {
            let id_short = &esc.id.to_string()[..8];
            let sev = esc.severity.as_str();
            let sev_color = match sev {
                "critical" => Color::Red,
                "high" => Color::LightRed,
                _ => Color::Yellow,
            };
            let label = format!("{id_short} [{sev}]");
            let row_style = if selected && app.list_state.selected() == Some(i) {
                Style::default().bg(Color::DarkGray)
            } else {
                Style::default()
            };
            ListItem::new(Span::styled(label, Style::default().fg(sev_color))).style(row_style)
        })
        .collect();

    let list = List::new(items);
    f.render_widget(list, inner);
}

fn render_issues(f: &mut Frame, app: &App, area: Rect) {
    let selected = app.selected_panel == Panel::Issues;
    let block = panel_block("Issues", selected);
    let inner = block.inner(area);
    f.render_widget(block, area);

    let c = &app.data.issue_counts;
    let lines = vec![
        Line::from(vec![
            Span::raw("Open:        "),
            Span::styled(c.open.to_string(), Style::default().fg(Color::Yellow)),
        ]),
        Line::from(vec![
            Span::raw("In Progress: "),
            Span::styled(c.in_progress.to_string(), Style::default().fg(Color::Cyan)),
        ]),
        Line::from(vec![
            Span::raw("Resolved:    "),
            Span::styled(c.resolved.to_string(), Style::default().fg(Color::Green)),
        ]),
        Line::from(vec![
            Span::raw("Closed:      "),
            Span::styled(c.closed.to_string(), Style::default().fg(Color::DarkGray)),
        ]),
    ];

    let para = Paragraph::new(lines);
    f.render_widget(para, inner);
}

fn render_recent_versions(f: &mut Frame, app: &App, area: Rect) {
    let selected = app.selected_panel == Panel::RecentVersions;
    let block = panel_block("Recent Versions", selected);
    let inner = block.inner(area);
    f.render_widget(block, area);

    if app.data.recent_versions.is_empty() {
        let msg = Paragraph::new("No versions yet")
            .style(Style::default().fg(Color::DarkGray))
            .alignment(Alignment::Center);
        f.render_widget(msg, inner);
        return;
    }

    let items: Vec<ListItem> = app
        .data
        .recent_versions
        .iter()
        .enumerate()
        .map(|(i, v)| {
            let date = v.created_at.format("%m-%d %H:%M").to_string();
            let intent = truncate(&v.intent, inner.width.saturating_sub(20) as usize);
            let label = format!("{:>4}  {}  {}", v.version_id, date, intent);
            let row_style = if selected && app.list_state.selected() == Some(i) {
                Style::default().bg(Color::DarkGray)
            } else {
                Style::default()
            };
            ListItem::new(Span::raw(label)).style(row_style)
        })
        .collect();

    let list = List::new(items);
    f.render_widget(list, inner);
}

fn render_status_bar(f: &mut Frame, app: &App, area: Rect) {
    let stats = app.data.graph_stats.as_ref();
    let entities = stats.map_or(0, |s| s.entity_count);
    let relationships = stats.map_or(0, |s| s.relationship_count);
    let ws_count = app.data.workspaces.len();

    let elapsed = app.last_refresh.elapsed().as_secs();
    let refresh_str = if elapsed < 5 {
        "just now".to_string()
    } else {
        format!("{elapsed}s ago")
    };

    let msg_part = if let Some(msg) = &app.status_message {
        format!(" | {msg}")
    } else {
        String::new()
    };

    let tabs_titles: Vec<Line> = Panel::ALL
        .iter()
        .map(|p| {
            let style = if *p == app.selected_panel {
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD | Modifier::UNDERLINED)
            } else {
                Style::default().fg(Color::Gray)
            };
            Line::from(Span::styled(p.title(), style))
        })
        .collect();

    let tabs = Tabs::new(tabs_titles)
        .block(Block::default().borders(Borders::ALL).title(Span::styled(
            format!(
                " {entities} entities | {relationships} rels | {ws_count} ws | refreshed {refresh_str}{msg_part} | [Tab] nav  [/] filter  [Enter] drill-in  [r] refresh  [q] quit "
            ),
            Style::default().fg(Color::Gray),
        )))
        .select(app.selected_panel as usize)
        .style(Style::default())
        .highlight_style(Style::default().fg(Color::Cyan));

    f.render_widget(tabs, area);
}

// ── Rendering — drill-down ────────────────────────────────────────────────────

fn render_drilldown(f: &mut Frame, app: &App, content: &DrillDownContent) {
    let size = f.area();

    // Two sections: detail area + keybinding bar
    let layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(3), Constraint::Length(3)])
        .split(size);

    match content {
        DrillDownContent::Workspace { meta, files } => {
            render_workspace_detail(f, app, meta, files, layout[0]);
        }
        DrillDownContent::Escalation {
            escalation,
            resolution_cursor,
            chosen,
        } => {
            render_escalation_detail(f, escalation, *resolution_cursor, chosen.as_ref(), layout[0]);
        }
        DrillDownContent::Issue {
            issue,
            linked_workspaces,
            scroll,
        } => {
            render_issue_detail(f, issue, linked_workspaces, *scroll, layout[0]);
        }
    }

    render_drilldown_bar(f, content, layout[1]);
}

fn render_workspace_detail(
    f: &mut Frame,
    app: &App,
    meta: &workspace::WorkspaceMeta,
    files: &[String],
    area: Rect,
) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Cyan))
        .title(Span::styled(
            format!(" Workspace: {} ", &meta.id.to_string()[..8]),
            Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD),
        ));
    let inner = block.inner(area);
    f.render_widget(block, area);

    let cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(45), Constraint::Percentage(55)])
        .split(inner);

    // Left: metadata
    let status_color_val = status_color(meta.status.as_str());
    let mut meta_lines = vec![
        Line::from(vec![
            Span::styled("Intent:     ", Style::default().fg(Color::Gray)),
            Span::raw(meta.intent.clone()),
        ]),
        Line::from(vec![
            Span::styled("Status:     ", Style::default().fg(Color::Gray)),
            Span::styled(
                meta.status.as_str(),
                Style::default().fg(status_color_val),
            ),
        ]),
        Line::from(vec![
            Span::styled("Base:       ", Style::default().fg(Color::Gray)),
            Span::raw(meta.base_version.clone()),
        ]),
        Line::from(vec![
            Span::styled("Created:    ", Style::default().fg(Color::Gray)),
            Span::raw(meta.created_at.format("%Y-%m-%d %H:%M:%S UTC").to_string()),
        ]),
        Line::from(vec![
            Span::styled("Updated:    ", Style::default().fg(Color::Gray)),
            Span::raw(meta.updated_at.format("%Y-%m-%d %H:%M:%S UTC").to_string()),
        ]),
    ];
    if let Some(issue_id) = meta.issue_id {
        meta_lines.push(Line::from(vec![
            Span::styled("Issue:      ", Style::default().fg(Color::Gray)),
            Span::styled(
                issue_id.to_string(),
                Style::default().fg(Color::Cyan),
            ),
        ]));
    }

    // Show active workspace indicator
    let active_id = workspace::active_id(&app.vai_dir);
    if active_id.as_deref() == Some(meta.id.to_string().as_str()) {
        meta_lines.push(Line::from(Span::styled(
            "★ Currently active workspace",
            Style::default().fg(Color::Yellow),
        )));
    }

    let meta_block = Block::default()
        .borders(Borders::RIGHT)
        .border_style(Style::default().fg(Color::DarkGray))
        .title(Span::styled(" Metadata ", Style::default().fg(Color::Gray)));
    let meta_inner = meta_block.inner(cols[0]);
    f.render_widget(meta_block, cols[0]);
    f.render_widget(Paragraph::new(meta_lines), meta_inner);

    // Right: changed files
    let file_block = Block::default()
        .borders(Borders::NONE)
        .title(Span::styled(
            format!(" Changed Files ({}) ", files.len()),
            Style::default().fg(Color::Gray),
        ));
    let file_inner = file_block.inner(cols[1]);
    f.render_widget(file_block, cols[1]);

    if files.is_empty() {
        f.render_widget(
            Paragraph::new("No files changed yet")
                .style(Style::default().fg(Color::DarkGray))
                .alignment(Alignment::Center),
            file_inner,
        );
    } else {
        let items: Vec<ListItem> = files
            .iter()
            .map(|fname| {
                ListItem::new(Span::styled(
                    fname.clone(),
                    Style::default().fg(Color::LightGreen),
                ))
            })
            .collect();
        f.render_widget(List::new(items), file_inner);
    }
}

fn render_escalation_detail(
    f: &mut Frame,
    esc: &Escalation,
    cursor: usize,
    chosen: Option<&ResolutionOption>,
    area: Rect,
) {
    let sev_color = match esc.severity.as_str() {
        "critical" => Color::Red,
        "high" => Color::LightRed,
        _ => Color::Yellow,
    };
    let title = format!(
        " Escalation: {} [{}] ",
        &esc.id.to_string()[..8],
        esc.severity.as_str()
    );
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(sev_color))
        .title(Span::styled(
            title,
            Style::default().fg(sev_color).add_modifier(Modifier::BOLD),
        ));
    let inner = block.inner(area);
    f.render_widget(block, area);

    let cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(55), Constraint::Percentage(45)])
        .split(inner);

    // Left: summary + agents + intents + entities
    let mut detail_lines = vec![
        Line::from(vec![
            Span::styled("Type:    ", Style::default().fg(Color::Gray)),
            Span::raw(esc.escalation_type.as_str()),
        ]),
        Line::from(vec![
            Span::styled("Summary: ", Style::default().fg(Color::Gray)),
            Span::raw(esc.summary.clone()),
        ]),
        Line::from(Span::raw("")),
    ];

    if !esc.agents.is_empty() {
        detail_lines.push(Line::from(Span::styled(
            "Agents:",
            Style::default().fg(Color::Gray),
        )));
        for agent in &esc.agents {
            detail_lines.push(Line::from(format!("  • {agent}")));
        }
        detail_lines.push(Line::from(Span::raw("")));
    }

    if !esc.intents.is_empty() {
        detail_lines.push(Line::from(Span::styled(
            "Intents:",
            Style::default().fg(Color::Gray),
        )));
        for intent in &esc.intents {
            detail_lines.push(Line::from(format!("  • {intent}")));
        }
        detail_lines.push(Line::from(Span::raw("")));
    }

    if !esc.affected_entities.is_empty() {
        detail_lines.push(Line::from(Span::styled(
            "Affected entities:",
            Style::default().fg(Color::Gray),
        )));
        for entity in &esc.affected_entities {
            detail_lines.push(Line::from(format!("  • {entity}")));
        }
    }

    let detail_block = Block::default()
        .borders(Borders::RIGHT)
        .border_style(Style::default().fg(Color::DarkGray))
        .title(Span::styled(" Details ", Style::default().fg(Color::Gray)));
    let detail_inner = detail_block.inner(cols[0]);
    f.render_widget(detail_block, cols[0]);
    f.render_widget(Paragraph::new(detail_lines), detail_inner);

    // Right: resolution options
    let res_title = if chosen.is_some() {
        " Resolution (applied) "
    } else {
        " Resolution Options "
    };
    let res_block = Block::default()
        .borders(Borders::NONE)
        .title(Span::styled(res_title, Style::default().fg(Color::Gray)));
    let res_inner = res_block.inner(cols[1]);
    f.render_widget(res_block, cols[1]);

    if let Some(resolved) = chosen {
        let lines = vec![
            Line::from(Span::styled(
                "✓ Resolved",
                Style::default().fg(Color::Green).add_modifier(Modifier::BOLD),
            )),
            Line::from(Span::raw("")),
            Line::from(Span::raw(resolved.label())),
        ];
        f.render_widget(Paragraph::new(lines), res_inner);
    } else {
        let items: Vec<ListItem> = esc
            .resolution_options
            .iter()
            .enumerate()
            .map(|(i, opt)| {
                let (prefix, style) = if i == cursor {
                    (
                        "▶ ",
                        Style::default()
                            .fg(Color::Cyan)
                            .add_modifier(Modifier::BOLD)
                            .bg(Color::DarkGray),
                    )
                } else {
                    ("  ", Style::default())
                };
                ListItem::new(Span::styled(
                    format!("{prefix}{}", opt.label()),
                    style,
                ))
            })
            .collect();
        f.render_widget(List::new(items), res_inner);
    }
}

fn render_issue_detail(
    f: &mut Frame,
    issue: &Issue,
    linked: &[Uuid],
    scroll: u16,
    area: Rect,
) {
    let priority_color = match issue.priority.as_str() {
        "critical" => Color::Red,
        "high" => Color::LightRed,
        "medium" => Color::Yellow,
        _ => Color::Gray,
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Cyan))
        .title(Span::styled(
            format!(" Issue: {} ", truncate(&issue.title, 50)),
            Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD),
        ));
    let inner = block.inner(area);
    f.render_widget(block, area);

    let cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(65), Constraint::Percentage(35)])
        .split(inner);

    // Left: description
    let desc_block = Block::default()
        .borders(Borders::RIGHT)
        .border_style(Style::default().fg(Color::DarkGray))
        .title(Span::styled(" Description ", Style::default().fg(Color::Gray)));
    let desc_inner = desc_block.inner(cols[0]);
    f.render_widget(desc_block, cols[0]);

    // Wrap description into lines for scrollable display.
    let width = desc_inner.width as usize;
    let mut desc_lines: Vec<Line> = Vec::new();
    for raw_line in issue.description.lines() {
        if raw_line.is_empty() {
            desc_lines.push(Line::from(""));
            continue;
        }
        // Simple word-wrap
        let mut cur = String::new();
        for word in raw_line.split_whitespace() {
            if cur.is_empty() {
                cur = word.to_string();
            } else if cur.len() + 1 + word.len() <= width {
                cur.push(' ');
                cur.push_str(word);
            } else {
                desc_lines.push(Line::from(cur.clone()));
                cur = word.to_string();
            }
        }
        if !cur.is_empty() {
            desc_lines.push(Line::from(cur));
        }
    }

    f.render_widget(
        Paragraph::new(desc_lines).scroll((scroll, 0)),
        desc_inner,
    );

    // Right: metadata + linked workspaces
    let meta_block = Block::default()
        .borders(Borders::NONE)
        .title(Span::styled(" Metadata ", Style::default().fg(Color::Gray)));
    let meta_inner = meta_block.inner(cols[1]);
    f.render_widget(meta_block, cols[1]);

    let mut meta_lines = vec![
        Line::from(vec![
            Span::styled("Status:   ", Style::default().fg(Color::Gray)),
            Span::styled(issue.status.as_str(), Style::default().fg(Color::Cyan)),
        ]),
        Line::from(vec![
            Span::styled("Priority: ", Style::default().fg(Color::Gray)),
            Span::styled(
                issue.priority.as_str(),
                Style::default().fg(priority_color),
            ),
        ]),
        Line::from(vec![
            Span::styled("Creator:  ", Style::default().fg(Color::Gray)),
            Span::raw(issue.creator.clone()),
        ]),
        Line::from(vec![
            Span::styled("Created:  ", Style::default().fg(Color::Gray)),
            Span::raw(issue.created_at.format("%Y-%m-%d").to_string()),
        ]),
    ];

    if !issue.labels.is_empty() {
        meta_lines.push(Line::from(vec![
            Span::styled("Labels:   ", Style::default().fg(Color::Gray)),
            Span::raw(issue.labels.join(", ")),
        ]));
    }

    if let Some(res) = &issue.resolution {
        meta_lines.push(Line::from(vec![
            Span::styled("Resolved: ", Style::default().fg(Color::Gray)),
            Span::raw(res.clone()),
        ]));
    }

    if !linked.is_empty() {
        meta_lines.push(Line::from(""));
        meta_lines.push(Line::from(Span::styled(
            "Workspaces:",
            Style::default().fg(Color::Gray),
        )));
        for wid in linked {
            meta_lines.push(Line::from(format!("  {}", &wid.to_string()[..8])));
        }
    }

    f.render_widget(Paragraph::new(meta_lines), meta_inner);
}

fn render_drilldown_bar(f: &mut Frame, content: &DrillDownContent, area: Rect) {
    let hint = match content {
        DrillDownContent::Workspace { .. } => "[Esc] back  [j/k] scroll files",
        DrillDownContent::Escalation { chosen: None, .. } => {
            "[j/k] select resolution  [Enter] confirm  [Esc] back"
        }
        DrillDownContent::Escalation { .. } => "[Enter/Esc] back",
        DrillDownContent::Issue { .. } => "[j/k] scroll  [Esc] back",
    };

    let bar = Paragraph::new(hint)
        .style(Style::default().fg(Color::Gray))
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::DarkGray)),
        )
        .alignment(Alignment::Center);
    f.render_widget(bar, area);
}

// ── Helpers ───────────────────────────────────────────────────────────────────

fn status_color(status: &str) -> Color {
    match status {
        "Active" | "active" => Color::Green,
        "Submitted" | "submitted" => Color::Cyan,
        "Merged" | "merged" => Color::Blue,
        "Discarded" | "discarded" => Color::DarkGray,
        _ => Color::White,
    }
}

fn truncate(s: &str, max: usize) -> String {
    if max < 3 {
        return s.chars().take(max).collect();
    }
    if s.len() <= max {
        s.to_string()
    } else {
        format!("{}…", &s[..max - 1])
    }
}

/// Walk the overlay directory for a workspace and return relative file paths.
fn overlay_files(vai_dir: &Path, workspace_id: &str) -> Vec<String> {
    let overlay = workspace::overlay_dir(vai_dir, workspace_id);
    if !overlay.exists() {
        return Vec::new();
    }
    collect_files_recursive(&overlay, &overlay)
}

fn collect_files_recursive(base: &Path, dir: &Path) -> Vec<String> {
    let mut result = Vec::new();
    let Ok(entries) = std::fs::read_dir(dir) else {
        return result;
    };
    let mut entries: Vec<_> = entries.flatten().collect();
    entries.sort_by_key(|e| e.file_name());
    for entry in entries {
        let path = entry.path();
        if path.is_dir() {
            result.extend(collect_files_recursive(base, &path));
        } else if let Ok(rel) = path.strip_prefix(base) {
            result.push(rel.to_string_lossy().into_owned());
        }
    }
    result
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_panel_cycling() {
        let p = Panel::ActiveWork;
        assert_eq!(p.next(), Panel::Conflicts);
        assert_eq!(p.prev(), Panel::SystemHealth);
        assert_eq!(Panel::SystemHealth.next(), Panel::ActiveWork);
    }

    #[test]
    fn test_truncate() {
        assert_eq!(truncate("hello world", 5), "hell…");
        assert_eq!(truncate("hi", 5), "hi");
        assert_eq!(truncate("exact", 5), "exact");
        assert_eq!(truncate("ab", 2), "ab");
    }

    #[test]
    fn test_run_returns_error_for_missing_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let missing = tmp.path().join("nonexistent");
        let result = run(&missing);
        assert!(result.is_err());
    }

    #[test]
    fn test_filter_workspaces() {
        use chrono::Utc;
        use uuid::Uuid;

        let tmp = tempfile::tempdir().unwrap();
        let vai_dir = tmp.path().join(".vai");
        std::fs::create_dir_all(&vai_dir).unwrap();

        let mut app = App::new(vai_dir);
        let now = Utc::now();
        app.data.workspaces = vec![
            workspace::WorkspaceMeta {
                id: Uuid::new_v4(),
                intent: "add rate limiting to auth".to_string(),
                base_version: "v1".to_string(),
                status: workspace::WorkspaceStatus::Active,
                created_at: now,
                updated_at: now,
                issue_id: None,
            },
            workspace::WorkspaceMeta {
                id: Uuid::new_v4(),
                intent: "fix database connection pool".to_string(),
                base_version: "v1".to_string(),
                status: workspace::WorkspaceStatus::Active,
                created_at: now,
                updated_at: now,
                issue_id: None,
            },
        ];

        // No filter — both visible
        assert_eq!(app.filtered_workspaces().len(), 2);

        // Filter by "auth"
        app.filter = "auth".to_string();
        assert_eq!(app.filtered_workspaces().len(), 1);
        assert!(app
            .filtered_workspaces()[0]
            .intent
            .contains("rate limiting"));

        // Filter by "database"
        app.filter = "database".to_string();
        assert_eq!(app.filtered_workspaces().len(), 1);

        // Filter with no match
        app.filter = "xyz123".to_string();
        assert_eq!(app.filtered_workspaces().len(), 0);
    }

    #[test]
    fn test_overlay_files_empty_for_missing_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let files = overlay_files(tmp.path(), "nonexistent-id");
        assert!(files.is_empty());
    }

    #[test]
    fn test_overlay_files_lists_changed_files() {
        let tmp = tempfile::tempdir().unwrap();
        let vai_dir = tmp.path();
        let ws_id = Uuid::new_v4().to_string();
        let overlay = workspace::overlay_dir(vai_dir, &ws_id);
        std::fs::create_dir_all(&overlay).unwrap();
        std::fs::write(overlay.join("foo.rs"), "fn main() {}").unwrap();
        let sub = overlay.join("src");
        std::fs::create_dir_all(&sub).unwrap();
        std::fs::write(sub.join("lib.rs"), "").unwrap();

        let files = overlay_files(vai_dir, &ws_id);
        assert_eq!(files.len(), 2);
        assert!(files.iter().any(|f| f == "foo.rs"));
        assert!(files.iter().any(|f| f == "src/lib.rs"));
    }

    #[test]
    fn test_enter_drilldown_no_item_does_nothing() {
        let tmp = tempfile::tempdir().unwrap();
        let vai_dir = tmp.path().join(".vai");
        std::fs::create_dir_all(&vai_dir).unwrap();

        let mut app = App::new(vai_dir);
        app.list_state.select(Some(0));
        // No workspaces loaded — drill-in should stay in Overview
        app.enter_drilldown();
        assert!(matches!(app.view, View::Overview));
    }
}
