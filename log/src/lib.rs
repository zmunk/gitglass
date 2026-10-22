//! Interactive Git log viewer with branch filtering.
//!
//! This crate provides the log-TUI half of `gitglass`.  Use [`run`] to
//! start the event loop; it manages its own terminal lifecycle.

use std::{
    collections::BTreeSet,
    error::Error,
    io,
    process::Command,
    time::{Duration, Instant},
};

use ansi_to_tui::IntoText;
use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::{
    layout::{Constraint, Layout},
    style::{Color, Modifier, Style},
    text::Line,
    widgets::{Block, Borders, List, ListItem, ListState, Paragraph},
    DefaultTerminal, Frame,
};

const REFRESH_INTERVAL: Duration = Duration::from_secs(2);

struct BranchPane {
    entries: Vec<String>,
    state: ListState,
    error: Option<String>,
    filter: String,
    editing_filter: bool,
    visible: Vec<usize>,
}

impl BranchPane {
    fn new(result: io::Result<Vec<String>>) -> Self {
        let (entries, error) = match result {
            Ok(entries) => (entries, None),
            Err(error) => (Vec::new(), Some(error.to_string())),
        };
        let selected = (!entries.is_empty()).then_some(0);
        let visible = (0..entries.len()).collect();
        Self {
            entries,
            state: ListState::default().with_selected(selected),
            error,
            filter: String::new(),
            editing_filter: false,
            visible,
        }
    }

    fn refresh(&mut self, result: io::Result<Vec<String>>) {
        let entries = match result {
            Ok(entries) => entries,
            Err(error) => {
                self.error = Some(error.to_string());
                return;
            }
        };
        let selected = self.state.selected();
        let reference = selected
            .and_then(|index| self.visible.get(index))
            .map(|index| branch_ref(&self.entries[*index]).to_owned());
        let offset = self.state.offset();
        self.entries = entries;
        self.error = None;
        self.update_filter();
        let selected = reference
            .and_then(|reference| {
                self.visible
                    .iter()
                    .position(|index| branch_ref(&self.entries[*index]) == reference)
            })
            .or_else(|| {
                (!self.visible.is_empty())
                    .then(|| selected.unwrap_or(0).min(self.visible.len() - 1))
            });
        self.state = ListState::default()
            .with_selected(selected)
            .with_offset(offset.min(self.visible.len().saturating_sub(1)));
    }

    fn update_filter(&mut self) {
        let query = self.filter.to_lowercase();
        self.visible = self
            .entries
            .iter()
            .enumerate()
            .filter(|(_, entry)| branch_ref(entry).to_lowercase().contains(&query))
            .map(|(index, _)| index)
            .collect();
        self.state = ListState::default().with_selected((!self.visible.is_empty()).then_some(0));
    }

    fn clear_filter(&mut self) {
        self.filter.clear();
        self.editing_filter = false;
        self.update_filter();
    }

    fn edit_filter(&mut self, key: KeyEvent) {
        match (key.code, key.modifiers) {
            (KeyCode::Enter, _) => self.editing_filter = false,
            (KeyCode::Esc, _) => self.clear_filter(),
            (KeyCode::Backspace, _) => {
                self.filter.pop();
                self.update_filter();
            }
            (KeyCode::Char(character), KeyModifiers::NONE | KeyModifiers::SHIFT) => {
                self.filter.push(character);
                self.update_filter();
            }
            _ => {}
        }
    }

    fn navigate(&mut self, key: KeyEvent, height: usize) {
        let Some(selected) = self.state.selected() else {
            return;
        };
        let half_page = (height / 2).max(1);
        let next = match (key.code, key.modifiers) {
            (KeyCode::Char('j') | KeyCode::Down, KeyModifiers::NONE) => selected.saturating_add(1),
            (KeyCode::Char('k') | KeyCode::Up, KeyModifiers::NONE) => selected.saturating_sub(1),
            (KeyCode::Char('d'), KeyModifiers::CONTROL) => selected.saturating_add(half_page),
            (KeyCode::Char('u'), KeyModifiers::CONTROL) => selected.saturating_sub(half_page),
            _ => return,
        };
        self.state.select(Some(next.min(self.visible.len() - 1)));
    }
}

struct App {
    lines: Vec<Line<'static>>,
    selected: usize,
    offset: usize,
    page_height: usize,
    branches: Option<BranchPane>,
    included_branches: BTreeSet<String>,
    log_error: Option<String>,
}

impl App {
    fn new(lines: Vec<Line<'static>>) -> Self {
        Self {
            lines,
            selected: 0,
            offset: 0,
            page_height: 0,
            branches: None,
            included_branches: BTreeSet::new(),
            log_error: None,
        }
    }

    fn resize(&mut self, height: usize) {
        self.page_height = height;
        self.keep_visible();
    }

    fn keep_visible(&mut self) {
        if self.page_height == 0 {
            return;
        }
        if self.selected < self.offset {
            self.offset = self.selected;
        } else if self.selected >= self.offset.saturating_add(self.page_height) {
            self.offset = self.selected + 1 - self.page_height;
        }
        self.offset = self
            .offset
            .min(self.lines.len().saturating_sub(self.page_height));
    }

    fn move_down(&mut self, count: usize) {
        self.selected = self
            .selected
            .saturating_add(count)
            .min(self.lines.len().saturating_sub(1));
        self.keep_visible();
    }

    fn move_up(&mut self, count: usize) {
        self.selected = self.selected.saturating_sub(count);
        self.keep_visible();
    }

    // Returns true when the user requests exit.
    fn handle_key(&mut self, key: KeyEvent) -> bool {
        if key.kind == KeyEventKind::Release {
            return false;
        }
        if let Some(pane) = &mut self.branches {
            if key.code == KeyCode::Char('c') && key.modifiers == KeyModifiers::CONTROL {
                return true;
            }
            if pane.editing_filter {
                pane.edit_filter(key);
                return false;
            }
            if key.code == KeyCode::Esc && !pane.filter.is_empty() {
                pane.clear_filter();
                return false;
            }
            if key.code == KeyCode::Char('/') && key.modifiers == KeyModifiers::NONE {
                pane.editing_filter = true;
                return false;
            }
        }
        if self.branches.is_some() && key.modifiers == KeyModifiers::NONE {
            match key.code {
                KeyCode::Char(' ') => {
                    self.toggle_branch();
                    return false;
                }
                KeyCode::Char('a') => {
                    self.toggle_all_branches();
                    return false;
                }
                _ => {}
            }
        }
        if let Some(branches) = &mut self.branches {
            match (key.code, key.modifiers) {
                (KeyCode::Esc | KeyCode::Char('b'), KeyModifiers::NONE) => self.branches = None,
                (KeyCode::Char('q'), KeyModifiers::NONE)
                | (KeyCode::Char('c'), KeyModifiers::CONTROL) => return true,
                _ => branches.navigate(key, self.page_height.saturating_sub(2)),
            }
            return false;
        }
        let half_page = (self.page_height / 2).max(1);
        match (key.code, key.modifiers) {
            (KeyCode::Char('q'), KeyModifiers::NONE)
            | (KeyCode::Esc, KeyModifiers::NONE)
            | (KeyCode::Char('c'), KeyModifiers::CONTROL) => return true,
            (KeyCode::Char('b'), KeyModifiers::NONE) => {
                self.branches = Some(BranchPane::new(load_branches()));
            }
            (KeyCode::Char('j') | KeyCode::Down, KeyModifiers::NONE) => self.move_down(1),
            (KeyCode::Char('k') | KeyCode::Up, KeyModifiers::NONE) => self.move_up(1),
            (KeyCode::Char('d'), KeyModifiers::CONTROL) => self.move_down(half_page),
            (KeyCode::Char('u'), KeyModifiers::CONTROL) => self.move_up(half_page),
            _ => {}
        }
        false
    }

    fn toggle_branch(&mut self) {
        let Some(entry) = self.branches.as_ref().and_then(|pane| {
            pane.state
                .selected()
                .and_then(|index| pane.visible.get(index))
                .and_then(|index| pane.entries.get(*index))
        }) else {
            return;
        };
        let candidate = toggled_branches(&self.included_branches, branch_ref(entry));
        self.apply_branches(candidate);
    }

    fn toggle_all_branches(&mut self) {
        let Some(pane) = &self.branches else {
            return;
        };
        if pane.error.is_some() || pane.entries.is_empty() {
            return;
        }
        let candidate = toggled_all_branches(&self.included_branches, &pane.entries);
        self.apply_branches(candidate);
    }

    fn apply_branches(&mut self, candidate: BTreeSet<String>) {
        match load_log(&candidate) {
            Ok(lines) => {
                self.lines = lines;
                self.included_branches = candidate;
                self.selected = 0;
                self.offset = 0;
                self.log_error = None;
            }
            Err(error) => self.log_error = Some(error.to_string()),
        }
    }

    fn draw(&mut self, frame: &mut Frame) {
        let [body, footer] =
            Layout::vertical([Constraint::Min(0), Constraint::Length(1)]).areas(frame.area());
        self.resize(usize::from(body.height));
        let (body, branch_area) = if self.branches.is_some() {
            let [log, branches] =
                Layout::horizontal([Constraint::Percentage(65), Constraint::Percentage(35)])
                    .areas(body);
            (log, Some(branches))
        } else {
            (body, None)
        };

        if self.lines.is_empty() {
            let message = if self.included_branches.is_empty() {
                if self.branches.is_some() {
                    "No branches selected — use Space or a to select branches."
                } else {
                    "No branches selected — press b to select branches."
                }
            } else {
                "No log entries."
            };
            frame.render_widget(Paragraph::new(message), body);
        } else {
            let items = self.lines.iter().cloned().map(ListItem::new);
            let list =
                List::new(items).highlight_style(Style::default().add_modifier(Modifier::BOLD));
            let mut state = ListState::default()
                .with_offset(self.offset)
                .with_selected(self.branches.is_none().then_some(self.selected));
            frame.render_stateful_widget(list, body, &mut state);
        }

        if let (Some(branches), Some(area)) = (&mut self.branches, branch_area) {
            let title = if branches.editing_filter || !branches.filter.is_empty() {
                format!("Branches  Filter: {}", branches.filter)
            } else {
                "Branches".to_owned()
            };
            let block = Block::default().borders(Borders::ALL).title(title);
            if let Some(error) = &branches.error {
                frame.render_widget(Paragraph::new(error.as_str()).block(block), area);
            } else if branches.entries.is_empty() {
                frame.render_widget(Paragraph::new("No branches.").block(block), area);
            } else if branches.visible.is_empty() {
                frame.render_widget(Paragraph::new("No matching branches.").block(block), area);
            } else {
                let items = branches.visible.iter().map(|index| {
                    let entry = &branches.entries[*index];
                    let item = branch_item(entry, &self.included_branches);
                    if entry.starts_with("* ") {
                        item.style(Style::default().fg(Color::Green))
                    } else {
                        item
                    }
                });
                let highlight = if branches.editing_filter {
                    Style::default()
                } else {
                    Style::default().add_modifier(Modifier::BOLD)
                };
                let list = List::new(items).block(block).highlight_style(highlight);
                frame.render_stateful_widget(list, area, &mut branches.state);
            }
        }

        let hints = if self
            .branches
            .as_ref()
            .is_some_and(|pane| pane.editing_filter)
        {
            "Type to filter  Enter: focus list  Esc: clear filter"
        } else if self.branches.is_some() {
            "/: filter  Space: toggle  a: all/none  j/k: move  b: close  Esc: clear/close"
        } else {
            "j/k: move  Ctrl-d/u: half page  b: branches  q: quit"
        };
        frame.render_widget(
            Paragraph::new(self.log_error.as_deref().unwrap_or(hints)).style(Style::default().fg(
                if self.log_error.is_some() {
                    Color::Red
                } else {
                    Color::DarkGray
                },
            )),
            footer,
        );
    }

    fn refresh_log(&mut self, result: Result<Vec<Line<'static>>, Box<dyn Error>>) {
        match result {
            Ok(lines) => {
                self.lines = lines;
                self.selected = self.selected.min(self.lines.len().saturating_sub(1));
                self.keep_visible();
                self.log_error = None;
            }
            Err(error) => self.log_error = Some(error.to_string()),
        }
    }

    fn run(&mut self, terminal: &mut DefaultTerminal) -> io::Result<()> {
        let mut next_refresh = Instant::now() + REFRESH_INTERVAL;
        loop {
            terminal.draw(|frame| self.draw(frame))?;
            if event::poll(next_refresh.saturating_duration_since(Instant::now()))? {
                if let Event::Key(key) = event::read()? {
                    if self.handle_key(key) {
                        return Ok(());
                    }
                }
            }
            if Instant::now() >= next_refresh {
                self.refresh_log(load_log(&self.included_branches));
                if let Some(pane) = &mut self.branches {
                    pane.refresh(load_branches());
                }
                next_refresh = Instant::now() + REFRESH_INTERVAL;
            }
        }
    }
}

fn branch_ref(entry: &str) -> &str {
    entry.get(2..).unwrap_or(entry)
}

fn toggled_branches(included: &BTreeSet<String>, branch: &str) -> BTreeSet<String> {
    let mut result = included.clone();
    if !result.remove(branch) {
        result.insert(branch.to_owned());
    }
    result
}

fn toggled_all_branches(included: &BTreeSet<String>, entries: &[String]) -> BTreeSet<String> {
    let all: BTreeSet<String> = entries
        .iter()
        .map(|entry| branch_ref(entry).to_owned())
        .collect();
    if all.is_subset(included) {
        BTreeSet::new()
    } else {
        all
    }
}

fn branch_item(entry: &str, included: &BTreeSet<String>) -> ListItem<'static> {
    let reference = branch_ref(entry);
    let name = reference
        .strip_prefix("refs/heads/")
        .or_else(|| reference.strip_prefix("refs/remotes/"))
        .unwrap_or(reference);
    let marker = if included.contains(reference) {
        "[x]"
    } else {
        "[ ]"
    };
    let head = if entry.starts_with("* ") { "*" } else { " " };
    ListItem::new(format!("{marker} {head} {name}"))
}

fn initial_branches() -> io::Result<BTreeSet<String>> {
    let output = Command::new("git")
        .args(["symbolic-ref", "--quiet", "HEAD"])
        .output()?;
    if output.status.success() {
        let reference = String::from_utf8_lossy(&output.stdout).trim().to_owned();
        Ok(BTreeSet::from([reference]))
    } else if output.status.code() == Some(1) {
        // Detached HEAD has no branch to mark as included.
        Ok(BTreeSet::new())
    } else {
        Err(io::Error::other(format!(
            "git symbolic-ref failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        )))
    }
}

fn load_branches() -> io::Result<Vec<String>> {
    let output = Command::new("git")
        .args([
            "--no-pager",
            "branch",
            "--all",
            "--color=never",
            "--sort=-committerdate",
            "--format=%(HEAD) %(refname)",
        ])
        .output()?;
    if !output.status.success() {
        return Err(io::Error::other(format!(
            "git branch failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }
    Ok(String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(str::to_owned)
        .collect())
}

fn load_log(branches: &BTreeSet<String>) -> Result<Vec<Line<'static>>, Box<dyn Error>> {
    if branches.is_empty() {
        return Ok(Vec::new());
    }
    // Force color even though stdout is piped, then translate ANSI styles into
    // Ratatui spans while preserving graph connector lines.
    let mut command = Command::new("git");
    command.args([
        "--no-pager",
        "log",
        "--oneline",
        "--graph",
        "--decorate",
        "--color=always",
        "--end-of-options",
    ]);
    command.args(branches);
    let output = command.arg("--").output()?;
    if !output.status.success() {
        return Err(io::Error::other(format!(
            "git log failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ))
        .into());
    }
    Ok(output.stdout.into_text()?.lines)
}

/// Run the interactive Git log TUI.
///
/// Fetches Git data before entering raw mode, so Git errors leave the
/// terminal untouched.  Manages its own terminal lifecycle.
pub fn run() -> Result<(), Box<dyn Error>> {
    let included_branches = initial_branches()?;
    let mut app = App::new(load_log(&included_branches)?);
    app.included_branches = included_branches;
    let mut terminal = ratatui::init();
    let result = app.run(&mut terminal);
    ratatui::restore();
    result?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn auto_refresh_preserves_navigation_and_clamps_after_removal() {
        let mut app = app(10, 3);
        app.move_down(7);
        let offset = app.offset;
        app.refresh_log(Ok((0..12).map(|i| Line::from(format!("* {i}"))).collect()));
        assert_eq!(app.selected, 7);
        assert_eq!(app.offset, offset);
        app.refresh_log(Ok(vec![Line::from("only entry")]));
        assert_eq!(app.selected, 0);
        assert_eq!(app.offset, 0);
        app.refresh_log(Err(io::Error::other("refresh failed").into()));
        assert_eq!(app.lines.len(), 1);
        assert_eq!(app.log_error.as_deref(), Some("refresh failed"));
        app.refresh_log(Ok(Vec::new()));
        assert_eq!(app.selected, 0);
        assert_eq!(app.offset, 0);
        assert!(app.log_error.is_none());
    }

    #[test]
    fn branch_refresh_preserves_filter_and_focused_reference() {
        let mut pane = BranchPane::new(Ok(vec![
            "* refs/heads/main".into(),
            "  refs/heads/feature".into(),
        ]));
        pane.filter = "heads".into();
        pane.editing_filter = true;
        pane.update_filter();
        pane.state.select(Some(1));
        pane.refresh(Ok(vec![
            "* refs/heads/feature".into(),
            "  refs/heads/main".into(),
        ]));
        assert_eq!(pane.state.selected(), Some(0));
        assert_eq!(pane.filter, "heads");
        assert!(pane.editing_filter);
        pane.refresh(Err(io::Error::other("refresh failed")));
        assert_eq!(pane.entries.len(), 2);
        assert!(pane.error.is_some());
        pane.refresh(Ok(Vec::new()));
        assert_eq!(pane.state.selected(), None);
        assert!(pane.error.is_none());
    }

    fn app(count: usize, height: usize) -> App {
        let mut app = App::new((0..count).map(|i| Line::from(format!("* {i}"))).collect());
        app.resize(height);
        app
    }

    fn press(app: &mut App, character: char, modifiers: KeyModifiers) -> bool {
        app.handle_key(KeyEvent::new(KeyCode::Char(character), modifiers))
    }

    #[test]
    fn moves_one_line_and_stops_at_boundaries() {
        let mut app = app(3, 2);
        press(&mut app, 'k', KeyModifiers::NONE);
        assert_eq!(app.selected, 0);
        press(&mut app, 'j', KeyModifiers::NONE);
        assert_eq!(app.selected, 1);
        app.move_down(usize::MAX);
        assert_eq!((app.selected, app.offset), (2, 1));
        app.move_up(usize::MAX);
        assert_eq!((app.selected, app.offset), (0, 0));
    }

    #[test]
    fn half_page_uses_visible_log_height() {
        let mut app = app(30, 10);
        press(&mut app, 'd', KeyModifiers::CONTROL);
        assert_eq!(app.selected, 5);
        press(&mut app, 'd', KeyModifiers::CONTROL);
        assert_eq!((app.selected, app.offset), (10, 1));
        press(&mut app, 'u', KeyModifiers::CONTROL);
        assert_eq!(app.selected, 5);
        press(&mut app, 'd', KeyModifiers::NONE);
        assert_eq!(app.selected, 5);
    }

    #[test]
    fn empty_and_tiny_viewports_are_safe() {
        let mut empty = app(0, 0);
        press(&mut empty, 'd', KeyModifiers::CONTROL);
        press(&mut empty, 'u', KeyModifiers::CONTROL);
        assert_eq!(empty.selected, 0);
        let mut tiny = app(3, 1);
        press(&mut tiny, 'd', KeyModifiers::CONTROL);
        assert_eq!((tiny.selected, tiny.offset), (1, 1));
    }

    #[test]
    fn resize_keeps_selection_visible() {
        let mut app = app(100, 20);
        app.move_down(50);
        app.resize(5);
        assert_eq!((app.selected, app.offset), (50, 46));
        app.resize(100);
        assert_eq!((app.selected, app.offset), (50, 0));
    }

    #[test]
    fn release_events_do_not_move_cursor() {
        let mut app = app(3, 2);
        let mut key = KeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE);
        key.kind = KeyEventKind::Release;
        app.handle_key(key);
        assert_eq!(app.selected, 0);
    }

    #[test]
    fn branch_navigation_has_focus_and_escape_restores_log() {
        let mut app = app(30, 12);
        app.move_down(3);
        app.branches = Some(BranchPane::new(Ok((0..20)
            .map(|i| format!("branch-{i}"))
            .collect())));
        press(&mut app, 'j', KeyModifiers::NONE);
        assert_eq!(app.branches.as_ref().unwrap().state.selected(), Some(1));
        press(&mut app, 'd', KeyModifiers::CONTROL);
        assert_eq!(app.branches.as_ref().unwrap().state.selected(), Some(6));
        assert_eq!(app.selected, 3);
        assert!(!app.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)));
        assert!(app.branches.is_none());
        press(&mut app, 'j', KeyModifiers::NONE);
        assert_eq!(app.selected, 4);
    }

    #[test]
    fn branch_navigation_clamps_and_handles_empty_or_failed_loads() {
        let mut pane = BranchPane::new(Ok(vec!["main".into(), "remotes/origin/main".into()]));
        let down = KeyEvent::new(KeyCode::Char('d'), KeyModifiers::CONTROL);
        let up = KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL);
        pane.navigate(down, 100);
        assert_eq!(pane.state.selected(), Some(1));
        pane.navigate(up, 100);
        assert_eq!(pane.state.selected(), Some(0));
        let mut empty = BranchPane::new(Ok(Vec::new()));
        empty.navigate(down, 0);
        assert_eq!(empty.state.selected(), None);
        let failed = BranchPane::new(Err(io::Error::other("failed to load")));
        assert_eq!(failed.error.as_deref(), Some("failed to load"));
    }

    #[test]
    fn branch_pane_renders_on_right_with_bold_focus() {
        use ratatui::{backend::TestBackend, Terminal};

        let mut terminal = Terminal::new(TestBackend::new(80, 8)).unwrap();
        let mut app = app(10, 7);
        app.branches = Some(BranchPane::new(Ok(vec!["  refs/heads/main".into()])));
        terminal.draw(|frame| app.draw(frame)).unwrap();
        let buffer = terminal.backend().buffer();
        assert!(!buffer[(0, 0)].modifier.contains(Modifier::BOLD));
        assert_eq!(buffer[(59, 1)].symbol(), "m");
        assert!(buffer[(53, 1)].modifier.contains(Modifier::BOLD));
        // Tiny windows must remain safe with the sidebar open.
        let mut tiny = Terminal::new(TestBackend::new(1, 1)).unwrap();
        tiny.draw(|frame| app.draw(frame)).unwrap();
    }

    #[test]
    fn current_branch_is_green_independently_of_cursor() {
        use ratatui::{backend::TestBackend, Terminal};

        let mut terminal = Terminal::new(TestBackend::new(80, 8)).unwrap();
        let mut app = app(10, 7);
        app.branches = Some(BranchPane::new(Ok(vec![
            "  feature".into(),
            "* main".into(),
        ])));
        terminal.draw(|frame| app.draw(frame)).unwrap();
        let buffer = terminal.backend().buffer();
        assert_eq!(buffer[(57, 2)].symbol(), "*");
        assert_eq!(buffer[(53, 2)].fg, Color::Green);
        assert!(!buffer[(53, 2)].modifier.contains(Modifier::BOLD));
        assert_ne!(buffer[(53, 1)].fg, Color::Green);
        assert!(buffer[(53, 1)].modifier.contains(Modifier::BOLD));

        press(&mut app, 'j', KeyModifiers::NONE);
        terminal.draw(|frame| app.draw(frame)).unwrap();
        let buffer = terminal.backend().buffer();
        assert_eq!(buffer[(53, 2)].fg, Color::Green);
        assert!(buffer[(53, 2)].modifier.contains(Modifier::BOLD));
        assert!(!buffer[(53, 1)].modifier.contains(Modifier::BOLD));
    }

    #[test]
    fn b_closes_branch_pane_and_restores_log_focus() {
        let mut app = app(10, 7);
        app.move_down(2);
        app.branches = Some(BranchPane::new(Ok(vec!["* main".into()])));
        assert!(!press(&mut app, 'b', KeyModifiers::NONE));
        assert!(app.branches.is_none());
        assert_eq!(app.selected, 2);
        press(&mut app, 'j', KeyModifiers::NONE);
        assert_eq!(app.selected, 3);
    }

    #[test]
    fn branch_toggles_preserve_other_included_branches() {
        let original = BTreeSet::new();
        let main = toggled_branches(&original, "refs/heads/main");
        let both = toggled_branches(&main, "refs/remotes/origin/feature");
        assert_eq!(both.len(), 2);
        let remote_only = toggled_branches(&both, "refs/heads/main");
        assert_eq!(remote_only.len(), 1);
        assert!(remote_only.contains("refs/remotes/origin/feature"));
        assert!(toggled_branches(&remote_only, "refs/remotes/origin/feature").is_empty());
        assert!(original.is_empty());
    }

    #[test]
    fn branch_inclusion_markers_render_without_full_ref_prefix() {
        use ratatui::{backend::TestBackend, Terminal};

        let included = BTreeSet::from(["refs/heads/main".to_owned()]);
        let mut terminal = Terminal::new(TestBackend::new(40, 2)).unwrap();
        terminal
            .draw(|frame| {
                let list = List::new(vec![
                    branch_item("* refs/heads/main", &included),
                    branch_item("  refs/remotes/origin/main", &included),
                ]);
                frame.render_widget(list, frame.area());
            })
            .unwrap();
        let buffer = terminal.backend().buffer();
        assert_eq!(buffer[(1, 0)].symbol(), "x");
        assert_eq!(buffer[(4, 0)].symbol(), "*");
        assert_eq!(buffer[(6, 0)].symbol(), "m");
        assert_eq!(buffer[(1, 1)].symbol(), " ");
        assert_eq!(buffer[(6, 1)].symbol(), "o");
    }

    #[test]
    fn space_without_branches_or_on_empty_pane_is_a_noop() {
        let mut app = app(5, 4);
        assert!(!press(&mut app, ' ', KeyModifiers::NONE));
        app.branches = Some(BranchPane::new(Ok(Vec::new())));
        assert!(!press(&mut app, ' ', KeyModifiers::NONE));
        assert!(app.included_branches.is_empty());
        assert!(app.log_error.is_none());
    }

    #[test]
    fn toggle_all_selects_every_branch_then_clears() {
        let entries = vec![
            "* refs/heads/main".into(),
            "  refs/remotes/origin/topic".into(),
        ];
        let partial = BTreeSet::from(["refs/heads/main".to_owned()]);
        let all = toggled_all_branches(&partial, &entries);
        assert_eq!(all.len(), 2);
        assert!(all.contains("refs/remotes/origin/topic"));
        assert_eq!(toggled_all_branches(&BTreeSet::new(), &entries), all);
        assert!(toggled_all_branches(&all, &entries).is_empty());
    }

    #[test]
    fn a_clears_all_and_empty_log_explains_how_to_select_branches() {
        use ratatui::{backend::TestBackend, Terminal};

        let mut app = app(5, 4);
        app.included_branches.insert("refs/heads/main".into());
        app.branches = Some(BranchPane::new(Ok(vec!["* refs/heads/main".into()])));
        app.move_down(3);
        assert!(!press(&mut app, 'a', KeyModifiers::NONE));
        assert!(app.included_branches.is_empty());
        assert!(app.lines.is_empty());
        assert_eq!((app.selected, app.offset), (0, 0));
        assert!(load_log(&BTreeSet::new()).unwrap().is_empty());
        press(&mut app, 'b', KeyModifiers::NONE);
        let mut terminal = Terminal::new(TestBackend::new(80, 5)).unwrap();
        terminal.draw(|frame| app.draw(frame)).unwrap();
        let buffer = terminal.backend().buffer();
        let row: String = (0..80).map(|x| buffer[(x, 0)].symbol()).collect();
        assert!(row.starts_with("No branches selected"));
        assert!(row.contains("press b"));
    }

    #[test]
    fn a_does_nothing_in_empty_or_failed_branch_pane() {
        let mut app = app(5, 4);
        app.branches = Some(BranchPane::new(Ok(Vec::new())));
        press(&mut app, 'a', KeyModifiers::NONE);
        assert_eq!(app.lines.len(), 5);
        app.branches = Some(BranchPane::new(Err(io::Error::other("unavailable"))));
        press(&mut app, 'a', KeyModifiers::NONE);
        assert_eq!(app.lines.len(), 5);
    }

    #[test]
    fn filter_edits_live_then_enter_focuses_list_and_escape_clears() {
        let mut app = app(5, 4);
        app.branches = Some(BranchPane::new(Ok(vec![
            "* refs/heads/main".into(),
            "  refs/heads/topic".into(),
            "  refs/remotes/origin/topic".into(),
        ])));
        press(&mut app, '/', KeyModifiers::NONE);
        for character in "TOPIC".chars() {
            press(&mut app, character, KeyModifiers::SHIFT);
        }
        let pane = app.branches.as_ref().unwrap();
        assert!(pane.editing_filter);
        assert_eq!(pane.visible, vec![1, 2]);
        assert_eq!(pane.state.selected(), Some(0));
        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        press(&mut app, 'j', KeyModifiers::NONE);
        let pane = app.branches.as_ref().unwrap();
        assert!(!pane.editing_filter);
        assert_eq!(pane.state.selected(), Some(1));
        assert_eq!(
            branch_ref(&pane.entries[pane.visible[1]]),
            "refs/remotes/origin/topic"
        );
        app.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        let pane = app.branches.as_ref().unwrap();
        assert!(pane.filter.is_empty());
        assert_eq!(pane.visible, vec![0, 1, 2]);
        app.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert!(app.branches.is_none());
    }

    #[test]
    fn filter_input_does_not_trigger_shortcuts_and_handles_no_matches() {
        let mut app = app(5, 4);
        app.branches = Some(BranchPane::new(Ok(vec!["* refs/heads/main".into()])));
        press(&mut app, '/', KeyModifiers::NONE);
        for character in "ba q ".chars() {
            assert!(!press(&mut app, character, KeyModifiers::NONE));
        }
        let pane = app.branches.as_ref().unwrap();
        assert_eq!(pane.filter, "ba q ");
        assert!(pane.visible.is_empty());
        assert_eq!(pane.state.selected(), None);
        assert_eq!(app.lines.len(), 5);
        app.handle_key(KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE));
        assert_eq!(app.branches.as_ref().unwrap().filter, "ba q");
        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        press(&mut app, 'j', KeyModifiers::NONE);
        press(&mut app, ' ', KeyModifiers::NONE);
        assert_eq!(app.lines.len(), 5);
        app.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert_eq!(app.branches.as_ref().unwrap().state.selected(), Some(0));
    }

    #[test]
    fn filter_title_is_visible_and_selection_is_unchanged() {
        use ratatui::{backend::TestBackend, Terminal};

        let mut app = app(5, 4);
        app.included_branches.insert("refs/heads/main".into());
        app.branches = Some(BranchPane::new(Ok(vec!["* refs/heads/main".into()])));
        press(&mut app, '/', KeyModifiers::NONE);
        press(&mut app, 'z', KeyModifiers::NONE);
        let mut terminal = Terminal::new(TestBackend::new(120, 8)).unwrap();
        terminal.draw(|frame| app.draw(frame)).unwrap();
        let buffer = terminal.backend().buffer();
        let title: String = (78..120).map(|x| buffer[(x, 0)].symbol()).collect();
        let body: String = (78..120).map(|x| buffer[(x, 1)].symbol()).collect();
        assert!(title.contains("Branches  Filter: z"));
        assert!(body.contains("No matching branches."));
        assert!(app.included_branches.contains("refs/heads/main"));
        app.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert!(!app.branches.as_ref().unwrap().editing_filter);
        assert!(app.branches.as_ref().unwrap().filter.is_empty());
    }

    #[test]
    fn quit_keys_exit() {
        let mut app = app(3, 2);
        assert!(press(&mut app, 'q', KeyModifiers::NONE));
        assert!(press(&mut app, 'c', KeyModifiers::CONTROL));
        assert!(app.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)));
    }

    #[test]
    fn git_colors_survive_selection_and_hints_are_gray() {
        use ratatui::{backend::TestBackend, Terminal};

        let text = b"\x1b[31m*\x1b[m \x1b[33mabc123\x1b[m message\n|/";
        let mut app = App::new(text.into_text().unwrap().lines);
        let mut terminal = Terminal::new(TestBackend::new(80, 5)).unwrap();
        terminal.draw(|frame| app.draw(frame)).unwrap();
        let buffer = terminal.backend().buffer();
        assert_eq!(buffer[(0, 0)].symbol(), "*");
        assert_eq!(buffer[(0, 0)].fg, Color::Red);
        assert_eq!(buffer[(2, 0)].fg, Color::Yellow);
        assert!(buffer[(0, 0)].modifier.contains(Modifier::BOLD));
        assert_eq!(buffer[(0, 4)].fg, Color::DarkGray);

        app.move_down(1);
        terminal.draw(|frame| app.draw(frame)).unwrap();
        let buffer = terminal.backend().buffer();
        assert_eq!(buffer[(0, 0)].fg, Color::Red);
        assert!(!buffer[(0, 0)].modifier.contains(Modifier::BOLD));
    }

    #[test]
    fn selected_line_is_bold_without_changing_graph_text() {
        use ratatui::{backend::TestBackend, Terminal};

        let mut terminal = Terminal::new(TestBackend::new(80, 5)).unwrap();
        let mut app = App::new(vec!["* abc123 (HEAD -> main) first".into(), "|/".into()]);
        terminal.draw(|frame| app.draw(frame)).unwrap();
        let buffer = terminal.backend().buffer();
        assert_eq!(buffer[(0, 0)].symbol(), "*");
        assert!(buffer[(0, 0)].modifier.contains(Modifier::BOLD));
        assert!(!buffer[(0, 1)].modifier.contains(Modifier::BOLD));
        app.move_down(1);
        terminal.draw(|frame| app.draw(frame)).unwrap();
        let buffer = terminal.backend().buffer();
        assert!(!buffer[(0, 0)].modifier.contains(Modifier::BOLD));
        assert!(buffer[(0, 1)].modifier.contains(Modifier::BOLD));
    }
}
