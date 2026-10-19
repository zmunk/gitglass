use ratatui::style::Color;

#[derive(Clone, Copy, Debug)]
pub struct Theme {
    pub selection_bg: Color,
    pub cursor_bg: Color,
}

impl Default for Theme {
    fn default() -> Self {
        Self {
            selection_bg: Color::Rgb(64, 64, 64),
            cursor_bg: Color::Rgb(80, 80, 80),
        }
    }
}
