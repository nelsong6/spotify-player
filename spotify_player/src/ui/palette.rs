//! The command palette, drawn over the page from what fzt returns: its prompt and the menu rows.

use super::{
    utils::construct_and_render_block, Borders, Constraint, Frame, Layout, Line, Modifier,
    Paragraph, Rect, Span, Style, UIStateGuard, Wrap,
};
use crate::{
    fzt,
    state::{CommandPalette, PopupState},
};
use ratatui::{layout::Flex, widgets::Clear};

pub fn render(frame: &mut Frame, ui: &UIStateGuard, rect: Rect) {
    let Some(PopupState::CommandPalette(palette)) = &ui.popup else {
        return;
    };
    let [area] = Layout::horizontal([Constraint::Percentage(70)])
        .flex(Flex::Center)
        .areas(rect);
    let [area] = Layout::vertical([Constraint::Percentage(70)])
        .flex(Flex::Center)
        .areas(area);
    frame.render_widget(Clear, area);
    let inner = construct_and_render_block("Command palette", &ui.theme, Borders::ALL, frame, area);
    match palette.as_ref() {
        CommandPalette::Failed(err) => frame.render_widget(
            Paragraph::new(format!("{err}\n\nPress any key to close.")).wrap(Wrap { trim: false }),
            inner,
        ),
        CommandPalette::Open { frame: fzt, .. } | CommandPalette::Shown(fzt) => {
            render_open(frame, ui, fzt, inner);
        }
    }
}

fn render_open(frame: &mut Frame, ui: &UIStateGuard, fzt: &fzt::Frame, rect: Rect) {
    let [prompt_rect, _, rows_rect] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Fill(0),
    ])
    .areas(rect);
    let dim = Style::default().add_modifier(Modifier::DIM);
    let match_style = ui.theme.block_title().add_modifier(Modifier::BOLD);

    let prompt = &fzt.prompt;
    let mut spans = vec![Span::styled("› ", ui.theme.block_title())];
    for scope in prompt.scope_path.iter().flatten() {
        spans.push(Span::styled(format!("{scope} › "), dim));
    }
    if prompt.query.is_empty() {
        spans.push(Span::styled(prompt.hint.clone(), dim));
    } else {
        spans.push(Span::raw(prompt.query.clone()));
        spans.push(Span::styled(prompt.ghost.clone(), dim));
    }
    frame.render_widget(Line::from(spans), prompt_rect);

    let name_width = fzt
        .rows
        .iter()
        .map(|r| r.depth * 2 + 2 + r.name.chars().count())
        .max()
        .unwrap_or(0)
        + 2;
    let height = usize::from(rows_rect.height);
    let selected = fzt.rows.iter().position(|r| r.is_selected).unwrap_or(0);
    let offset = (selected + 1).saturating_sub(height);
    let lines: Vec<Line> = fzt
        .rows
        .iter()
        .skip(offset)
        .take(height)
        .map(|row| {
            let mut spans = vec![Span::raw(format!(
                "{}{}",
                "  ".repeat(row.depth),
                if row.is_folder { "▸ " } else { "  " }
            ))];
            spans.extend(highlight(
                &row.name,
                row.name_match_indices.as_deref(),
                match_style,
            ));
            let used = row.depth * 2 + 2 + row.name.chars().count();
            spans.push(Span::raw(" ".repeat(name_width.saturating_sub(used))));
            spans.extend(
                highlight(
                    &row.description,
                    row.desc_match_indices.as_deref(),
                    match_style,
                )
                .into_iter()
                .map(|span| span.patch_style(ui.theme.playlist_desc())),
            );
            let line = Line::from(spans);
            if row.is_selected {
                line.style(ui.theme.selection(true))
            } else {
                line
            }
        })
        .collect();
    frame.render_widget(Paragraph::new(lines), rows_rect);
}

/// `text` as spans, with the characters at `indices` (fzt's matches) in `style`
fn highlight(text: &str, indices: Option<&[usize]>, style: Style) -> Vec<Span<'static>> {
    let indices = indices.unwrap_or_default();
    text.chars()
        .enumerate()
        .map(|(i, c)| {
            if indices.contains(&i) {
                Span::styled(c.to_string(), style)
            } else {
                Span::raw(c.to_string())
            }
        })
        .collect()
}
