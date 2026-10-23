//! Interactive Git log viewer with branch filtering.
//!
//! This crate provides the log-TUI half of `gitglass`.  Use [`run`] to
//! start the event loop; it manages its own terminal lifecycle.

use std::{
    collections::{BTreeMap, BTreeSet},
    error::Error,
    io,
    process::Command,
    time::{Duration, Instant},
};

use ansi_to_tui::IntoText;
use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use gitglass_core::{
    actions,
    config::{self, Binding},
};
use ratatui::{
    layout::{Constraint, Layout},
    style::{Color, Modifier, Style},
    text::{Line, Span},
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

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum SearchMode {
    #[default]
    Normal,
    Input,
    Results,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct SearchMatch {
    line: usize,
    start: usize,
    end: usize,
}

#[derive(Default)]
struct Search {
    mode: SearchMode,
    query: String,
    cursor: usize,
    matches: Vec<SearchMatch>,
    active: Option<usize>,
    message: Option<&'static str>,
}

struct App {
    lines: Vec<Line<'static>>,
    selected: usize,
    offset: usize,
    page_height: usize,
    branches: Option<BranchPane>,
    included_branches: BTreeSet<String>,
    log_error: Option<String>,
    action_message: Option<String>,
    log_hints: String,
    repository_name: Option<String>,
    search: Search,
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
            action_message: None,
            log_hints: "j/k: move  Ctrl-d/u: half page  /: search  b: branches  q: quit".into(),
            repository_name: None,
            search: Search::default(),
        }
    }

    fn include_binding_hints(&mut self, bindings: &[Binding]) {
        for binding in bindings {
            self.log_hints.push_str("  ");
            self.log_hints.push_str(&binding.help_text);
        }
    }

    fn resize(&mut self, height: usize) {
        self.page_height = height;
        if self.search.mode == SearchMode::Input {
            self.preview_search();
        } else {
            self.keep_visible();
        }
    }

    fn keep_visible(&mut self) {
        if self
            .search
            .active
            .and_then(|index| self.search.matches.get(index))
            .is_some_and(|item| item.line != self.selected)
        {
            self.search.active = None;
            self.search.message = None;
        }
        self.show_line(self.selected);
    }

    fn show_line(&mut self, line: usize) {
        if self.page_height == 0 {
            return;
        }
        if line < self.offset {
            self.offset = line;
        } else if line >= self.offset.saturating_add(self.page_height) {
            self.offset = line + 1 - self.page_height;
        }
        // The first two display rows are padding, not selectable log entries.
        if self.offset == 0 && line >= self.page_height.saturating_sub(self.top_padding()) {
            self.offset = (line + 1).saturating_sub(self.page_height).max(1);
        }
        let max_offset = self
            .lines
            .len()
            .saturating_sub(self.page_height)
            .max(usize::from(
                self.lines.len() > self.page_height.saturating_sub(self.top_padding()),
            ));
        self.offset = self.offset.min(max_offset);
    }

    fn top_padding(&self) -> usize {
        // Keep at least one log row usable in tiny terminals.
        self.page_height.saturating_sub(1).min(2)
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

    fn update_search(&mut self) {
        let active = self
            .search
            .active
            .and_then(|index| self.search.matches.get(index))
            .copied();
        self.search.matches.clear();
        if !self.search.query.is_empty() {
            for (line, text) in self.lines.iter().enumerate() {
                for (start, matched) in text.to_string().match_indices(&self.search.query) {
                    self.search.matches.push(SearchMatch {
                        line,
                        start,
                        end: start + matched.len(),
                    });
                }
            }
        }
        self.search.active =
            active.and_then(|active| self.search.matches.iter().position(|item| *item == active));
        self.search.message = None;
        if self.search.mode == SearchMode::Input {
            self.search.active = None;
            self.preview_search();
        }
    }

    fn preview_search(&mut self) {
        if let Some(first) = self.search.matches.first() {
            self.show_line(first.line);
        } else {
            self.keep_visible();
        }
    }

    fn select_search_match(&mut self, index: usize) {
        self.search.active = Some(index);
        self.selected = self.search.matches[index].line;
        self.search.message = None;
        self.keep_visible();
    }

    fn navigate_search(&mut self, forward: bool) {
        let next = match self.search.active {
            Some(index) if forward => (index + 1 < self.search.matches.len()).then_some(index + 1),
            Some(index) => index.checked_sub(1),
            None if forward => self
                .search
                .matches
                .iter()
                .position(|item| item.line > self.selected),
            None => self
                .search
                .matches
                .iter()
                .rposition(|item| item.line < self.selected),
        };
        if let Some(index) = next {
            self.select_search_match(index);
        } else {
            self.search.message = Some(if self.search.matches.is_empty() {
                "No search results"
            } else if forward {
                "No next search result"
            } else {
                "No previous search result"
            });
        }
    }

    fn search_key(&mut self, key: KeyEvent) -> bool {
        if key.code == KeyCode::Esc && self.search.mode != SearchMode::Normal {
            self.search = Search::default();
            self.keep_visible();
            return true;
        }
        if self.search.mode == SearchMode::Input {
            match (key.code, key.modifiers) {
                (KeyCode::Enter, _) => {
                    self.search.mode = SearchMode::Results;
                    if self.search.matches.is_empty() {
                        self.search.message = Some("No search results");
                    } else {
                        self.select_search_match(0);
                    }
                }
                (KeyCode::Char('u'), KeyModifiers::CONTROL) => {
                    self.search.query.clear();
                    self.search.cursor = 0;
                    self.update_search();
                }
                (KeyCode::Left, _) => {
                    self.search.cursor = self.search.query[..self.search.cursor]
                        .char_indices()
                        .next_back()
                        .map_or(0, |(index, _)| index);
                }
                (KeyCode::Right, _) => {
                    if let Some(c) = self.search.query[self.search.cursor..].chars().next() {
                        self.search.cursor += c.len_utf8();
                    }
                }
                (KeyCode::Home, _) => self.search.cursor = 0,
                (KeyCode::End, _) => self.search.cursor = self.search.query.len(),
                (KeyCode::Backspace, _) => {
                    if let Some((previous, _)) = self.search.query[..self.search.cursor]
                        .char_indices()
                        .next_back()
                    {
                        self.search.query.drain(previous..self.search.cursor);
                        self.search.cursor = previous;
                        self.update_search();
                    }
                }
                (KeyCode::Delete, _) => {
                    if self.search.cursor < self.search.query.len() {
                        self.search.query.remove(self.search.cursor);
                        self.update_search();
                    }
                }
                (KeyCode::Char(c), KeyModifiers::NONE | KeyModifiers::SHIFT) => {
                    self.search.query.insert(self.search.cursor, c);
                    self.search.cursor += c.len_utf8();
                    self.update_search();
                }
                _ => {}
            }
            return true;
        }
        match (key.code, key.modifiers) {
            (KeyCode::Char('/'), KeyModifiers::NONE) => {
                self.search.mode = SearchMode::Input;
                self.search.cursor = self.search.query.len();
                self.update_search();
                true
            }
            (KeyCode::Char('n'), KeyModifiers::NONE) if self.search.mode == SearchMode::Results => {
                self.navigate_search(true);
                true
            }
            (KeyCode::Char('N'), KeyModifiers::NONE | KeyModifiers::SHIFT)
                if self.search.mode == SearchMode::Results =>
            {
                self.navigate_search(false);
                true
            }
            _ => false,
        }
    }

    fn search_line(&self, index: usize, line: &Line<'static>) -> Line<'static> {
        let start = self
            .search
            .matches
            .partition_point(|item| item.line < index);
        let end = self
            .search
            .matches
            .partition_point(|item| item.line <= index);
        let matches: Vec<_> = self.search.matches[start..end]
            .iter()
            .enumerate()
            .map(|(number, item)| (start + number, item))
            .collect();
        if matches.is_empty() {
            return line.clone();
        }
        let mut result = line.clone();
        result.spans.clear();
        let mut offset = 0;
        for span in &line.spans {
            let end = offset + span.content.len();
            let mut boundaries = vec![offset, end];
            for (_, item) in &matches {
                if item.start > offset && item.start < end {
                    boundaries.push(item.start);
                }
                if item.end > offset && item.end < end {
                    boundaries.push(item.end);
                }
            }
            boundaries.sort_unstable();
            boundaries.dedup();
            for range in boundaries.windows(2) {
                let mut style = span.style;
                if let Some((number, _)) = matches
                    .iter()
                    .find(|(_, item)| item.start <= range[0] && item.end > range[0])
                {
                    let active = self.search.mode == SearchMode::Results
                        && self.search.active == Some(*number);
                    style =
                        style
                            .fg(Color::Black)
                            .bg(if active { Color::Cyan } else { Color::Yellow });
                }
                result.spans.push(Span::styled(
                    span.content[range[0] - offset..range[1] - offset].to_owned(),
                    style,
                ));
            }
            offset = end;
        }
        result
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
        if key.code == KeyCode::Char('c') && key.modifiers == KeyModifiers::CONTROL {
            return true;
        }
        if self.search_key(key) {
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
                self.update_search();
            }
            Err(error) => self.log_error = Some(error.to_string()),
        }
    }

    fn draw(&mut self, frame: &mut Frame) {
        let [body, footer, repository] = Layout::vertical([
            Constraint::Min(0),
            Constraint::Length(1),
            Constraint::Length(u16::from(self.repository_name.is_some())),
        ])
        .areas(frame.area());
        if let Some(name) = &self.repository_name {
            frame.render_widget(
                Paragraph::new(name.as_str())
                    .style(Style::default().fg(Color::Cyan).add_modifier(Modifier::DIM)),
                repository,
            );
        }
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
            let padding = if self.offset == 0 {
                self.top_padding()
            } else {
                0
            } as u16;
            let body = ratatui::layout::Rect {
                y: body.y + padding,
                height: body.height.saturating_sub(padding),
                ..body
            };
            let items = self
                .lines
                .iter()
                .enumerate()
                .skip(self.offset)
                .take(usize::from(body.height))
                .map(|(index, line)| ListItem::new(self.search_line(index, line)));
            let list =
                List::new(items).highlight_style(Style::default().add_modifier(Modifier::BOLD));
            let mut state = ListState::default().with_selected(
                (self.branches.is_none()
                    && self.selected >= self.offset
                    && self.selected < self.offset.saturating_add(self.page_height))
                .then_some(self.selected.saturating_sub(self.offset)),
            );
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

        let search_hints = format!(
            "/{}  {}{}",
            self.search.query,
            self.search
                .message
                .unwrap_or(if self.search.mode == SearchMode::Input {
                    "Enter: results  Esc: cancel"
                } else {
                    "n/N: next/previous  /: edit  Esc: clear"
                }),
            if self.search.mode == SearchMode::Input
                && !self.search.query.is_empty()
                && self.search.matches.is_empty()
            {
                "  No search results"
            } else {
                ""
            }
        );
        let hints = if self
            .branches
            .as_ref()
            .is_some_and(|pane| pane.editing_filter)
        {
            "Type to filter  Enter: focus list  Esc: clear filter"
        } else if self.branches.is_some() {
            "/: filter  Space: toggle  a: all/none  j/k: move  b: close  Esc: clear/close"
        } else if self.search.mode != SearchMode::Normal {
            search_hints.as_str()
        } else {
            self.log_hints.as_str()
        };
        let editing_search = self.branches.is_none() && self.search.mode == SearchMode::Input;
        let cursor_column = 1 + Line::raw(&self.search.query[..self.search.cursor]).width();
        let scroll = if editing_search {
            cursor_column
                .saturating_sub(usize::from(footer.width.saturating_sub(1)))
                .min(usize::from(u16::MAX)) as u16
        } else {
            0
        };
        let text = if editing_search {
            hints
        } else {
            self.log_error
                .as_deref()
                .or(self.action_message.as_deref())
                .unwrap_or(hints)
        };
        frame.render_widget(
            Paragraph::new(text)
                .scroll((0, scroll))
                .style(
                    Style::default().fg(if self.log_error.is_some() && !editing_search {
                        Color::Red
                    } else {
                        Color::DarkGray
                    }),
                ),
            footer,
        );
        if editing_search && footer.width > 0 && footer.height > 0 {
            let x = cursor_column
                .saturating_sub(usize::from(scroll))
                .min(usize::from(footer.width - 1)) as u16;
            frame.set_cursor_position((footer.x + x, footer.y));
        }
    }

    fn refresh_log(&mut self, result: Result<Vec<Line<'static>>, Box<dyn Error>>) {
        match result {
            Ok(lines) => {
                self.lines = lines;
                self.selected = self.selected.min(self.lines.len().saturating_sub(1));
                self.keep_visible();
                self.log_error = None;
                self.update_search();
            }
            Err(error) => self.log_error = Some(error.to_string()),
        }
    }

    fn run(&mut self, terminal: &mut DefaultTerminal, bindings: &[Binding]) -> io::Result<()> {
        self.include_binding_hints(bindings);
        let cwd = std::env::current_dir()?;
        let mut next_refresh = Instant::now() + REFRESH_INTERVAL;
        loop {
            terminal.draw(|frame| self.draw(frame))?;
            if event::poll(next_refresh.saturating_duration_since(Instant::now()))? {
                if let Event::Key(key) = event::read()? {
                    let mut handled = false;
                    if key.kind != KeyEventKind::Release
                        && self.branches.is_none()
                        && self.search.mode != SearchMode::Input
                    {
                        self.action_message = None;
                        let key = config::normalize_key(key.code, key.modifiers);
                        if let Some(binding) = bindings.iter().find(|binding| binding.key == key) {
                            let hash = self.lines.get(self.selected).and_then(commit_hash);
                            match hash.as_deref().map(resolve_commit_hash).transpose() {
                                Ok(hash) => {
                                    self.action_message = actions::execute_hash(
                                        terminal,
                                        binding,
                                        hash.as_deref(),
                                        &cwd,
                                    )?;
                                }
                                Err(error) => self.action_message = Some(error.to_string()),
                            }
                            handled = true;
                        }
                    }
                    if !handled && self.handle_key(key) {
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

fn commit_hash(line: &Line<'_>) -> Option<String> {
    let text: String = line
        .spans
        .iter()
        .map(|span| span.content.as_ref())
        .collect();
    let hash = text
        .trim_start_matches(|c: char| c.is_whitespace() || "*|\\/_.-".contains(c))
        .split_whitespace()
        .next()?;
    (hash.len() >= 4 && hash.len() <= 64 && hash.bytes().all(|byte| byte.is_ascii_hexdigit()))
        .then(|| hash.to_owned())
}

fn resolve_commit_hash(hash: &str) -> io::Result<String> {
    let output = Command::new("git")
        .args(["rev-parse", "--verify", &format!("{hash}^{{commit}}")])
        .output()?;
    if !output.status.success() {
        return Err(io::Error::other(
            String::from_utf8_lossy(&output.stderr).trim().to_owned(),
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
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

// Follow actual parent links, not graph row positions: merge side branches do
// not advance HEAD~N. Only HEAD's other parents receive HEAD^N labels.
fn head_relative_labels(history: &str) -> BTreeMap<String, Vec<String>> {
    let mut labels: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for (depth, line) in history.lines().take(3).enumerate() {
        let mut fields = line.split_whitespace();
        let Some(hash) = fields.next() else {
            continue;
        };
        if depth > 0 {
            let label = if depth == 1 {
                "HEAD^".to_owned()
            } else {
                format!("HEAD~{depth}")
            };
            labels.entry(hash.to_owned()).or_default().push(label);
        } else {
            for (index, parent) in fields.enumerate().skip(1) {
                labels
                    .entry(parent.to_owned())
                    .or_default()
                    .push(format!("HEAD^{}", index + 1));
            }
        }
    }
    labels
}

fn decorate_relative_log(output: &str, labels: &BTreeMap<String, Vec<String>>) -> String {
    output
        .lines()
        .map(|line| {
            // Explicit fields keep subjects and graph-only lines out of parsing.
            let fields: Vec<_> = line.splitn(4, '\0').collect();
            let [prefix, hash, decorations, subject] = fields.as_slice() else {
                return line.to_owned();
            };
            let mut decorations = decorations.to_string();
            if let Some(labels) = labels.get(*hash) {
                let label = format!("\x1b[36m{}\x1b[m", labels.join(", "));
                if let Some(end) = decorations.rfind(')') {
                    decorations.insert_str(end, &format!(", {label}"));
                } else {
                    decorations = format!(" ({label})");
                }
            }
            format!("{prefix}{decorations} {subject}")
        })
        .collect::<Vec<_>>()
        .join("\n")
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
        "--format=%C(auto)%h%C(reset)%x00%H%x00%C(auto)%d%C(reset)%x00%s",
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
    let history = Command::new("git")
        .args([
            "rev-list",
            "--first-parent",
            "--max-count=3",
            "--parents",
            "HEAD",
            "--",
        ])
        .output()?;
    let labels = if history.status.success() {
        head_relative_labels(&String::from_utf8_lossy(&history.stdout))
    } else {
        BTreeMap::new()
    };
    let decorated = decorate_relative_log(&String::from_utf8_lossy(&output.stdout), &labels);
    Ok(decorated.as_bytes().into_text()?.lines)
}

fn display_directory(path: &std::path::Path, home: Option<&std::path::Path>) -> String {
    if let Some(home) = home.filter(|home| home.is_absolute()) {
        if let Ok(relative) = path.strip_prefix(home) {
            return if relative.as_os_str().is_empty() {
                "~".to_owned()
            } else {
                format!("~/{}", relative.display())
            };
        }
    }
    path.display().to_string()
}

/// Run the interactive Git log TUI.
///
/// Fetches Git data before entering raw mode, so Git errors leave the
/// terminal untouched.  Manages its own terminal lifecycle.
pub fn run() -> Result<(), Box<dyn Error>> {
    let bindings = config::load_log_bindings()?;
    gitglass_core::prepare_signals()?;
    let included_branches = initial_branches()?;
    let mut app = App::new(load_log(&included_branches)?);
    app.included_branches = included_branches;
    let root = Command::new("git")
        .args(["rev-parse", "--show-toplevel"])
        .output()?;
    if root.status.success() {
        let root = String::from_utf8_lossy(&root.stdout);
        let home = std::env::var_os("HOME").map(std::path::PathBuf::from);
        app.repository_name = Some(display_directory(
            std::path::Path::new(root.trim()),
            home.as_deref(),
        ));
    }
    let mut terminal = ratatui::init();
    let result = app.run(&mut terminal, &bindings);
    ratatui::restore();
    result?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relative_labels_follow_first_parents_and_include_merge_parents() {
        let labels = head_relative_labels("aaaa bbbb dddd eeee\nbbbb cccc\ncccc ffff\nffff\n");
        assert!(!labels.contains_key("aaaa"));
        assert!(!labels.contains_key("ffff"));
        assert_eq!(labels["bbbb"], vec!["HEAD^"]);
        assert_eq!(labels["cccc"], vec!["HEAD~2"]);
        assert_eq!(labels["dddd"], vec!["HEAD^2"]);
        assert_eq!(labels["eeee"], vec!["HEAD^3"]);
        assert!(head_relative_labels("aaaa\n").is_empty());
        assert!(head_relative_labels("").is_empty());
    }

    #[test]
    fn relative_decorations_preserve_refs_subjects_colors_and_graph_lines() {
        let labels = head_relative_labels("aaaa bbbb cccc\nbbbb cccc\ncccc\n");
        let output = concat!(
            "* aaaa\0aaaa\0 (HEAD -> main, origin/main)\0tip\n",
            "| * \x1b[33mbbbb\x1b[m\0bbbb\0\0subject (untouched)\n",
            "|/\n",
            "* cccc\0cccc\0\x1b[32m (topic)\x1b[m\0older\n",
        );
        let decorated = decorate_relative_log(output, &labels);
        let text = decorated.as_bytes().into_text().unwrap();
        let lines: Vec<String> = text.lines.iter().map(ToString::to_string).collect();
        assert_eq!(lines[0], "* aaaa (HEAD -> main, origin/main) tip");
        assert_eq!(lines[1], "| * bbbb (HEAD^) subject (untouched)");
        assert_eq!(lines[2], "|/");
        assert_eq!(lines[3], "* cccc (topic, HEAD^2, HEAD~2) older");
        assert_eq!(commit_hash(&text.lines[1]).as_deref(), Some("bbbb"));
        assert!(decorated.contains("\x1b[33mbbbb\x1b[m"));
    }

    #[test]
    fn top_padding_scrolls_away_and_returns_at_the_top() {
        use ratatui::{backend::TestBackend, Terminal};

        let mut app = app(20, 5);
        let mut terminal = Terminal::new(TestBackend::new(80, 6)).unwrap();
        for _ in 0..2 {
            terminal.draw(|frame| app.draw(frame)).unwrap();
            let buffer = terminal.backend().buffer();
            for y in 0..2 {
                assert!((0..80).all(|x| buffer[(x, y)].symbol() == " "));
            }
            assert_eq!(buffer[(0, 2)].symbol(), "*");
            app.move_down(8);
            terminal.draw(|frame| app.draw(frame)).unwrap();
            assert!(app.offset > 0);
            assert_eq!(terminal.backend().buffer()[(0, 0)].symbol(), "*");
            app.move_up(usize::MAX);
        }
    }

    #[test]
    fn repository_directory_matches_status_format() {
        use std::path::Path;

        let home = Some(Path::new("/home/alex"));
        assert_eq!(display_directory(Path::new("/home/alex"), home), "~");
        assert_eq!(
            display_directory(Path::new("/home/alex/projects/app"), home),
            "~/projects/app"
        );
        assert_eq!(
            display_directory(Path::new("/home/alexander/app"), home),
            "/home/alexander/app"
        );
        for home in [None, Some(Path::new("")), Some(Path::new("relative"))] {
            assert_eq!(display_directory(Path::new("/work/app"), home), "/work/app");
        }
    }

    #[test]
    fn repository_path_is_pinned_below_hints() {
        use ratatui::{backend::TestBackend, Terminal};

        let mut app = app(10, 3);
        app.repository_name = Some("~/projects/gitglass".into());
        let mut terminal = Terminal::new(TestBackend::new(100, 5)).unwrap();
        for branches in [None, Some(BranchPane::new(Ok(Vec::new())))] {
            app.branches = branches;
            terminal.draw(|frame| app.draw(frame)).unwrap();
            let buffer = terminal.backend().buffer();
            let repository: String = (0..100).map(|x| buffer[(x, 4)].symbol()).collect();
            assert_eq!(repository.trim_end(), "~/projects/gitglass");
            assert_eq!(buffer[(0, 4)].fg, Color::Cyan);
            assert!(buffer[(0, 4)].modifier.contains(Modifier::DIM));
            assert_eq!(buffer[(0, 3)].fg, Color::DarkGray);
            assert_eq!(app.page_height, 3);
        }
        let mut tiny = Terminal::new(TestBackend::new(1, 1)).unwrap();
        tiny.draw(|frame| app.draw(frame)).unwrap();
    }

    #[test]
    fn configured_bindings_appear_in_log_footer_only() {
        use ratatui::{backend::TestBackend, Terminal};

        let bindings = config::parse_log_config(
            r#"
[[log.keybindings]]
key = "y"
command = 'copy "$HASH"'
description = "Copy commit hash"
[[log.keybindings]]
key = "x"
command = 'echo "$HASH"'
"#,
        )
        .unwrap();
        let mut app = App::new(vec![Line::from("* abc1234 subject")]);
        app.include_binding_hints(&bindings);
        let mut terminal = Terminal::new(TestBackend::new(160, 5)).unwrap();
        terminal.draw(|frame| app.draw(frame)).unwrap();
        let footer = |terminal: &Terminal<TestBackend>| -> String {
            (0..160)
                .map(|x| terminal.backend().buffer()[(x, 4)].symbol())
                .collect()
        };
        let text = footer(&terminal);
        assert!(text.contains("y  Copy commit hash"));
        assert!(text.contains("x  echo \"$HASH\""));
        app.branches = Some(BranchPane::new(Ok(Vec::new())));
        terminal.draw(|frame| app.draw(frame)).unwrap();
        assert!(!footer(&terminal).contains("Copy commit hash"));
        app.branches = None;
        app.action_message = Some("Commit hash copied".into());
        terminal.draw(|frame| app.draw(frame)).unwrap();
        assert!(footer(&terminal).starts_with("Commit hash copied"));
    }

    #[test]
    fn extracts_hash_from_styled_commit_lines_only() {
        use ratatui::text::Span;

        let line = Line::from(vec![
            Span::raw("| * "),
            Span::raw("abc1234"),
            Span::raw(" subject"),
        ]);
        assert_eq!(commit_hash(&line).as_deref(), Some("abc1234"));
        for text in ["|\\", "| |/", "", "* not-a-hash subject"] {
            assert_eq!(commit_hash(&Line::from(text)), None);
        }
    }

    #[test]
    fn y_does_not_copy_while_filtering_branches() {
        let mut app = App::new(vec![Line::from("* abc1234 subject")]);
        let mut pane = BranchPane::new(Ok(vec!["  refs/heads/my-branch".into()]));
        pane.editing_filter = true;
        app.branches = Some(pane);
        assert!(!app.handle_key(KeyEvent::new(KeyCode::Char('y'), KeyModifiers::NONE)));
        assert_eq!(app.branches.as_ref().unwrap().filter, "y");
    }

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
        assert_eq!((app.selected, app.offset), (50, 1));
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

    fn search(app: &mut App, query: &str) {
        press(app, '/', KeyModifiers::NONE);
        for character in query.chars() {
            press(app, character, KeyModifiers::NONE);
        }
    }

    #[test]
    fn search_previews_offscreen_without_moving_cursor_even_after_draw_or_refresh() {
        use ratatui::{backend::TestBackend, Terminal};

        let mut app = app(20, 3);
        search(&mut app, "19");
        assert_eq!(app.search.matches.len(), 1);
        assert_eq!((app.selected, app.offset), (0, 17));
        let mut terminal = Terminal::new(TestBackend::new(80, 4)).unwrap();
        terminal.draw(|frame| app.draw(frame)).unwrap();
        assert_eq!((app.selected, app.offset), (0, 17));
        let row: String = (0..80)
            .map(|x| terminal.backend().buffer()[(x, 2)].symbol())
            .collect();
        assert!(row.starts_with("* 19"));
        app.refresh_log(Ok(app.lines.clone()));
        assert_eq!((app.selected, app.offset), (0, 17));
        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert_eq!(app.search.mode, SearchMode::Results);
        assert_eq!(app.search.active, Some(0));
        assert_eq!(app.selected, 19);
        app.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert_eq!(app.selected, 19);
        assert_eq!(app.search.mode, SearchMode::Normal);
        assert!(app.search.matches.is_empty());
    }

    #[test]
    fn search_navigates_occurrences_without_wrapping_and_retains_query_on_edit() {
        let mut app = App::new(vec!["hit hit".into(), "nothing".into(), "hit".into()]);
        app.resize(2);
        search(&mut app, "hit");
        assert_eq!(app.search.matches.len(), 3);
        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        press(&mut app, 'N', KeyModifiers::SHIFT);
        assert_eq!(app.search.message, Some("No previous search result"));
        press(&mut app, 'n', KeyModifiers::NONE);
        assert_eq!((app.selected, app.search.active), (0, Some(1)));
        press(&mut app, 'n', KeyModifiers::NONE);
        assert_eq!(
            (app.selected, app.offset, app.search.active),
            (2, 1, Some(2))
        );
        press(&mut app, 'n', KeyModifiers::NONE);
        assert_eq!(app.search.message, Some("No next search result"));
        press(&mut app, 'N', KeyModifiers::NONE);
        assert_eq!((app.selected, app.search.active), (0, Some(1)));
        press(&mut app, '/', KeyModifiers::NONE);
        assert_eq!(app.search.mode, SearchMode::Input);
        assert_eq!(app.search.query, "hit");
        assert_eq!(app.search.active, None);
        assert!(!press(&mut app, 'q', KeyModifiers::NONE));
        assert_eq!(app.search.query, "hitq");
        press(&mut app, 'u', KeyModifiers::CONTROL);
        assert!(app.search.query.is_empty());
        assert!(app.search.matches.is_empty());
        assert_eq!(app.selected, 0);
        assert!(!app.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)));
        assert_eq!(app.search.mode, SearchMode::Normal);
    }

    #[test]
    fn moving_off_search_result_clears_active_color_and_searches_from_cursor() {
        let mut app = App::new(vec![
            "hit".into(),
            "gap".into(),
            "hit".into(),
            "gap".into(),
            "hit".into(),
        ]);
        app.resize(3);
        search(&mut app, "hit");
        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        let active_line = app.search_line(0, &app.lines[0]);
        assert_eq!(active_line.spans[0].style.bg, Some(Color::Cyan));
        press(&mut app, 'k', KeyModifiers::NONE);
        assert_eq!(app.search.active, Some(0)); // Boundary: cursor did not move.
        press(&mut app, 'j', KeyModifiers::NONE);
        assert_eq!(app.search.active, None);
        let inactive_line = app.search_line(0, &app.lines[0]);
        assert_eq!(inactive_line.spans[0].style.bg, Some(Color::Yellow));
        app.move_down(2);
        press(&mut app, 'n', KeyModifiers::NONE);
        assert_eq!((app.selected, app.search.active), (4, Some(2)));
        app.move_up(2); // Land on a different matching line without activating it.
        assert_eq!(app.search.active, None);
        press(&mut app, 'n', KeyModifiers::NONE);
        assert_eq!((app.selected, app.search.active), (4, Some(2)));
        app.move_up(2);
        press(&mut app, 'N', KeyModifiers::SHIFT);
        assert_eq!((app.selected, app.search.active), (0, Some(0)));
        app.move_down(4);
        press(&mut app, 'n', KeyModifiers::NONE);
        assert_eq!(app.search.active, None);
        assert_eq!(app.search.message, Some("No next search result"));
    }

    #[test]
    fn search_cursor_moves_and_edits_at_unicode_boundaries() {
        let mut app = app(5, 4);
        search(&mut app, "a界é");
        let key = |app: &mut App, code| {
            app.handle_key(KeyEvent::new(code, KeyModifiers::NONE));
        };
        key(&mut app, KeyCode::Left);
        assert_eq!(app.search.cursor, "a界".len());
        press(&mut app, 'x', KeyModifiers::NONE);
        assert_eq!(app.search.query, "a界xé");
        key(&mut app, KeyCode::Backspace);
        key(&mut app, KeyCode::Left);
        key(&mut app, KeyCode::Delete);
        assert_eq!(app.search.query, "aé");
        assert_eq!(app.search.cursor, 1);
        key(&mut app, KeyCode::Right);
        key(&mut app, KeyCode::Right);
        assert_eq!(app.search.cursor, "aé".len());
        key(&mut app, KeyCode::Home);
        key(&mut app, KeyCode::Left);
        key(&mut app, KeyCode::Backspace);
        assert_eq!(app.search.cursor, 0);
        assert_eq!(app.search.query, "aé");
        key(&mut app, KeyCode::End);
        key(&mut app, KeyCode::Delete);
        assert_eq!(app.search.query, "aé");
        press(&mut app, 'u', KeyModifiers::CONTROL);
        assert_eq!(app.search.cursor, 0);
    }

    #[test]
    fn search_cursor_is_visible_and_scrolls_with_long_input() {
        use ratatui::{backend::TestBackend, Terminal};

        let mut app = app(5, 4);
        let mut terminal = Terminal::new(TestBackend::new(10, 4)).unwrap();
        search(&mut app, "a界é");
        terminal.draw(|frame| app.draw(frame)).unwrap();
        assert_eq!(terminal.get_cursor_position().unwrap().x, 5);
        app.handle_key(KeyEvent::new(KeyCode::Left, KeyModifiers::NONE));
        terminal.draw(|frame| app.draw(frame)).unwrap();
        assert_eq!(terminal.get_cursor_position().unwrap().x, 4);
        for _ in 0..20 {
            press(&mut app, 'x', KeyModifiers::NONE);
        }
        terminal.draw(|frame| app.draw(frame)).unwrap();
        assert_eq!(terminal.get_cursor_position().unwrap().x, 9);
        app.handle_key(KeyEvent::new(KeyCode::Home, KeyModifiers::NONE));
        terminal.draw(|frame| app.draw(frame)).unwrap();
        assert_eq!(terminal.get_cursor_position().unwrap().x, 1);
        assert_eq!(terminal.backend().buffer()[(0, 3)].symbol(), "/");
    }

    #[test]
    fn search_empty_missing_unicode_backspace_and_refresh_are_safe() {
        let mut app = App::new(vec!["é界é".into()]);
        search(&mut app, "界é");
        assert_eq!(
            app.search.matches,
            vec![SearchMatch {
                line: 0,
                start: 2,
                end: 7
            }]
        );
        app.handle_key(KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE));
        assert_eq!(app.search.query, "界");
        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        app.refresh_log(Ok(Vec::new()));
        assert_eq!(app.search.active, None);
        press(&mut app, 'n', KeyModifiers::NONE);
        assert_eq!(app.search.message, Some("No search results"));
        press(&mut app, '/', KeyModifiers::NONE);
        press(&mut app, 'u', KeyModifiers::CONTROL);
        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert_eq!(app.search.mode, SearchMode::Results);
        assert_eq!(app.search.message, Some("No search results"));
    }

    #[test]
    fn search_highlights_across_styled_spans_and_restores_git_colors() {
        use ratatui::{backend::TestBackend, Terminal};

        let line = Line::from(vec![
            Span::styled("aé", Style::default().fg(Color::Red)),
            Span::styled("界 aé界", Style::default().fg(Color::Green)),
        ]);
        let mut app = App::new(vec![line.clone()]);
        search(&mut app, "é界");
        assert_eq!(app.search.matches.len(), 2);
        let highlighted = app.search_line(0, &line);
        assert_eq!(highlighted.to_string(), line.to_string());
        assert_eq!(highlighted.spans[0].style.fg, Some(Color::Red));
        assert_eq!(highlighted.spans[1].style.bg, Some(Color::Yellow));
        let mut terminal = Terminal::new(TestBackend::new(40, 3)).unwrap();
        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        terminal.draw(|frame| app.draw(frame)).unwrap();
        assert_eq!(terminal.backend().buffer()[(1, 1)].bg, Color::Cyan);
        assert_eq!(terminal.backend().buffer()[(6, 1)].bg, Color::Yellow);
        press(&mut app, 'n', KeyModifiers::NONE);
        terminal.draw(|frame| app.draw(frame)).unwrap();
        assert_eq!(terminal.backend().buffer()[(1, 1)].bg, Color::Yellow);
        assert_eq!(terminal.backend().buffer()[(6, 1)].bg, Color::Cyan);
        app.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert_eq!(app.search_line(0, &line), line);
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
        assert_eq!(buffer[(0, 2)].symbol(), "*");
        assert_eq!(buffer[(0, 2)].fg, Color::Red);
        assert_eq!(buffer[(2, 2)].fg, Color::Yellow);
        assert!(buffer[(0, 2)].modifier.contains(Modifier::BOLD));
        assert_eq!(buffer[(0, 4)].fg, Color::DarkGray);

        app.move_down(1);
        terminal.draw(|frame| app.draw(frame)).unwrap();
        let buffer = terminal.backend().buffer();
        assert_eq!(buffer[(0, 2)].fg, Color::Red);
        assert!(!buffer[(0, 2)].modifier.contains(Modifier::BOLD));
    }

    #[test]
    fn selected_line_is_bold_without_changing_graph_text() {
        use ratatui::{backend::TestBackend, Terminal};

        let mut terminal = Terminal::new(TestBackend::new(80, 5)).unwrap();
        let mut app = App::new(vec!["* abc123 (HEAD -> main) first".into(), "|/".into()]);
        terminal.draw(|frame| app.draw(frame)).unwrap();
        let buffer = terminal.backend().buffer();
        assert_eq!(buffer[(0, 2)].symbol(), "*");
        assert!(buffer[(0, 2)].modifier.contains(Modifier::BOLD));
        assert!(!buffer[(0, 3)].modifier.contains(Modifier::BOLD));
        app.move_down(1);
        terminal.draw(|frame| app.draw(frame)).unwrap();
        let buffer = terminal.backend().buffer();
        assert!(!buffer[(0, 2)].modifier.contains(Modifier::BOLD));
        assert!(buffer[(0, 3)].modifier.contains(Modifier::BOLD));
    }
}
