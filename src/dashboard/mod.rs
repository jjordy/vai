//! TUI dashboard for human oversight of ai-agent vai activity.
//!
//! Displays real-time agent activity, workspace status, conflicts, issues,
//! and version history in an interactive terminal UI powered by ratatui.

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

use crate::escalation::{EscalationStatus, EscalationStore};
use crate::graph::GraphSnapshot;
use crate::issue::{IssueFilter, IssueStatus, IssueStore};
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

/// Application state for the TUI.
struct App {
    vai_dir: PathBuf,
    selected_panel: Panel,
    data: DashboardData,
    last_refresh: Instant,
    /// Scroll offset for the focused list panel.
    list_state: ListState,
    should_quit: bool,
}

impl App {
    fn new(vai_dir: PathBuf) -> Self {
        let mut app = App {
            vai_dir,
            selected_panel: Panel::ActiveWork,
            data: DashboardData::default(),
            last_refresh: Instant::now() - Duration::from_secs(60), // trigger immediate load
            list_state: ListState::default(),
            should_quit: false,
        };
        app.list_state.select(Some(0));
        app
    }

    /// Reload all data from disk.
    fn refresh(&mut self) {
        self.last_refresh = Instant::now();
        let vai_dir = &self.vai_dir;

        // Workspaces — all active
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

    fn handle_key(&mut self, key: KeyCode) {
        match key {
            KeyCode::Char('q') | KeyCode::Esc => self.should_quit = true,
            KeyCode::Tab => {
                self.selected_panel = self.selected_panel.next();
                self.list_state.select(Some(0));
            }
            KeyCode::BackTab => {
                self.selected_panel = self.selected_panel.prev();
                self.list_state.select(Some(0));
            }
            KeyCode::Down | KeyCode::Char('j') => self.scroll_down(),
            KeyCode::Up | KeyCode::Char('k') => self.scroll_up(),
            KeyCode::Char('r') => self.refresh(),
            _ => {}
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
            Panel::ActiveWork => self.data.workspaces.len(),
            Panel::Conflicts => self.data.pending_escalations.len(),
            Panel::Issues => 0, // summary only
            Panel::RecentVersions => self.data.recent_versions.len(),
            Panel::SystemHealth => 0,
        }
    }
}

/// Launch the TUI dashboard in local mode, polling `.vai/` for changes.
pub fn run(vai_dir: &Path) -> Result<(), DashboardError> {
    if !vai_dir.exists() {
        return Err(DashboardError::NotInitialised);
    }

    // Set up terminal
    enable_raw_mode()?;
    let mut stdout = std::io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
    let backend = ratatui::backend::CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let mut app = App::new(vai_dir.to_path_buf());
    let refresh_interval = Duration::from_secs(2);
    let tick_rate = Duration::from_millis(100);

    let result = run_loop(&mut terminal, &mut app, refresh_interval, tick_rate);

    // Restore terminal
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
        // Refresh data if interval elapsed
        if app.last_refresh.elapsed() >= refresh_interval {
            app.refresh();
        }

        terminal.draw(|f| render(f, app))?;

        // Poll for input
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

/// Render the full dashboard.
fn render(f: &mut Frame, app: &App) {
    let size = f.area();

    // Overall layout: top panels | bottom status bar
    let outer = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(3), Constraint::Length(3)])
        .split(size);

    let main_area = outer[0];
    let status_bar_area = outer[1];

    // Main area: top row (Active Work + Conflicts) | bottom row (Issues + Recent Versions)
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
        .title(Span::styled(
            format!(" {title} "),
            style,
        ))
}

fn render_active_work(f: &mut Frame, app: &App, area: Rect) {
    let selected = app.selected_panel == Panel::ActiveWork;
    let block = panel_block("Active Work", selected);
    let inner = block.inner(area);
    f.render_widget(block, area);

    if app.data.workspaces.is_empty() {
        let msg = Paragraph::new("No active workspaces")
            .style(Style::default().fg(Color::DarkGray))
            .alignment(Alignment::Center);
        f.render_widget(msg, inner);
        return;
    }

    // Table: ID | Intent | Status
    let header = Row::new(vec![
        Cell::from("ID").style(Style::default().add_modifier(Modifier::BOLD)),
        Cell::from("Intent").style(Style::default().add_modifier(Modifier::BOLD)),
        Cell::from("Status").style(Style::default().add_modifier(Modifier::BOLD)),
    ])
    .style(Style::default().fg(Color::Yellow));

    let rows: Vec<Row> = app
        .data
        .workspaces
        .iter()
        .enumerate()
        .map(|(i, ws)| {
            let id_str = ws.id.to_string();
            let id_short = &id_str[..id_str.len().min(8)];
            let intent = truncate(&ws.intent, inner.width.saturating_sub(20) as usize);
            let status = ws.status.as_str();

            let style = if selected
                && app.list_state.selected() == Some(i)
            {
                Style::default().bg(Color::DarkGray)
            } else {
                Style::default()
            };

            Row::new(vec![
                Cell::from(id_short.to_string()),
                Cell::from(intent),
                Cell::from(status).style(Style::default().fg(status_color(status))),
            ])
            .style(style)
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
            let style = if selected && app.list_state.selected() == Some(i) {
                Style::default().bg(Color::DarkGray)
            } else {
                Style::default()
            };
            ListItem::new(Span::styled(label, Style::default().fg(sev_color))).style(style)
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
            let style = if selected && app.list_state.selected() == Some(i) {
                Style::default().bg(Color::DarkGray)
            } else {
                Style::default()
            };
            ListItem::new(Span::raw(label)).style(style)
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
                " Health: {entities} entities | {relationships} rels | {ws_count} ws | refreshed {refresh_str} | [Tab] nav  [r] refresh  [q] quit "
            ),
            Style::default().fg(Color::Gray),
        )))
        .select(app.selected_panel as usize)
        .style(Style::default())
        .highlight_style(Style::default().fg(Color::Cyan));

    f.render_widget(tabs, area);
}

fn status_color(status: &str) -> Color {
    match status {
        "active" => Color::Green,
        "submitted" => Color::Cyan,
        "merged" => Color::Blue,
        "discarded" => Color::DarkGray,
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
}
