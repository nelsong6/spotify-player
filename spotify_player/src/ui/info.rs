//! The info panel, drawn over the page as plain text so the terminal can select it.

use super::{
    utils::construct_and_render_block, Borders, Constraint, Frame, Layout, Line, Modifier,
    Paragraph, Rect, Style, UIStateGuard, Wrap,
};
use crate::state::PopupState;
use ratatui::{layout::Flex, widgets::Clear};

const HINT: &str = "Drag to select text. Press any key to close.";
/// The panel's borders, top and bottom or left and right
const BORDERS: u16 = 2;

pub fn render(frame: &mut Frame, ui: &UIStateGuard, rect: Rect) {
    let Some(PopupState::Info(lines)) = &ui.popup else {
        return;
    };
    let text = text(lines);
    let area = panel_area(rect, &text);
    frame.render_widget(Clear, area);
    let inner = construct_and_render_block("Info", &ui.theme, Borders::ALL, frame, area);
    frame.render_widget(paragraph(text), inner);
}

/// Each line with its value lined up under the others, then how to use the panel
fn text(lines: &[(&'static str, String)]) -> Vec<String> {
    let width = lines
        .iter()
        .map(|(label, _)| label.len())
        .max()
        .unwrap_or(0)
        + 2;
    let mut text: Vec<String> = lines
        .iter()
        .map(|(label, value)| format!("{label:<width$}{value}"))
        .collect();
    text.push(String::new());
    text.push(HINT.to_string());
    text
}

fn paragraph(text: Vec<String>) -> Paragraph<'static> {
    let dim = Style::default().add_modifier(Modifier::DIM);
    let lines: Vec<Line> = text
        .into_iter()
        .map(|row| {
            if row == HINT {
                Line::styled(row, dim)
            } else {
                Line::raw(row)
            }
        })
        .collect();
    Paragraph::new(lines).wrap(Wrap { trim: false })
}

/// Wide enough that a value stays on one row, which is how it copies whole; a narrower terminal
/// wraps it
fn panel_area(rect: Rect, text: &[String]) -> Rect {
    let widest = text.iter().map(|row| Line::raw(row).width()).max();
    let width = (widest.unwrap_or(0) as u16 + BORDERS).min(rect.width);
    let inner = usize::from(width.saturating_sub(BORDERS)).max(1);
    let height: usize = text
        .iter()
        .map(|row| {
            let rows = Line::raw(row).width().div_ceil(inner).max(1);
            // wrapping breaks at a space where it can, which can take one more row
            if rows > 1 {
                rows + 1
            } else {
                rows
            }
        })
        .sum();
    let height = (height as u16).saturating_add(BORDERS).min(rect.height);

    let [area] = Layout::horizontal([Constraint::Length(width)])
        .flex(Flex::Center)
        .areas(rect);
    let [area] = Layout::vertical([Constraint::Length(height)])
        .flex(Flex::Center)
        .areas(area);
    area
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::{backend::TestBackend, layout::Margin, Terminal};

    const PATH: &str = r"C:\Program Files\at-home\spt\v0.25.1-109-gabcdef1\spotify_player.exe";

    fn drawn(columns: u16, height: u16) -> String {
        let text = text(&[
            ("Repo", "nelsong6/spotify-player".to_string()),
            ("Installed", PATH.to_string()),
        ]);
        let mut terminal = Terminal::new(TestBackend::new(columns, height)).unwrap();
        terminal
            .draw(|frame| {
                let area = panel_area(frame.area(), &text);
                let inner = area.inner(Margin::new(1, 1));
                frame.render_widget(paragraph(text.clone()), inner);
            })
            .unwrap();
        crate::ui::screen_text(terminal.backend().buffer()).join("\n")
    }

    #[test]
    fn a_value_stays_on_one_row_when_the_terminal_has_room() {
        let screen = drawn(120, 30);
        assert!(screen.contains("Repo       nelsong6/spotify-player"));
        assert!(screen.contains(&format!("Installed  {PATH}")));
        assert!(screen.contains(HINT));
    }

    #[test]
    fn a_narrow_terminal_wraps_the_value_instead_of_cutting_it() {
        let screen = drawn(44, 16);
        let letters: String = screen.chars().filter(|c| !c.is_whitespace()).collect();
        assert!(letters.contains(&PATH.replace(' ', "")), "{screen}");
        assert!(letters.contains(&HINT.replace(' ', "")), "{screen}");
    }

    #[test]
    fn a_short_terminal_still_draws_within_its_bounds() {
        assert!(drawn(30, 4).contains("Repo"));
    }
}
