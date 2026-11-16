use ratatui::{layout::Rect, style::Color, widgets::Paragraph, Frame};

use crate::config::Binding;

pub struct Help {
    pub expanded: bool,
    pub background: Color,
    rows: Vec<String>,
}

impl Help {
    pub fn new(bindings: &[Binding]) -> Self {
        let rows = [
            "q  quit".to_owned(),
            "j/k  move down/up".to_owned(),
            "ctrl-d/ctrl-u  half page down/up".to_owned(),
        ]
        .into_iter()
        .chain(bindings.iter().map(|binding| binding.help_text.clone()))
        .map(|row| {
            row.chars()
                .flat_map(|c| {
                    if c.is_control() {
                        c.escape_default().collect::<Vec<_>>()
                    } else {
                        vec![c]
                    }
                })
                .collect()
        })
        .collect();
        Self {
            expanded: false,
            background: Color::Rgb(56, 56, 56),
            rows,
        }
    }

    pub fn toggle(&mut self) {
        self.expanded = !self.expanded;
    }

    // Allocate enough space for the list, keeping at least one status row.
    pub fn height(&self, available: u16) -> u16 {
        if available <= 1 {
            return 0;
        }
        if !self.expanded {
            return 1;
        }
        self.rows
            .len()
            .saturating_add(1)
            .min(usize::from(available - 1)) as u16
    }

    pub fn render(&self, frame: &mut Frame, area: Rect) {
        let text = if self.expanded {
            let mut lines = vec!["? Hide keybindings".to_owned()];
            lines.extend(self.rows.iter().cloned());
            lines.join("\n")
        } else {
            "? Show keybindings".to_owned()
        };
        let lines: Vec<_> = text
            .lines()
            .enumerate()
            .map(|(index, text)| {
                let mut line = gitglass_core::hints::hint_row(text);
                if index == 0 {
                    line.spans.insert(0, "  ".into());
                    line.spans.push("  ".into());
                    line.style(ratatui::style::Style::default().bg(self.background))
                } else {
                    line
                }
            })
            .collect();
        frame.render_widget(Paragraph::new(lines), area);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::parse_config;
    use ratatui::{backend::TestBackend, style::Color, Terminal};

    #[test]
    fn collapsed_by_default_and_includes_compact_bindings() {
        let bindings = parse_config("[[status.keybindings]]\nkey='e'\ncommand='editor'\ndescription='Edit file'\n[[status.keybindings]]\nkey='s'\ncommand='git status'").unwrap();
        let mut help = Help::new(&bindings);
        assert!(!help.expanded);
        assert_eq!(help.height(20), 1);
        assert_eq!(
            help.rows,
            [
                "q  quit",
                "j/k  move down/up",
                "ctrl-d/ctrl-u  half page down/up",
                "e  Edit file",
                "s  git status"
            ]
        );
        help.toggle();
        assert!(help.expanded);
        assert_eq!(help.height(20), (help.rows.len() + 1) as u16);
        help.toggle();
        assert!(!help.expanded);
    }

    #[test]
    fn renders_light_grey_keys_and_dark_grey_actions() {
        let mut help = Help::new(&[]);
        let mut terminal = Terminal::new(TestBackend::new(40, 5)).unwrap();
        for expanded in [false, true] {
            help.expanded = expanded;
            help.background = Color::Rgb(40, 40, 40);
            terminal
                .draw(|frame| {
                    let area = frame.area();
                    help.render(frame, area);
                })
                .unwrap();
            let buffer = terminal.backend().buffer();
            for column in 0..40 {
                assert_eq!(buffer[(column, 0)].bg, Color::Rgb(40, 40, 40));
                assert_eq!(buffer[(column, 1)].bg, Color::Reset);
            }
            let header = if expanded {
                "? Hide keybindings"
            } else {
                "? Show keybindings"
            };
            for (column, character) in header.chars().enumerate() {
                assert_eq!(
                    buffer[(column as u16 + 2, 0)].symbol(),
                    character.to_string()
                );
                assert_eq!(
                    buffer[(column as u16 + 2, 0)].fg,
                    if column == 0 {
                        Color::Gray
                    } else {
                        Color::DarkGray
                    }
                );
            }
            for column in [0, 1, header.len() as u16 + 2, header.len() as u16 + 3] {
                assert_eq!(buffer[(column, 0)].symbol(), " ");
            }
            if expanded {
                assert_eq!(buffer[(0, 1)].symbol(), "q");
                assert_eq!(buffer[(0, 2)].symbol(), "j");
                assert_eq!(buffer[(0, 2)].fg, Color::Gray);
                assert_eq!(buffer[(5, 2)].fg, Color::DarkGray);
                assert_eq!(buffer[(0, 3)].symbol(), "c");
            }
        }
    }

    #[test]
    fn small_terminals_and_control_characters_are_safe() {
        let bindings = parse_config(
            "[[status.keybindings]]\nkey='e'\ncommand='true'\ndescription=\"Edit\\nfile\"",
        )
        .unwrap();
        let mut help = Help::new(&bindings);
        assert!(help.rows.contains(&r"e  Edit\nfile".to_owned()));
        help.toggle();
        assert_eq!(help.height(0), 0);
        assert_eq!(help.height(1), 0);
        assert_eq!(help.height(2), 1);
        let mut terminal = Terminal::new(TestBackend::new(1, 1)).unwrap();
        terminal
            .draw(|frame| help.render(frame, Rect::new(0, 0, 0, 0)))
            .unwrap();
    }
}
