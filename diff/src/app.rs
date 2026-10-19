use crate::{
    diff::{Diff, Kind},
    repo::{Operation, Snapshot},
    theme::Theme,
};
use crossterm::event::KeyCode;
use ratatui::{
    prelude::*,
    widgets::{Paragraph, Wrap},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Line,
    Hunk,
}
#[derive(Clone, Debug)]
pub struct Nav {
    pub cursor: Option<usize>,
    pub mode: Mode,
    pub anchor: Option<usize>,
    pub scroll: usize,
    pub top_padding: usize,
    pub follow_cursor: bool,
}
impl Nav {
    fn center_target(&self, diff: &Diff) -> Option<usize> {
        let cursor = self.cursor?;
        if self.mode == Mode::Hunk {
            if let Some(hunk) = diff.hunk(cursor).map(|h| &diff.hunks[h]) {
                return Some(hunk.start + (hunk.len() - 1) / 2);
            }
        }
        Some(cursor)
    }
}
impl Default for Nav {
    fn default() -> Self {
        Self {
            cursor: None,
            mode: Mode::Hunk,
            anchor: None,
            scroll: 0,
            top_padding: 0,
            follow_cursor: true,
        }
    }
}
#[derive(Clone)]
pub struct Request {
    pub operation: Operation,
    pub selected: Vec<usize>,
    pub snapshot: Snapshot,
}
pub enum Intent {
    None,
    Quit,
    Mutate(Request),
}
pub struct App {
    pub theme: Theme,
    pub snapshot: Snapshot,
    pub diffs: [Diff; 2],
    pub nav: [Nav; 2],
    pub tab: usize,
    pub preview_scroll: usize,
    pub preview_top_padding: usize,
    pub horizontal: u16,
    pub message: String,
    pub valid: bool,
    pub confirmation: Option<Request>,
    pub viewport_height: usize,
}
impl App {
    pub fn new(snapshot: Snapshot) -> Self {
        let diffs = [
            Diff::between(&snapshot.index, &snapshot.work),
            Diff::between(&snapshot.head, &snapshot.index),
        ];
        let nav = std::array::from_fn(|i| Nav {
            cursor: diffs[i].first(),
            ..Nav::default()
        });
        Self {
            theme: Theme::default(),
            snapshot,
            diffs,
            nav,
            tab: 0,
            preview_scroll: 0,
            preview_top_padding: 0,
            horizontal: 0,
            message: String::new(),
            valid: true,
            confirmation: None,
            viewport_height: 0,
        }
    }
    pub fn selecting(&self) -> bool {
        self.tab < 2 && self.nav[self.tab].anchor.is_some()
    }
    pub fn frozen(&self) -> bool {
        self.selecting() || self.confirmation.is_some()
    }
    pub fn replace(&mut self, snapshot: Snapshot) {
        let changed = [
            snapshot.index != self.snapshot.index || snapshot.work != self.snapshot.work,
            snapshot.head != self.snapshot.head || snapshot.index != self.snapshot.index,
        ];
        for (i, changed) in changed.into_iter().enumerate() {
            if changed {
                let diff = if i == 0 {
                    Diff::between(&snapshot.index, &snapshot.work)
                } else {
                    Diff::between(&snapshot.head, &snapshot.index)
                };
                self.nav[i].cursor = diff.retain_cursor(&self.diffs[i], self.nav[i].cursor);
                self.diffs[i] = diff;
            }
        }
        self.snapshot = snapshot;
        self.valid = true;
    }
    pub fn submitted(&mut self, snapshot: Snapshot, request: &Request) {
        let tab = self.tab;
        let diff = &self.diffs[tab];
        let next = request.selected.last().and_then(|last| {
            (last + 1..diff.rows.len())
                .find(|i| diff.actionable(*i))
                .or_else(|| {
                    (0..*request.selected.first().unwrap())
                        .rev()
                        .find(|i| diff.actionable(*i))
                })
        });
        // Retain the next surviving change, not the just-submitted cursor.
        self.nav[tab].cursor = next;
        self.cancel_selection();
        self.replace(snapshot);
        self.nav[tab].follow_cursor = true;
        self.message = format!(
            "{:?}: {} changed row(s) applied",
            request.operation,
            request.selected.len()
        );
        if self.nav[tab].cursor.is_none() {
            self.message
                .push_str(" — All changes submitted; no remaining changes in this view");
        }
    }
    pub fn scroll_page(&mut self, down: bool) {
        let amount = (self.viewport_height / 2).max(1);
        let (scroll, padding, len) = if self.tab == 2 {
            (
                &mut self.preview_scroll,
                &mut self.preview_top_padding,
                self.snapshot.index.lines().count(),
            )
        } else {
            let nav = &mut self.nav[self.tab];
            nav.follow_cursor = false;
            (
                &mut nav.scroll,
                &mut nav.top_padding,
                self.diffs[self.tab].rows.len(),
            )
        };
        let origin = *scroll as i128 - *padding as i128;
        let delta = if down {
            amount as i128
        } else {
            -(amount as i128)
        };
        let max = (len as i128 + 2 - self.viewport_height as i128).max(0);
        let origin = if len == 0 {
            0
        } else {
            (origin + delta).clamp(-2, max)
        };
        *scroll = origin.max(0) as usize;
        *padding = (-origin).max(0) as usize;
    }
    pub fn cancel_selection(&mut self) {
        for nav in &mut self.nav {
            nav.anchor = None;
        }
        self.confirmation = None;
    }
    pub fn selected(&self) -> Vec<usize> {
        if self.tab == 2 {
            return Vec::new();
        }
        let nav = &self.nav[self.tab];
        let diff = &self.diffs[self.tab];
        let Some(cursor) = nav.cursor else {
            return Vec::new();
        };
        if let Some(anchor) = nav.anchor {
            (anchor.min(cursor)..=anchor.max(cursor))
                .filter(|i| diff.actionable(*i))
                .collect()
        } else if nav.mode == Mode::Hunk {
            diff.hunk(cursor)
                .map(|h| {
                    diff.hunks[h]
                        .clone()
                        .filter(|i| diff.actionable(*i))
                        .collect()
                })
                .unwrap_or_default()
        } else {
            vec![cursor]
        }
    }
    pub fn key(&mut self, key: KeyCode) -> Intent {
        if key == KeyCode::Char('q') {
            return Intent::Quit;
        }
        if self.confirmation.is_some() {
            match key {
                KeyCode::Char('y') => return Intent::Mutate(self.confirmation.take().unwrap()),
                KeyCode::Char('n') | KeyCode::Esc => {
                    self.confirmation = None;
                    self.message = "Discard cancelled".into();
                    if key == KeyCode::Esc && !self.selecting() {
                        return self.key(KeyCode::Esc);
                    }
                }
                _ => {}
            }
            return Intent::None;
        }
        match key {
            KeyCode::Char('[') if !self.selecting() => {
                self.tab = (self.tab + 2) % 3;
            }
            KeyCode::Char(']') if !self.selecting() => {
                self.tab = (self.tab + 1) % 3;
            }
            KeyCode::Left => {
                self.horizontal = self.horizontal.saturating_sub(4);
            }
            KeyCode::Right => {
                self.horizontal = self.horizontal.saturating_add(4);
            }
            KeyCode::Char('j') | KeyCode::Down if self.tab == 2 => {
                self.preview_top_padding = 0;
                self.preview_scroll = (self.preview_scroll + 1)
                    .min(self.snapshot.index.lines().count().saturating_sub(1));
            }
            KeyCode::Char('k') | KeyCode::Up if self.tab == 2 => {
                self.preview_top_padding = 0;
                self.preview_scroll = self.preview_scroll.saturating_sub(1);
            }
            _ if self.tab == 2 => {}
            KeyCode::Esc => {
                let nav = &mut self.nav[self.tab];
                if nav.anchor.take().is_none() {
                    if let Some(target) = nav.center_target(&self.diffs[self.tab]) {
                        (nav.scroll, nav.top_padding) = centered_viewport(
                            target,
                            self.diffs[self.tab].rows.len(),
                            self.viewport_height,
                        );
                        nav.follow_cursor = false;
                    }
                }
            }
            KeyCode::Tab if !self.selecting() => {
                let nav = &mut self.nav[self.tab];
                nav.follow_cursor = true;
                nav.mode = if nav.mode == Mode::Line {
                    Mode::Hunk
                } else {
                    Mode::Line
                };
            }
            KeyCode::Char('v') if self.nav[self.tab].mode == Mode::Line && !self.selecting() => {
                self.nav[self.tab].anchor = self.nav[self.tab].cursor;
            }
            KeyCode::Char('j') | KeyCode::Down => self.move_cursor(true),
            KeyCode::Char('k') | KeyCode::Up => self.move_cursor(false),
            KeyCode::Char(' ') | KeyCode::Char('d') if self.valid => {
                let selected = self.selected();
                if selected.is_empty() || (key == KeyCode::Char('d') && self.tab != 0) {
                    return Intent::None;
                }
                let operation = if key == KeyCode::Char('d') {
                    Operation::Discard
                } else if self.tab == 0 {
                    Operation::Stage
                } else {
                    Operation::Unstage
                };
                let request = Request {
                    operation,
                    selected,
                    snapshot: self.snapshot.clone(),
                };
                if key == KeyCode::Char('d') {
                    self.confirmation = Some(request);
                } else {
                    return Intent::Mutate(request);
                }
            }
            _ => {}
        }
        Intent::None
    }
    fn move_cursor(&mut self, down: bool) {
        let diff = &self.diffs[self.tab];
        let nav = &mut self.nav[self.tab];
        let Some(cursor) = nav.cursor else {
            return;
        };
        let next = if nav.mode == Mode::Hunk {
            diff.hunk(cursor).and_then(|h| {
                let h = if down {
                    h.checked_add(1)
                } else {
                    h.checked_sub(1)
                }?;
                diff.hunks.get(h)?.clone().find(|i| diff.actionable(*i))
            })
        } else if nav.anchor.is_some() {
            let i = if down {
                cursor.checked_add(1)
            } else {
                cursor.checked_sub(1)
            };
            i.filter(|i| diff.actionable(*i))
        } else if down {
            (cursor + 1..diff.rows.len()).find(|i| diff.actionable(*i))
        } else {
            (0..cursor).rev().find(|i| diff.actionable(*i))
        };
        if next.is_some() {
            nav.cursor = next;
        }
        nav.follow_cursor = true;
    }
    pub fn draw(&mut self, frame: &mut Frame, name: &str) {
        let area = frame.area();
        let status_len = if self.confirmation.is_some() {
            150
        } else {
            self.message.chars().count()
        };
        let status_rows = status_len.div_ceil(area.width.max(1) as usize).clamp(1, 6) as u16;
        let regions = Layout::vertical([
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Min(0),
            Constraint::Length(2 + status_rows),
        ])
        .split(area);
        let titles = ["Working", "Staged", "Preview"];
        let mut tabs = Vec::new();
        for (i, title) in titles.iter().enumerate() {
            if i > 0 {
                tabs.push(Span::raw(" | "));
            }
            let style = if i == self.tab {
                Style::default()
                    .fg(Color::Black)
                    .bg(Color::Gray)
                    .add_modifier(Modifier::UNDERLINED | Modifier::BOLD)
            } else if self.selecting() {
                Style::default().fg(Color::DarkGray)
            } else {
                Style::default()
            };
            tabs.push(Span::styled(*title, style));
        }
        frame.render_widget(Paragraph::new(Line::from(tabs)), regions[0]);
        let mode = if self.tab == 2 {
            "read only"
        } else if self.selecting() {
            "multi-line"
        } else if self.nav[self.tab].mode == Mode::Hunk {
            "hunk"
        } else {
            "single-line"
        };
        frame.render_widget(
            Paragraph::new(format!("{name} — {mode}")).style(Style::default().fg(Color::DarkGray)),
            regions[1],
        );
        let body = regions[2];
        self.viewport_height = body.height as usize;
        if self.tab == 2 {
            let lines: Vec<_> = self.snapshot.index.split_inclusive('\n').collect();
            self.preview_scroll = self.preview_scroll.min(
                lines.len().saturating_sub(1).max(
                    lines
                        .len()
                        .saturating_add(2)
                        .saturating_sub(body.height as usize),
                ),
            );
            let top_padding = self.preview_top_padding.min(body.height as usize);
            if !self.valid {
                frame.render_widget(
                    Paragraph::new("Index content unavailable: resolve the refresh error below."),
                    body,
                );
            } else if lines.is_empty() {
                frame.render_widget(Paragraph::new("Index is empty"), body);
            } else {
                for (screen, text) in lines
                    .iter()
                    .skip(self.preview_scroll)
                    .take(body.height as usize - top_padding)
                    .enumerate()
                {
                    self.render_row(
                        frame,
                        body,
                        screen + top_padding,
                        &format!(
                            "{:>5}   {}",
                            self.preview_scroll + screen + 1,
                            display_text(text)
                        ),
                        Style::default(),
                    );
                }
            }
        } else {
            let diff = &self.diffs[self.tab];
            let nav = &mut self.nav[self.tab];
            let hunk = nav
                .cursor
                .and_then(|i| diff.hunk(i))
                .map(|h| diff.hunks[h].clone());
            if let Some(target) = nav.center_target(diff).filter(|_| nav.follow_cursor) {
                (nav.scroll, nav.top_padding) =
                    centered_viewport(target, diff.rows.len(), body.height as usize);
            }
            nav.scroll = nav.scroll.min(
                diff.rows
                    .len()
                    .saturating_add(2)
                    .saturating_sub(body.height as usize),
            );
            let scroll = nav.scroll;
            let top_padding = nav.top_padding.min(body.height as usize);
            if diff.rows.is_empty() {
                frame.render_widget(Paragraph::new("Empty file — no content changes"), body);
            }
            for (screen, (i, row)) in diff
                .rows
                .iter()
                .enumerate()
                .skip(scroll)
                .take(body.height as usize - top_padding)
                .enumerate()
            {
                let (mark, color) = match row.kind {
                    Kind::Context => (' ', None),
                    Kind::Add => ('+', Some(Color::Green)),
                    Kind::Remove => ('-', Some(Color::Red)),
                };
                let mut style = Style::default();
                if let Some(color) = color {
                    style = style.fg(color);
                }
                if nav.mode == Mode::Hunk
                    && diff.actionable(i)
                    && hunk.as_ref().is_some_and(|h| h.contains(&i))
                {
                    style = style.bg(self.theme.cursor_bg);
                } else if nav.cursor == Some(i) {
                    style = style.bg(self.theme.cursor_bg);
                } else if nav.anchor.is_some_and(|a| {
                    nav.cursor
                        .is_some_and(|c| (a.min(c)..=a.max(c)).contains(&i))
                }) {
                    style = style.bg(self.theme.selection_bg);
                }
                let number = if row.kind == Kind::Remove {
                    row.old + 1
                } else {
                    row.new + 1
                };
                let line = format!("{number:>5} {mark} {}", display_text(&row.text));
                let rect = Rect::new(
                    body.x,
                    body.y + (screen + top_padding) as u16,
                    body.width,
                    1,
                );
                let content = Paragraph::new(line)
                    .style(style)
                    .scroll((0, self.horizontal));
                frame.render_widget(content, rect);
            }
        }
        let footer = if self.tab == 0 {
            "Space: Stage selection  d: Discard (confirm)"
        } else if self.tab == 1 {
            "Space: Unstage selection"
        } else {
            "Read only"
        };
        let mut lines = vec![
            Line::raw(footer),
            Line::raw(
                "j/k: move  [/]: tabs  Tab: line/hunk  v: select  Esc: cancel/center  Ctrl-U/D: scroll  ←/→: pan  q: quit",
            ),
        ];
        if let Some(request) = &self.confirmation {
            let scope = if self.nav[0].mode == Mode::Hunk {
                "hunk"
            } else if self.selecting() {
                "multi-line selection"
            } else {
                "line"
            };
            lines.push(Line::styled(
                format!(
                    "Discard {} changed row(s) in this {scope}? [y confirms / n or Esc cancels]",
                    request.selected.len()
                ),
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            ));
        } else {
            lines.push(Line::styled(
                &self.message,
                Style::default().fg(if self.valid {
                    Color::Yellow
                } else {
                    Color::Red
                }),
            ));
        }
        let status = lines.split_off(2);
        let footer = regions[3];
        frame.render_widget(
            Paragraph::new(lines),
            Rect::new(footer.x, footer.y, footer.width, footer.height.min(2)),
        );
        if footer.height > 2 {
            frame.render_widget(
                Paragraph::new(status).wrap(Wrap { trim: false }),
                Rect::new(footer.x, footer.y + 2, footer.width, footer.height - 2),
            );
        }
    }
    fn render_row(&self, frame: &mut Frame, body: Rect, row: usize, text: &str, style: Style) {
        frame.render_widget(
            Paragraph::new(text)
                .style(style)
                .scroll((0, self.horizontal)),
            Rect::new(body.x, body.y + row as u16, body.width, 1),
        );
    }
}
// Return the first real row and the blank rows above it. Signed viewport
// origins allow centering near the start without inventing diff rows.
fn centered_viewport(cursor: usize, len: usize, height: usize) -> (usize, usize) {
    if height == 0 || len == 0 {
        return (0, 0);
    }
    let ideal = cursor as i128 - (height / 2) as i128;
    let end = len as i128 - height as i128;
    let (min, max) = if len <= height {
        // Keep short files visible, but never add more than two blank rows above.
        (end.max(-2), 0)
    } else {
        (-2, end + 2)
    };
    let origin = ideal.clamp(min, max);
    (origin.max(0) as usize, (-origin).max(0) as usize)
}

fn display_text(text: &str) -> String {
    let no_newline = !text.ends_with('\n');
    let text = text.strip_suffix('\n').unwrap_or(text);
    let text = text.strip_suffix('\r').unwrap_or(text);
    let mut result = String::new();
    for ch in text.chars() {
        match ch {
            '\t' => result.push_str("    "),
            c if c.is_control() => result.push('�'),
            c => result.push(c),
        }
    }
    if no_newline {
        result.push_str(" ⟪no newline⟫");
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    fn app() -> App {
        App::new(Snapshot {
            head: "a\nb\nc\nd\ne\n".into(),
            index: "a\nb\nc\nd\ne\n".into(),
            work: "A\nB\nc\nD\ne\n".into(),
            head_oid: "h".into(),
            index_oid: "i".into(),
            mode: "100644".into(),
            dev: 0,
            ino: 0,
            permissions: 0,
        })
    }
    fn key(a: &mut App, c: char) {
        a.key(KeyCode::Char(c));
    }
    #[test]
    fn selection_is_bounded_and_blocks_switching() {
        let mut a = app();
        a.key(KeyCode::Tab);
        key(&mut a, 'v');
        for _ in 0..20 {
            key(&mut a, 'j');
        }
        let cursor = a.nav[0].cursor.unwrap();
        assert!(!a.diffs[0].actionable(cursor + 1));
        key(&mut a, ']');
        a.key(KeyCode::Tab);
        assert_eq!(a.tab, 0);
        assert_eq!(a.nav[0].mode, Mode::Line);
        assert_eq!(a.selected(), (0..=cursor).collect::<Vec<_>>());
        a.key(KeyCode::Esc);
        key(&mut a, 'j');
        assert!(a.nav[0].cursor.unwrap() > cursor + 1);
    }
    #[test]
    fn tab_local_state_hunks_and_preview() {
        let mut a = app();
        assert_eq!(a.nav[0].mode, Mode::Hunk);
        a.key(KeyCode::Tab);
        key(&mut a, 'j');
        let cursor = a.nav[0].cursor;
        a.key(KeyCode::Tab);
        assert_eq!(a.nav[0].mode, Mode::Hunk);
        assert!(a.selected().len() > 1);
        key(&mut a, ']');
        assert_eq!(a.nav[1].mode, Mode::Hunk);
        assert_eq!(a.nav[1].cursor, None);
        key(&mut a, ']');
        assert!(matches!(a.key(KeyCode::Char(' ')), Intent::None));
        key(&mut a, ']');
        assert_eq!(a.nav[0].cursor, cursor);
        assert_eq!(a.nav[0].mode, Mode::Hunk);
    }
    #[test]
    fn confirmation_is_explicit_and_captures_snapshot() {
        let mut a = app();
        key(&mut a, 'd');
        assert!(a.confirmation.is_some());
        assert!(matches!(a.key(KeyCode::Enter), Intent::None));
        a.key(KeyCode::Esc);
        assert!(a.confirmation.is_none());
        key(&mut a, 'd');
        assert!(matches!(a.key(KeyCode::Char('y')), Intent::Mutate(_)));
    }
    #[test]
    fn refresh_reuses_unaffected_diffs() {
        let mut a = app();
        let working_rows = a.diffs[0].rows.as_ptr();
        let staged_rows = a.diffs[1].rows.as_ptr();
        let mut snapshot = a.snapshot.clone();
        snapshot.permissions = 0o644;
        a.replace(snapshot);
        assert_eq!(a.diffs[0].rows.as_ptr(), working_rows);
        assert_eq!(a.diffs[1].rows.as_ptr(), staged_rows);

        let mut snapshot = a.snapshot.clone();
        snapshot.work = "external\n".into();
        a.replace(snapshot);
        assert_ne!(a.diffs[0].rows.as_ptr(), working_rows);
        assert_eq!(a.diffs[1].rows.as_ptr(), staged_rows);
        let working_rows = a.diffs[0].rows.as_ptr();
        let mut snapshot = a.snapshot.clone();
        snapshot.head = "committed\n".into();
        a.replace(snapshot);
        assert_eq!(a.diffs[0].rows.as_ptr(), working_rows);
        assert_ne!(a.diffs[1].rows.as_ptr(), staged_rows);

        let staged_rows = a.diffs[1].rows.as_ptr();
        let mut snapshot = a.snapshot.clone();
        snapshot.index = "staged\n".into();
        a.replace(snapshot);
        assert_ne!(a.diffs[0].rows.as_ptr(), working_rows);
        assert_ne!(a.diffs[1].rows.as_ptr(), staged_rows);
        for (i, expected) in [
            Diff::between(&a.snapshot.index, &a.snapshot.work),
            Diff::between(&a.snapshot.head, &a.snapshot.index),
        ]
        .iter()
        .enumerate()
        {
            assert_eq!(format!("{:?}", a.diffs[i]), format!("{expected:?}"));
        }
    }
    fn submit(a: &mut App) {
        let Intent::Mutate(request) = a.key(KeyCode::Char(' ')) else {
            panic!("expected mutation");
        };
        let mut snapshot = a.snapshot.clone();
        snapshot.index = a.diffs[a.tab].apply(&request.selected, a.tab == 1);
        a.submitted(snapshot, &request);
    }
    #[test]
    fn submissions_advance_down_then_up_in_both_views() {
        for tab in 0..2 {
            let mut a = app();
            if tab == 1 {
                a.snapshot.index = a.snapshot.work.clone();
                a = App::new(a.snapshot);
            }
            a.tab = tab;
            a.nav[tab].mode = Mode::Line;
            // The nearest change is above, but submission must prefer below.
            a.nav[tab].cursor = Some(3);
            submit(&mut a);
            let cursor = a.nav[tab].cursor.unwrap();
            assert_eq!(a.diffs[tab].rows[cursor].text, "d\n");
            // At the last change, fall back to the previous remaining row.
            a.nav[tab].cursor = a.diffs[tab]
                .rows
                .iter()
                .rposition(|r| r.kind != Kind::Context);
            submit(&mut a);
            let cursor = a.nav[tab].cursor.unwrap();
            assert_eq!(a.diffs[tab].rows[cursor].text, "d\n");
            a.nav[tab].mode = Mode::Hunk;
            submit(&mut a);
            assert_eq!(a.nav[tab].cursor, None);
            assert!(a.message.contains("All changes submitted"));
        }
    }
    #[test]
    fn scrolling_leaves_cursor_and_selection_unchanged_and_escape_centers() {
        use ratatui::{backend::TestBackend, Terminal};
        let mut a = app();
        let text = (0..100).map(|i| format!("line {i}\n")).collect::<String>();
        let mut snapshot = a.snapshot.clone();
        snapshot.index = text.clone();
        snapshot.head = text.clone();
        snapshot.work = text.replace("line 50\n", "changed\n");
        a.replace(snapshot);
        let mut terminal = Terminal::new(TestBackend::new(100, 25)).unwrap();
        terminal.draw(|f| a.draw(f, "test")).unwrap();
        let cursor = a.nav[0].cursor;
        let scroll = a.nav[0].scroll;
        let half = a.viewport_height / 2;
        a.scroll_page(false);
        terminal.draw(|f| a.draw(f, "test")).unwrap();
        assert_eq!(a.nav[0].scroll, scroll - half);
        assert_eq!(a.nav[0].cursor, cursor);
        a.scroll_page(true);
        assert_eq!(a.nav[0].scroll, scroll);
        a.key(KeyCode::Esc);
        terminal.draw(|f| a.draw(f, "test")).unwrap();
        assert_eq!(
            a.nav[0].scroll,
            a.nav[0].center_target(&a.diffs[0]).unwrap() - a.viewport_height / 2
        );
        a.key(KeyCode::Tab);
        key(&mut a, 'v');
        let selected = a.selected();
        a.scroll_page(true);
        assert_eq!(a.selected(), selected);
        let scroll = a.nav[0].scroll;
        a.key(KeyCode::Esc);
        assert!(!a.selecting());
        assert_eq!(a.nav[0].scroll, scroll);
    }
    #[test]
    fn manual_scrolling_reaches_both_padded_boundaries() {
        use ratatui::{backend::TestBackend, Terminal};
        for tab in 0..3 {
            let mut a = app();
            let text = (0..100).map(|i| format!("line {i}\n")).collect::<String>();
            a.snapshot.head = text.clone();
            a.snapshot.index = text.clone();
            a.snapshot.work = text.replace("line 50\n", "changed\n");
            a = App::new(a.snapshot);
            a.tab = tab;
            let mut terminal = Terminal::new(TestBackend::new(100, 25)).unwrap();
            terminal.draw(|f| a.draw(f, "test")).unwrap();
            let cursor = if tab < 2 { a.nav[tab].cursor } else { None };
            for down in [false, true, false] {
                for _ in 0..30 {
                    a.scroll_page(down);
                    terminal.draw(|f| a.draw(f, "test")).unwrap();
                }
                let (scroll, padding, len) = if tab == 2 {
                    (a.preview_scroll, a.preview_top_padding, 100)
                } else {
                    assert_eq!(a.nav[tab].cursor, cursor);
                    (
                        a.nav[tab].scroll,
                        a.nav[tab].top_padding,
                        a.diffs[tab].rows.len(),
                    )
                };
                if down {
                    assert_eq!(padding, 0);
                    assert_eq!(scroll + a.viewport_height, len + 2);
                } else {
                    assert_eq!((scroll, padding), (0, 2));
                    let buffer = terminal.backend().buffer();
                    for y in 2..4 {
                        for x in 0..100 {
                            assert_eq!(buffer[(x, y)].symbol(), " ");
                        }
                    }
                }
            }
        }
    }
    #[test]
    fn hunk_center_target_is_independent_of_cursor_position() {
        let mut a = app();
        let range = a.diffs[0].hunks[0].clone();
        let middle = range.start + (range.len() - 1) / 2;
        for cursor in range.filter(|i| a.diffs[0].actionable(*i)) {
            a.nav[0].cursor = Some(cursor);
            a.nav[0].mode = Mode::Hunk;
            assert_eq!(a.nav[0].center_target(&a.diffs[0]), Some(middle));
            a.nav[0].mode = Mode::Line;
            assert_eq!(a.nav[0].center_target(&a.diffs[0]), Some(cursor));
        }
    }
    #[test]
    fn centering_limits_blank_rows_and_keeps_short_files_visible() {
        assert_eq!(centered_viewport(50, 100, 20), (40, 0));
        assert_eq!(centered_viewport(0, 100, 20), (0, 2));
        assert_eq!(centered_viewport(99, 100, 20), (82, 0));
        assert_eq!(centered_viewport(0, 5, 20), (0, 2));
        assert_eq!(centered_viewport(4, 5, 20), (0, 2));
        assert_eq!(centered_viewport(19, 20, 20), (0, 0));
        assert_eq!(centered_viewport(0, 0, 20), (0, 0));
        assert_eq!(centered_viewport(0, 100, 0), (0, 0));
        for height in 1..30 {
            for len in 1..100 {
                for cursor in 0..len {
                    let (scroll, padding) = centered_viewport(cursor, len, height);
                    assert!(padding <= 2);
                    if len > height {
                        assert!((scroll + height).saturating_sub(len) <= 2);
                    }
                    assert!((scroll..scroll + height - padding).contains(&cursor));
                }
            }
        }
    }
    #[test]
    fn navigation_and_submission_recenter_in_both_views_and_modes() {
        use ratatui::{backend::TestBackend, Terminal};
        for tab in 0..2 {
            for mode in [Mode::Line, Mode::Hunk] {
                let mut a = app();
                let text = (0..100).map(|i| format!("line {i}\n")).collect::<String>();
                let changed = text
                    .replace("line 30\n", "changed 30\n")
                    .replace("line 50\n", "changed 50\n")
                    .replace("line 70\n", "changed 70\n");
                a.snapshot.head = text.clone();
                a.snapshot.index = if tab == 0 {
                    text.clone()
                } else {
                    changed.clone()
                };
                a.snapshot.work = changed;
                a = App::new(a.snapshot);
                a.tab = tab;
                a.nav[tab].mode = mode;
                let mut terminal = Terminal::new(TestBackend::new(100, 25)).unwrap();
                for action in ['j', 'k', 'j', ' '] {
                    // Navigation must also resume following after manual scrolling.
                    a.scroll_page(true);
                    if action == ' ' {
                        submit(&mut a);
                    } else {
                        key(&mut a, action);
                    }
                    terminal.draw(|f| a.draw(f, "test")).unwrap();
                    let nav = &a.nav[tab];
                    assert!(nav.follow_cursor);
                    assert_eq!(nav.top_padding, 0);
                    assert_eq!(
                        nav.scroll,
                        nav.center_target(&a.diffs[tab]).unwrap() - a.viewport_height / 2
                    );
                }
            }
        }
    }
    #[test]
    fn hunk_highlight_excludes_context() {
        use ratatui::{backend::TestBackend, Terminal};
        let mut a = app();
        let mut terminal = Terminal::new(TestBackend::new(100, 25)).unwrap();
        terminal.draw(|f| a.draw(f, "test")).unwrap();
        let buffer = terminal.backend().buffer();
        for (i, row) in a.diffs[0].rows.iter().enumerate() {
            let bg = buffer[(0, (i + a.nav[0].top_padding) as u16 + 2)].bg;
            assert_eq!(bg == a.theme.cursor_bg, row.kind != Kind::Context);
        }
    }
    #[test]
    fn logical_cursor_and_empty_state() {
        let mut a = app();
        let target = a.diffs[0]
            .rows
            .iter()
            .position(|r| r.text == "D\n")
            .unwrap();
        a.nav[0].cursor = Some(target);
        let mut snapshot = a.snapshot.clone();
        snapshot.work = format!("intro\n{}", snapshot.work);
        a.replace(snapshot);
        assert_eq!(a.diffs[0].rows[a.nav[0].cursor.unwrap()].text, "D\n");
        let mut snapshot = a.snapshot.clone();
        snapshot.work = snapshot.index.clone();
        a.replace(snapshot);
        assert_eq!(a.nav[0].cursor, None);
        assert!(a.selected().is_empty());
    }
}
