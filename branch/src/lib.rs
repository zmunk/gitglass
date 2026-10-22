//! Interactive branch list and safe branch switching.
use std::{
    io,
    path::Path,
    process::Command,
    time::{Duration, Instant},
};

use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use gitglass_core::{
    actions,
    config::{self, Binding},
};
use ratatui::{
    backend::CrosstermBackend,
    layout::{Constraint, Layout},
    style::{Color, Modifier, Style},
    text::Line,
    widgets::{List, ListItem, ListState, Paragraph},
    Terminal,
};

const REFRESH_INTERVAL: Duration = Duration::from_secs(2);

#[derive(Debug)]
struct Branch {
    reference: String,
    current: bool,
}

impl Branch {
    fn can_switch(&self) -> bool {
        !self.current && !self.is_remote()
    }

    fn is_remote(&self) -> bool {
        self.reference.starts_with("refs/remotes/")
    }

    fn name(&self) -> &str {
        self.reference
            .strip_prefix("refs/heads/")
            .or_else(|| self.reference.strip_prefix("refs/remotes/"))
            .unwrap_or(&self.reference)
    }
}

fn git(cwd: &Path, args: &[&str]) -> io::Result<Vec<u8>> {
    let output = Command::new("git").current_dir(cwd).args(args).output()?;
    if !output.status.success() {
        return Err(io::Error::other(format!(
            "git {} failed: {}",
            args[0],
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }
    Ok(output.stdout)
}

fn load_branches(cwd: &Path) -> io::Result<Vec<Branch>> {
    // Match the log menu's recency and tie ordering within each branch group.
    let output = git(
        cwd,
        &[
            "--no-pager",
            "branch",
            "--all",
            "--color=never",
            "--sort=-committerdate",
            "--format=%(HEAD) %(refname)",
        ],
    )?;
    let mut branches: Vec<Branch> = String::from_utf8_lossy(&output)
        .lines()
        .filter_map(|line| {
            let reference = line.get(2..)?;
            // git branch also emits a synthetic detached-HEAD entry; it is not a branch.
            reference.starts_with("refs/").then(|| Branch {
                reference: reference.to_owned(),
                current: line.starts_with("* "),
            })
        })
        .collect();
    // Stable sorting keeps Git's most-recent-first order within each group.
    branches.sort_by_key(Branch::is_remote);
    Ok(branches)
}

fn switch_branch(cwd: &Path, branch: &Branch) -> io::Result<()> {
    if branch.current {
        return Ok(());
    }
    if branch.is_remote() {
        return Err(io::Error::other(
            "Remote branch — switch to a local branch instead",
        ));
    }
    // Include untracked files and dirty submodules, independent of user config.
    let status = git(
        cwd,
        &[
            "status",
            "--porcelain=v1",
            "-z",
            "--untracked-files=all",
            "--ignore-submodules=none",
        ],
    )?;
    if !status.is_empty() {
        return Err(io::Error::other(
            "Cannot switch branch: uncommitted changes exist (including untracked files)",
        ));
    }
    git(cwd, &["switch", "--no-guess", "--", branch.name()])?;
    Ok(())
}

fn clamp(state: &mut ListState, len: usize) {
    state.select(if len == 0 {
        None
    } else {
        Some(state.selected().unwrap_or(0).min(len - 1))
    });
}

fn navigate(state: &mut ListState, len: usize, down: bool, amount: usize) {
    clamp(state, len);
    if let Some(index) = state.selected() {
        state.select(Some(if down {
            index.saturating_add(amount).min(len - 1)
        } else {
            index.saturating_sub(amount)
        }));
    }
}

fn refresh(cwd: &Path, branches: &mut Vec<Branch>, selection: &mut ListState) -> io::Result<()> {
    let reference = selection
        .selected()
        .and_then(|i| branches.get(i))
        .map(|b| b.reference.clone());
    *branches = load_branches(cwd)?;
    if let Some(index) = reference.and_then(|r| branches.iter().position(|b| b.reference == r)) {
        selection.select(Some(index));
    }
    clamp(selection, branches.len());
    Ok(())
}

fn safe_text(text: &str) -> String {
    text.chars()
        .flat_map(|c| {
            if c.is_control() {
                c.escape_default().collect::<Vec<_>>()
            } else {
                vec![c]
            }
        })
        .collect()
}

fn compact_hints(
    selected: Option<&Branch>,
    branch_count: usize,
    list_height: u16,
    bindings: &[Binding],
) -> String {
    let mut hints = vec!["q quit", "j/k move"];
    if selected.is_some_and(Branch::can_switch) {
        hints.push("space switch");
    }
    if branch_count > usize::from(list_height) {
        hints.push("ctrl-u/d half page");
    }
    hints
        .into_iter()
        .map(str::to_owned)
        .chain(bindings.iter().map(|binding| safe_text(&binding.help_text)))
        .collect::<Vec<_>>()
        .join(" • ")
}

/// Run on an already-initialised terminal.
pub fn run(
    terminal: &mut Terminal<CrosstermBackend<io::Stderr>>,
    bindings: &[Binding],
) -> io::Result<()> {
    let cwd = std::env::current_dir()?;
    let mut branches = load_branches(&cwd)?;
    let mut selection = ListState::default();
    clamp(&mut selection, branches.len());
    let mut message: Option<String> = None;
    let mut expanded = false;
    let mut next_refresh = Instant::now() + REFRESH_INTERVAL;
    loop {
        let mut height = 0;
        terminal.draw(|frame| {
            let mut help = if expanded {
                std::iter::once("? Hide keybindings".to_owned())
                    .chain(
                        config::BRANCH_BUILTINS
                            .iter()
                            .map(|(key, description)| format!("{key}  {description}")),
                    )
                    .chain(bindings.iter().map(|b| safe_text(&b.help_text)))
                    .collect::<Vec<_>>()
                    .join("\n")
            } else {
                "q quit • j/k move".to_owned()
            };
            let help_height =
                (help.lines().count() as u16).min(frame.area().height.saturating_sub(2));
            let [body, help_area, footer] = Layout::vertical([
                Constraint::Min(0),
                Constraint::Length(help_height),
                Constraint::Length(if message.is_some() { 2 } else { 1 }),
            ])
            .areas(frame.area());
            height = body.height;
            if !expanded {
                help = compact_hints(
                    selection.selected().and_then(|index| branches.get(index)),
                    branches.len(),
                    height,
                    bindings,
                );
            }
            if branches.is_empty() {
                frame.render_widget(Paragraph::new("No branches."), body);
            } else {
                let items = branches.iter().map(|branch| {
                    let item = ListItem::new(format!(
                        "{} {}",
                        if branch.current { "*" } else { " " },
                        branch.name()
                    ));
                    if branch.is_remote() {
                        item.style(Style::default().fg(Color::DarkGray))
                    } else if branch.current {
                        item.style(Style::default().fg(Color::Green))
                    } else {
                        item
                    }
                });
                frame.render_stateful_widget(
                    List::new(items)
                        .highlight_style(Style::default().add_modifier(Modifier::BOLD))
                        .highlight_symbol("> "),
                    body,
                    &mut selection,
                );
            }
            frame.render_widget(
                Paragraph::new(help).style(Style::default().fg(Color::DarkGray)),
                help_area,
            );
            let mut footer_lines = vec![Line::styled(
                format!("{} • branches", cwd.display()),
                Style::default().fg(Color::Cyan).add_modifier(Modifier::DIM),
            )];
            if let Some(message) = &message {
                footer_lines.push(Line::styled(
                    safe_text(message),
                    Style::default().fg(Color::Yellow),
                ));
            }
            frame.render_widget(Paragraph::new(footer_lines), footer);
        })?;

        if event::poll(next_refresh.saturating_duration_since(Instant::now()))? {
            if let Event::Key(key) = event::read()? {
                if matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat) {
                    let normalized = config::normalize_key(key.code, key.modifiers);
                    // Disabled Space is a no-op, including preserving existing feedback.
                    if normalized == (KeyCode::Char(' '), KeyModifiers::NONE)
                        && selection
                            .selected()
                            .and_then(|index| branches.get(index))
                            .map_or(true, |branch| !branch.can_switch())
                    {
                        continue;
                    }
                    if key.kind == KeyEventKind::Press {
                        message = None;
                    }
                    match normalized {
                        (KeyCode::Char('q') | KeyCode::Esc, KeyModifiers::NONE)
                        | (KeyCode::Char('c'), KeyModifiers::CONTROL) => return Ok(()),
                        (KeyCode::Char('j') | KeyCode::Down, KeyModifiers::NONE) => {
                            navigate(&mut selection, branches.len(), true, 1)
                        }
                        (KeyCode::Char('k') | KeyCode::Up, KeyModifiers::NONE) => {
                            navigate(&mut selection, branches.len(), false, 1)
                        }
                        (KeyCode::Char(c @ ('d' | 'u')), KeyModifiers::CONTROL) => navigate(
                            &mut selection,
                            branches.len(),
                            c == 'd',
                            usize::from((height / 2).max(1)),
                        ),
                        (KeyCode::Char('?'), KeyModifiers::NONE)
                            if key.kind == KeyEventKind::Press =>
                        {
                            expanded = !expanded
                        }
                        (KeyCode::Char(' '), KeyModifiers::NONE)
                            if key.kind == KeyEventKind::Press =>
                        {
                            if let Some(branch) = selection.selected().and_then(|i| branches.get(i))
                            {
                                message = Some(match switch_branch(&cwd, branch) {
                                    Ok(()) => format!("Switched to {}", branch.name()),
                                    Err(error) => error.to_string(),
                                });
                                next_refresh = Instant::now();
                            }
                        }
                        _ if key.kind == KeyEventKind::Press => {
                            if let Some(binding) = bindings.iter().find(|b| b.key == normalized) {
                                let branch = selection.selected().and_then(|i| branches.get(i));
                                message = actions::execute_branch(
                                    terminal,
                                    binding,
                                    branch.map(Branch::name),
                                    &cwd,
                                )?;
                                next_refresh = Instant::now();
                            }
                        }
                        _ => {}
                    }
                }
            }
        }
        if Instant::now() >= next_refresh {
            match refresh(&cwd, &mut branches, &mut selection) {
                Ok(()) => {}
                Err(error) => message = Some(error.to_string()),
            }
            next_refresh = Instant::now() + REFRESH_INTERVAL;
        }
    }
}

#[cfg(test)]
mod tests;
