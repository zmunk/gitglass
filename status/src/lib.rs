//! Git status viewer with configurable keybindings.
//!
//! This crate provides the status-TUI half of `gitglass`.  Use
//! [`config::load`] to read the configuration and [`run`] to start the
//! event loop on an already-initialised terminal.

pub mod actions;
pub mod config;
pub mod help;
pub mod status;

use std::{
    io,
    path::Path,
    process::Command,
    time::{Duration, Instant},
};

use ansi_to_tui::IntoText;
use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use ratatui::{
    backend::CrosstermBackend,
    layout::{Constraint, Layout},
    style::{Color, Modifier, Style},
    text::{Line, Text},
    widgets::{List, ListItem, ListState, Paragraph},
    Terminal,
};

use crate::config::Binding;

const REFRESH_INTERVAL: Duration = Duration::from_secs(2);

fn status_text(output: &[u8]) -> io::Result<Text<'static>> {
    if output.is_empty() {
        return Ok(Text::raw("Working tree clean"));
    }

    let mut text = output
        .into_text()
        .map_err(|error| io::Error::other(format!("Could not parse Git status colors: {error}")))?;
    for line in &mut text.lines {
        for span in &mut line.spans {
            if span.style.fg == Some(Color::Red) {
                span.style.fg = Some(Color::LightRed);
            }
        }
    }
    Ok(text)
}

fn git_status() -> io::Result<status::Status> {
    if !Path::new(".git").exists() {
        return Ok(status::Status::message("Not a git repository"));
    }

    match Command::new("git")
        .args([
            "--no-pager",
            "-c",
            "color.status=always",
            "status",
            "-s",
            "-z",
        ])
        .output()
    {
        Ok(output) if output.status.success() => status::parse(&output.stdout),
        Ok(output) => Err(io::Error::other(format!(
            "git status failed ({}):\n{}",
            output.status,
            String::from_utf8_lossy(&output.stderr)
        ))),
        Err(error) => Err(io::Error::new(
            error.kind(),
            format!("Could not run git status: {error}"),
        )),
    }
}

fn git_branch() -> io::Result<String> {
    if !Path::new(".git").exists() {
        return Ok("No branch".to_owned());
    }

    let output = Command::new("git")
        .args(["symbolic-ref", "--quiet", "--short", "HEAD"])
        .output()?;
    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
    } else if output.status.code() == Some(1) {
        Ok("Detached HEAD".to_owned())
    } else {
        Err(io::Error::other(format!(
            "git symbolic-ref failed ({}):\n{}",
            output.status,
            String::from_utf8_lossy(&output.stderr)
        )))
    }
}

fn display_directory(path: &Path, home: Option<&Path>) -> String {
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

fn render(
    frame: &mut ratatui::Frame,
    status: &Text<'static>,
    selection: &mut ListState,
    identity: Option<(&str, &str)>,
    message: Option<&str>,
    help: &mut help::Help,
) -> u16 {
    let [body, footer] = Layout::vertical([
        Constraint::Min(0),
        Constraint::Length(u16::from(identity.is_some()) + u16::from(message.is_some())),
    ])
    .areas(frame.area());
    let [body, help_area] = Layout::vertical([
        Constraint::Min(1),
        Constraint::Length(help.height(body.height)),
    ])
    .areas(body);
    frame.render_stateful_widget(status_list(status), body, selection);
    help.render(frame, help_area);
    let mut footer_lines = Vec::new();
    if let Some((cwd, branch)) = identity {
        footer_lines.push(Line::styled(
            format!("{cwd} • {branch}"),
            Style::default().fg(Color::Cyan),
        ));
    }
    if let Some(message) = message {
        let message: String = message
            .chars()
            .flat_map(|c| {
                if c.is_control() {
                    c.escape_default().collect::<Vec<_>>()
                } else {
                    vec![c]
                }
            })
            .collect();
        footer_lines.push(Line::styled(message, Style::default().fg(Color::Yellow)));
    }
    frame.render_widget(Paragraph::new(footer_lines), footer);
    body.height
}

fn status_list(status: &Text<'static>) -> List<'static> {
    List::new(
        status
            .lines
            .iter()
            .cloned()
            .map(ListItem::new)
            .collect::<Vec<_>>(),
    )
    .highlight_style(Style::default().add_modifier(Modifier::BOLD))
}

fn clamp_selection(state: &mut ListState, line_count: usize) {
    state.select(if line_count == 0 {
        None
    } else {
        Some(state.selected().unwrap_or(0).min(line_count - 1))
    });
}

fn move_selection(state: &mut ListState, line_count: usize, down: bool) {
    move_selection_by(state, line_count, down, 1);
}

fn move_selection_by(state: &mut ListState, line_count: usize, down: bool, amount: usize) {
    clamp_selection(state, line_count);
    if let Some(selected) = state.selected() {
        state.select(Some(if down {
            selected.saturating_add(amount).min(line_count - 1)
        } else {
            selected.saturating_sub(amount)
        }));
    }
}

/// Run the status TUI on an already-initialised terminal.
///
/// The `bindings` slice is usually obtained from [`config::load`].
pub fn run(
    terminal: &mut Terminal<CrosstermBackend<io::Stderr>>,
    bindings: &[Binding],
) -> io::Result<()> {
    run_mode(terminal, bindings, false)
}

/// Run status in the main screen, resizing to its contents on every update.
pub fn run_inline(
    terminal: &mut Terminal<CrosstermBackend<io::Stderr>>,
    bindings: &[Binding],
) -> io::Result<()> {
    let result = run_mode(terminal, bindings, true);
    if result.is_ok() {
        return result;
    }
    let area = terminal.get_frame().area();
    let cleanup = terminal
        .set_cursor_position((0, area.bottom().saturating_sub(1)))
        .and_then(|_| crossterm::execute!(terminal.backend_mut(), crossterm::style::Print("\r\n")));
    result.and(cleanup)
}

fn content_height(status: &Text<'_>, help: &help::Help, message: bool, rows: u16) -> u16 {
    status
        .lines
        .len()
        .max(1)
        .saturating_add(usize::from(help.height(u16::MAX)))
        .saturating_add(usize::from(message))
        .min(usize::from(rows.max(1))) as u16
}

fn run_mode(
    terminal: &mut Terminal<CrosstermBackend<io::Stderr>>,
    bindings: &[Binding],
    inline: bool,
) -> io::Result<()> {
    let cwd_path = std::env::current_dir()?;
    let home = std::env::var_os("HOME").map(std::path::PathBuf::from);
    let cwd = display_directory(&cwd_path, home.as_deref());
    let mut status = git_status()?;
    let mut branch = git_branch()?;
    let mut selection = ListState::default();
    clamp_selection(&mut selection, status.paths.len());
    let mut next_refresh = Instant::now() + REFRESH_INTERVAL;
    let mut message: Option<String> = None;
    let mut help = help::Help::new(bindings);
    let mut after_command = false;
    let mut previous_size = None;
    let max_height = if inline {
        gitglass_core::config::load_inline_max_height(
            gitglass_core::config::DefaultCommand::Status,
        )?
    } else {
        None
    };

    loop {
        if inline {
            let size = crossterm::terminal::size()?;
            let rows = max_height.map_or(size.1, |cap| cap.min(size.1));
            let height = content_height(&status.text, &help, message.is_some(), rows);
            if after_command
                || previous_size != Some(size)
                || terminal.get_frame().area().height != height
            {
                gitglass_core::resize_inline_terminal(terminal, height, after_command)?;
                after_command = false;
                previous_size = Some(size);
            }
        }
        let mut list_height = 0;
        terminal.draw(|frame| {
            list_height = render(
                frame,
                &status.text,
                &mut selection,
                if inline { None } else { Some((&cwd, &branch)) },
                message.as_deref(),
                &mut help,
            );
        })?;

        if event::poll(next_refresh.saturating_duration_since(Instant::now()))? {
            if let Event::Key(key) = event::read()? {
                if matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat) {
                    let normalized = config::normalize_key(key.code, key.modifiers);
                    if key.kind == KeyEventKind::Press {
                        message = None;
                    }
                    match normalized {
                        (KeyCode::Char('q'), KeyModifiers::NONE) => {
                            if inline {
                                gitglass_core::finish_inline_terminal(terminal, list_height)?;
                            }
                            return Ok(());
                        }
                        (KeyCode::Char('j'), KeyModifiers::NONE) => {
                            move_selection(&mut selection, status.paths.len(), true);
                        }
                        (KeyCode::Char('k'), KeyModifiers::NONE) => {
                            move_selection(&mut selection, status.paths.len(), false);
                        }
                        (KeyCode::Char(c @ ('d' | 'u')), KeyModifiers::CONTROL) => {
                            move_selection_by(
                                &mut selection,
                                status.paths.len(),
                                c == 'd',
                                usize::from((list_height / 2).max(1)),
                            );
                        }
                        (KeyCode::Char('?'), KeyModifiers::NONE)
                            if key.kind == KeyEventKind::Press =>
                        {
                            help.toggle();
                        }
                        _ if key.kind == KeyEventKind::Press => {
                            if let Some(binding) =
                                bindings.iter().find(|binding| binding.key == normalized)
                            {
                                let file = selection
                                    .selected()
                                    .and_then(|index| status.paths.get(index));
                                let execute = if inline {
                                    actions::execute_inline
                                } else {
                                    actions::execute
                                };
                                message = execute(
                                    terminal,
                                    binding,
                                    file.map(|p| p.as_path()),
                                    &cwd_path,
                                )?;
                                after_command = inline
                                    && binding.suspend
                                    && (!binding.requires_file || file.is_some());
                                next_refresh = Instant::now();
                            }
                        }
                        _ => {}
                    }
                }
            }
        }

        if Instant::now() >= next_refresh {
            status = git_status()?;
            branch = git_branch()?;
            clamp_selection(&mut selection, status.paths.len());
            next_refresh = Instant::now() + REFRESH_INTERVAL;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inline_height_tracks_contents_help_messages_and_terminal_size() {
        let mut help = help::Help::new(&[]);
        let short = Text::raw("first\nsecond");
        assert_eq!(content_height(&short, &help, false, 30), 3);
        assert_eq!(content_height(&short, &help, true, 30), 4);
        help.toggle();
        assert_eq!(content_height(&short, &help, false, 30), 6);
        assert_eq!(content_height(&short, &help, false, 5), 5);
        help.toggle();
        let long = Text::raw("row\n".repeat(100));
        assert_eq!(content_height(&long, &help, false, 30), 30);
        assert_eq!(content_height(&short, &help, false, 30), 3);
        assert_eq!(content_height(&Text::default(), &help, false, 30), 2);
        assert_eq!(content_height(&short, &help, false, 1), 1);
    }

    #[test]
    fn selection_moves_and_stops_at_boundaries() {
        let mut state = ListState::default();
        clamp_selection(&mut state, 3);
        assert_eq!(state.selected(), Some(0));
        move_selection(&mut state, 3, false);
        assert_eq!(state.selected(), Some(0));
        move_selection(&mut state, 3, true);
        assert_eq!(state.selected(), Some(1));
        move_selection(&mut state, 3, true);
        move_selection(&mut state, 3, true);
        assert_eq!(state.selected(), Some(2));
        move_selection(&mut state, 3, false);
        assert_eq!(state.selected(), Some(1));
    }

    #[test]
    fn half_page_movement_clamps_to_list_boundaries() {
        let mut state = ListState::default();
        move_selection_by(&mut state, 20, true, 5);
        assert_eq!(state.selected(), Some(5));
        move_selection_by(&mut state, 20, false, 5);
        assert_eq!(state.selected(), Some(0));
        move_selection_by(&mut state, 20, true, 50);
        assert_eq!(state.selected(), Some(19));
        move_selection_by(&mut state, 20, false, 50);
        assert_eq!(state.selected(), Some(0));
        move_selection_by(&mut state, 0, true, 5);
        assert_eq!(state.selected(), None);
        move_selection_by(&mut state, 1, true, 1);
        assert_eq!(state.selected(), Some(0));
    }

    #[test]
    fn selection_stays_valid_after_refresh() {
        let mut state = ListState::default().with_selected(Some(4));
        clamp_selection(&mut state, 6);
        assert_eq!(state.selected(), Some(4));
        clamp_selection(&mut state, 2);
        assert_eq!(state.selected(), Some(1));
        clamp_selection(&mut state, 0);
        assert_eq!(state.selected(), None);
        move_selection(&mut state, 0, true);
        assert_eq!(state.selected(), None);
        clamp_selection(&mut state, 1);
        assert_eq!(state.selected(), Some(0));
    }

    #[test]
    fn selected_row_is_bold_scrolls_into_view_and_preserves_git_colors() {
        use ratatui::{backend::TestBackend, Terminal};

        let text = status_text(b"first\nsecond\n\x1b[31mthird\x1b[m").unwrap();
        let mut terminal = Terminal::new(TestBackend::new(12, 2)).unwrap();
        let mut state = ListState::default().with_selected(Some(2));
        terminal
            .draw(|frame| {
                frame.render_stateful_widget(status_list(&text), frame.area(), &mut state);
            })
            .unwrap();
        let buffer = terminal.backend().buffer();
        assert_eq!(buffer[(0, 1)].symbol(), "t");
        assert_eq!(buffer[(0, 1)].fg, Color::LightRed);
        assert!(buffer[(0, 1)].modifier.contains(Modifier::BOLD));
        assert_eq!(buffer[(0, 1)].bg, Color::Reset);
        assert_eq!(buffer[(11, 1)].bg, Color::Reset);
        assert!(!buffer[(0, 0)].modifier.contains(Modifier::BOLD));
    }

    #[test]
    fn inline_omits_identity_without_leaving_a_blank_row() {
        use ratatui::{backend::TestBackend, Terminal};

        let status = Text::raw("first\nsecond");
        for message in [None, Some("Action failed")] {
            let mut help = help::Help::new(&[]);
            let height = content_height(&status, &help, message.is_some(), 30);
            let mut terminal = Terminal::new(TestBackend::new(30, height)).unwrap();
            terminal
                .draw(|frame| {
                    render(
                        frame,
                        &status,
                        &mut ListState::default(),
                        None,
                        message,
                        &mut help,
                    );
                })
                .unwrap();
            let buffer = terminal.backend().buffer();
            for (row, expected) in [(0, "first"), (1, "second"), (2, "? Show keybindings")]
                .into_iter()
                .chain(message.map(|message| (3, message)))
            {
                for (column, character) in expected.chars().enumerate() {
                    assert_eq!(buffer[(column as u16, row)].symbol(), character.to_string());
                }
            }
            assert_eq!(height, if message.is_some() { 4 } else { 3 });
        }
    }

    #[test]
    fn footer_is_pinned_below_scrolling_status() {
        use ratatui::{backend::TestBackend, Terminal};

        let status = Text::raw("first\nsecond\nthird\nfourth");
        let mut selection = ListState::default().with_selected(Some(3));
        let mut terminal = Terminal::new(TestBackend::new(30, 5)).unwrap();
        terminal
            .draw(|frame| {
                render(
                    frame,
                    &status,
                    &mut selection,
                    Some(("/work/project", "main")),
                    None,
                    &mut help::Help::new(&[]),
                );
            })
            .unwrap();
        let buffer = terminal.backend().buffer();
        for (row, expected) in [
            (2, "fourth"),
            (3, "? Show keybindings"),
            (4, "/work/project • main"),
        ] {
            for (column, character) in expected.chars().enumerate() {
                assert_eq!(buffer[(column as u16, row)].symbol(), character.to_string());
            }
        }
        assert!(!buffer[(0, 4)].modifier.contains(Modifier::BOLD));
        assert_eq!(buffer[(0, 4)].fg, Color::Cyan);
        assert!(!buffer[(0, 4)].modifier.contains(Modifier::DIM));
    }

    #[test]
    fn expanded_help_keeps_file_selection_and_footer() {
        use ratatui::{backend::TestBackend, Terminal};

        let status = Text::raw("first\nsecond\nthird");
        let mut selection = ListState::default().with_selected(Some(1));
        let mut help = help::Help::new(&[]);
        help.toggle();
        let mut terminal = Terminal::new(TestBackend::new(60, 12)).unwrap();
        terminal
            .draw(|frame| {
                render(
                    frame,
                    &status,
                    &mut selection,
                    Some(("/work", "main")),
                    None,
                    &mut help,
                );
            })
            .unwrap();
        assert_eq!(selection.selected(), Some(1));
        let buffer = terminal.backend().buffer();
        for (row, expected) in [
            (7, "? Hide keybindings"),
            (8, "q  quit"),
            (10, "ctrl-d/ctrl-u  half page down/up"),
            (11, "/work • main"),
        ] {
            for (column, character) in expected.chars().enumerate() {
                assert_eq!(buffer[(column as u16, row)].symbol(), character.to_string());
            }
        }
    }

    #[test]
    fn action_error_has_its_own_footer_line() {
        use ratatui::{backend::TestBackend, Terminal};

        let status = Text::raw("file");
        let mut selection = ListState::default();
        let mut terminal = Terminal::new(TestBackend::new(50, 4)).unwrap();
        terminal
            .draw(|frame| {
                render(
                    frame,
                    &status,
                    &mut selection,
                    Some(("/work", "main")),
                    Some("e (Edit): command failed\nnext"),
                    &mut help::Help::new(&[]),
                );
            })
            .unwrap();
        let buffer = terminal.backend().buffer();
        assert_eq!(buffer[(0, 3)].fg, Color::Yellow);
        for (row, expected) in [(2, "/work • main"), (3, r"e (Edit): command failed\nnext")] {
            for (column, character) in expected.chars().enumerate() {
                assert_eq!(buffer[(column as u16, row)].symbol(), character.to_string());
            }
        }
    }

    #[test]
    fn home_directory_shortening_respects_path_boundaries() {
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
        assert_eq!(display_directory(Path::new("/work/app"), None), "/work/app");
        assert_eq!(
            display_directory(Path::new("/work/app"), Some(Path::new(""))),
            "/work/app"
        );
        assert_eq!(
            display_directory(Path::new("/work/app"), Some(Path::new("relative"))),
            "/work/app"
        );
    }

    #[test]
    fn empty_status_shows_clean_message() {
        assert_eq!(status_text(b"").unwrap(), Text::raw("Working tree clean"));
    }

    #[test]
    fn plain_status_preserves_text() {
        assert_eq!(status_text(b"?? new.txt").unwrap(), Text::raw("?? new.txt"));
    }

    #[test]
    fn git_colors_become_styled_spans() {
        let text = status_text(b"\x1b[32mM\x1b[m\x1b[31mM\x1b[m file.txt").unwrap();
        let spans = &text.lines[0].spans;
        assert_eq!(spans[0].content, "M");
        assert_eq!(spans[0].style.fg, Some(Color::Green));
        assert_eq!(spans[1].content, "M");
        assert_eq!(spans[1].style.fg, Some(Color::LightRed));
        assert_eq!(spans[2].content, " file.txt");
        assert_ne!(spans[2].style.fg, Some(Color::LightRed));
    }
}
