use ratatui::{
    style::{Color, Style},
    text::{Line, Span},
};

/// Style a single key followed by its description, preserving spacing.
pub fn hint_row(text: &str) -> Line<'_> {
    let end = text.find(char::is_whitespace).unwrap_or(text.len());
    Line::from(vec![
        Span::styled(&text[..end], Style::default().fg(Color::Gray)),
        Span::styled(&text[end..], Style::default().fg(Color::DarkGray)),
    ])
}

/// Style compact hints separated by double spaces or bullets.
pub fn hint_line(text: &str) -> Line<'_> {
    let bullets = text.contains(" • ");
    let separator = if bullets { " • " } else { "  " };
    let mut spans = Vec::new();
    let mut description = false;
    for (index, hint) in text.split(separator).enumerate() {
        if index > 0 {
            spans.push(Span::styled(
                separator,
                Style::default().fg(Color::DarkGray),
            ));
        }
        if description {
            spans.push(Span::styled(hint, Style::default().fg(Color::DarkGray)));
            description = false;
        } else if let Some((key, action)) = hint.split_once(": ") {
            spans.push(Span::styled(key, Style::default().fg(Color::Gray)));
            spans.push(Span::styled(": ", Style::default().fg(Color::DarkGray)));
            spans.push(Span::styled(action, Style::default().fg(Color::DarkGray)));
        } else if bullets {
            spans.extend(hint_row(hint).spans);
        } else if crate::config::parse_key(hint).is_ok() {
            spans.push(Span::styled(hint, Style::default().fg(Color::Gray)));
            description = true;
        } else {
            spans.push(Span::styled(hint, Style::default().fg(Color::DarkGray)));
        }
    }
    Line::from(spans)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_are_light_gray_and_actions_are_dark_gray() {
        for text in ["q: quit  j/k: move", "q quit • j/k move", "q  quit"] {
            let line = hint_line(text);
            assert_eq!(line.to_string(), text);
            assert_eq!(line.spans[0].style.fg, Some(Color::Gray));
            assert_eq!(line.spans.last().unwrap().style.fg, Some(Color::DarkGray));
        }
        let row = hint_row("ctrl-d/ctrl-u  half page down/up");
        assert_eq!(row.spans[0].style.fg, Some(Color::Gray));
        assert_eq!(row.spans[1].style.fg, Some(Color::DarkGray));
    }

    #[test]
    fn prose_and_search_queries_stay_gray() {
        let line = hint_line("/query  No search results  Esc: clear");
        assert_eq!(line.spans[0].style.fg, Some(Color::DarkGray));
        assert_eq!(line.to_string(), "/query  No search results  Esc: clear");
    }
}
