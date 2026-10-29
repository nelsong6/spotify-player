use crate::{
    config,
    state::{
        Album, Artist, ArtistFocusState, BrowsePageUIState, Context, ContextPageUIState,
        DataReadGuard, Id, LibraryFocusState, MutableWindowState, Orientation, PageState, PageType,
        PlaybackMetadata, PlaylistCreateCurrentField, PlaylistFolderItem, PlaylistPopupAction,
        PopupState, SearchFocusState, SharedState, Track, UIStateGuard,
    },
};
use anyhow::{Context as AnyhowContext, Result};
use ratatui::{
    layout::{Alignment, Constraint, Layout, Rect},
    style::{Modifier, Style},
    text::{Line, Span, Text},
    widgets::{
        Block, BorderType, Borders, Cell, Gauge, LineGauge, List, ListItem, ListState, Paragraph,
        Row, Table, TableState, Wrap,
    },
    Frame,
};

#[cfg(feature = "image")]
use crate::state::ImageRenderInfo;

type Terminal = ratatui::Terminal<ratatui::backend::CrosstermBackend<std::io::Stdout>>;

#[cfg(feature = "ascii-cover")]
pub mod ascii_cover;
#[cfg(feature = "image")]
pub mod cover_image;
mod page;
mod palette;
mod playback;
mod popup;
mod problems;
#[cfg(feature = "cast")]
pub mod streaming;
pub mod utils;

/// Run the application UI
pub fn run(state: &SharedState, mut terminal: Terminal) -> Result<()> {
    let ui_refresh_duration = std::time::Duration::from_millis(
        config::get_config().app_config.app_refresh_duration_in_ms,
    );
    let mut last_terminal_size = None;
    let mut mouse_captured = true;

    loop {
        {
            let mut ui = state.ui.lock();
            if !ui.is_running {
                clean_up(terminal).context("clean up UI resources")?;
                std::process::exit(0);
            }

            if ui.mouse_capture != mouse_captured {
                mouse_captured = ui.mouse_capture;
                if mouse_captured {
                    crossterm::execute!(
                        terminal.backend_mut(),
                        crossterm::event::EnableMouseCapture
                    )?;
                } else {
                    crossterm::execute!(
                        terminal.backend_mut(),
                        crossterm::event::DisableMouseCapture
                    )?;
                }
            }

            let terminal_size = terminal.size()?;
            if Some(terminal_size) != last_terminal_size {
                last_terminal_size = Some(terminal_size);
                #[cfg(feature = "image")]
                {
                    // redraw the cover image when the terminal's size changes
                    ui.last_cover_image_render_info = ImageRenderInfo::default();
                }
            }

            match terminal.draw(|frame| {
                // set the background and foreground colors for the application
                let rect = frame.area();
                let block = Block::default().style(ui.theme.app());
                frame.render_widget(block, rect);

                render_application(frame, state, &mut ui, rect);
            }) {
                Ok(frame) => *state.screen.lock() = screen_text(frame.buffer),
                Err(err) => tracing::error!("Failed to render the application: {err:#}"),
            }
        }

        std::thread::sleep(ui_refresh_duration);
    }
}

pub fn init_terminal() -> Result<Terminal> {
    let mut stdout = std::io::stdout();
    crossterm::terminal::enable_raw_mode()?;
    crossterm::execute!(
        stdout,
        crossterm::terminal::EnterAlternateScreen,
        crossterm::event::EnableMouseCapture
    )?;
    let backend = ratatui::backend::CrosstermBackend::new(stdout);
    let mut terminal = ratatui::Terminal::new(backend)?;
    terminal.clear()?;
    Ok(terminal)
}

#[cfg(feature = "image")]
pub fn init_image_picker(state: &SharedState) -> Result<()> {
    let mut ui = state.ui.lock();
    crossterm::terminal::enable_raw_mode()?;
    ui.picker = match ratatui_image::picker::Picker::from_query_stdio() {
        Ok(p) => p,
        Err(err) => {
            tracing::warn!("Failed to initialize query_stdio picker, error: {err:#}");
            ratatui_image::picker::Picker::halfblocks()
        }
    };
    crossterm::terminal::disable_raw_mode()?;

    // ratatui_image might detect the wrong protocol for iTerm2, so override it to the native iTerm2 protocol if detected
    // https://github.com/ratatui/ratatui-image/issues/158
    if is_iterm2() && ui.picker.protocol_type() != ratatui_image::picker::ProtocolType::Iterm2 {
        ui.picker
            .set_protocol_type(ratatui_image::picker::ProtocolType::Iterm2);
        tracing::info!("Detected iTerm2; overriding image protocol to native iTerm2");
    }
    tracing::info!("Image protocol: {:?}", ui.picker.protocol_type());
    Ok(())
}

/// Whether the application is running inside iTerm2.
///
/// iTerm2 sets `TERM_PROGRAM=iTerm.app` locally and forwards `LC_TERMINAL=iTerm2`
/// over SSH, so checking both covers the common cases.
#[cfg(feature = "image")]
fn is_iterm2() -> bool {
    std::env::var("TERM_PROGRAM").is_ok_and(|v| v == "iTerm.app")
        || std::env::var("LC_TERMINAL").is_ok_and(|v| v.eq_ignore_ascii_case("iTerm2"))
}

/// Clean up UI resources before quitting the application
fn clean_up(mut terminal: Terminal) -> Result<()> {
    crossterm::terminal::disable_raw_mode()?;
    crossterm::execute!(
        terminal.backend_mut(),
        crossterm::terminal::LeaveAlternateScreen,
        crossterm::event::DisableMouseCapture,
    )?;
    terminal.show_cursor()?;
    Ok(())
}

/// Render the application
fn render_application(frame: &mut Frame, state: &SharedState, ui: &mut UIStateGuard, rect: Rect) {
    // what's wrong takes the top row, above everything else, while anything is
    let shown = problems::shown(state);
    let rect = if shown.is_empty() {
        rect
    } else {
        let [row, rest] =
            Layout::vertical([Constraint::Length(1), Constraint::Fill(1)]).areas(rect);
        problems::render(frame, &shown, row);
        rest
    };

    // rendering order: playback window -> shortcut help popup -> other popups -> main layout

    // render playback window before other popups and windows to ensure nothing is rendered on top
    // of the playback window, which is to avoid "duplicated images" issue
    // See: https://github.com/aome510/spotify-player/issues/498
    let rect = if visualization_shown(state) {
        // the visualization carries the playback itself, in its caption and transport line
        #[cfg(feature = "image")]
        {
            ui.last_cover_image_render_info = ImageRenderInfo::default();
        }
        rect
    } else {
        playback::render_playback_window(frame, state, ui, rect)
    };
    let page_rect = rect;

    let rect = popup::render_shortcut_help_popup(frame, ui, rect);

    let (rect, is_active) = popup::render_popup(frame, state, ui, rect);

    render_main_layout(is_active, frame, state, ui, rect);

    // drawn over the page, so after it
    palette::render(frame, ui, page_rect);
    #[cfg(feature = "cast")]
    if crate::screen::pc::is_locked() {
        render_lock(frame, page_rect);
    }
}

/// Mark the screen as another PC's, at the right of the page's top border
#[cfg(feature = "cast")]
fn render_lock(frame: &mut Frame, rect: Rect) {
    let lock = Line::from(" locked: space to take control ").style(
        Style::default()
            .fg(ratatui::style::Color::Yellow)
            .add_modifier(Modifier::BOLD),
    );
    let width = (lock.width() as u16).min(rect.width.saturating_sub(2));
    let area = Rect::new(rect.right().saturating_sub(width + 2), rect.y, width, 1);
    frame.render_widget(lock, area);
}

fn visualization_shown(state: &SharedState) -> bool {
    #[cfg(feature = "cast")]
    return state.is_visualization_shown();
    #[cfg(not(feature = "cast"))]
    {
        let _ = state;
        false
    }
}

/// Render the application's main layout
fn render_main_layout(
    is_active: bool,
    frame: &mut Frame,
    state: &SharedState,
    ui: &mut UIStateGuard,
    rect: Rect,
) {
    #[cfg(feature = "cast")]
    if visualization_shown(state) {
        streaming::render_visualization_page(frame, state, ui, rect);
        return;
    }

    let page_type = ui.current_page().page_type();
    match page_type {
        PageType::Library => page::render_library_page(is_active, frame, state, ui, rect),
        PageType::Search => page::render_search_page(is_active, frame, state, ui, rect),
        PageType::Context => page::render_context_page(is_active, frame, state, ui, rect),
        PageType::Browse => page::render_browse_page(is_active, frame, state, ui, rect),
        PageType::Lyrics => page::render_lyrics_page(is_active, frame, state, ui, rect),
        PageType::Queue => page::render_queue_page(frame, state, ui, rect),
        PageType::CommandHelp => page::render_commands_help_page(frame, ui, rect),
        PageType::Logs => page::render_logs_page(frame, state, ui, rect),
    }
}

/// The drawn frame as lines of text, as the terminal shows it
fn screen_text(buffer: &ratatui::buffer::Buffer) -> Vec<String> {
    let area = buffer.area;
    (area.top()..area.bottom())
        .map(|y| {
            (area.left()..area.right())
                .map(|x| buffer[(x, y)].symbol())
                .collect::<String>()
                .trim_end()
                .to_string()
        })
        .collect()
}
