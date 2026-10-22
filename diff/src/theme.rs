use ratatui::style::Color;

#[derive(Clone, Copy, Debug)]
pub struct Theme {
    pub selection_bg: Color,
    pub cursor_bg: Color,
    pub removed_color: Color,
    pub line_number_color: Option<Color>,
}

impl Default for Theme {
    fn default() -> Self {
        Self {
            selection_bg: Color::Rgb(64, 64, 64),
            cursor_bg: Color::Rgb(80, 80, 80),
            removed_color: Color::Red,
            line_number_color: None,
        }
    }
}
