//! The command palette: fzt's search over the menu in `menu.yaml`, and what its entries run.
//!
//! An entry's `action` is one of
//! - a command, as named in the keymap config (`NextTrack`), run as if its key was pressed
//! - `action:<Action>`, an action on the playing track (`action:GoToRadio`)
//! - `play:<playlist or album URI>`, which plays it
//! - `radio:<playlist id>`, a radio from a random song of that playlist
//! - `prompt:search` / `prompt:filter`, which take a search term for Spotify or for the list on
//!   screen

use super::*;
use crate::fzt;
use crate::state::CommandPalette;

const MENU_FILE: &str = "menu.yaml";
/// The menu used until `menu.yaml` exists, and copied there by "edit menu"
const DEFAULT_MENU: &str = include_str!("../../../examples/menu.yaml");
const EDIT_MENU: &str = "edit-menu";

fn menu_path() -> std::path::PathBuf {
    config::get_config().config_folder.join(MENU_FILE)
}

fn read_menu() -> Result<String> {
    match std::fs::read_to_string(menu_path()) {
        Ok(menu) => Ok(menu),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(DEFAULT_MENU.to_string()),
        Err(err) => Err(err).with_context(|| format!("read {}", menu_path().display())),
    }
}

/// Open the palette, reading the menu fresh so edits show up without a restart
pub fn open(ui: &mut UIStateGuard) {
    let opened = read_menu().and_then(|menu| {
        fzt::Session::open(
            &menu,
            &fzt::Options {
                title: "spt".to_string(),
                width: 100,
                height: 40,
                frontend: "spt".to_string(),
                commands: vec![fzt::Command {
                    name: "edit menu".to_string(),
                    description: format!("Open {MENU_FILE} in your editor"),
                    action: EDIT_MENU.to_string(),
                }],
            },
        )
    });
    let palette = match opened {
        Ok((session, frame)) => CommandPalette::Open { session, frame },
        Err(err) => {
            tracing::error!("Failed to open the command palette: {err:#}");
            CommandPalette::Failed(format!("{err:#}"))
        }
    };
    ui.popup = Some(PopupState::CommandPalette(Box::new(palette)));
}

fn fzt_key(key: Key) -> Option<fzt::KeyEvent> {
    let Key::None(code) = key else {
        return None;
    };
    let (key, char) = match code {
        KeyCode::Char(c) => ("char", c.to_string()),
        KeyCode::Enter => ("enter", String::new()),
        KeyCode::Esc => ("esc", String::new()),
        KeyCode::Up => ("up", String::new()),
        KeyCode::Down => ("down", String::new()),
        KeyCode::Left => ("left", String::new()),
        KeyCode::Right => ("right", String::new()),
        KeyCode::Tab => ("tab", String::new()),
        KeyCode::BackTab => ("backtab", String::new()),
        KeyCode::Backspace => ("backspace", String::new()),
        KeyCode::Delete => ("delete", String::new()),
        KeyCode::Home => ("home", String::new()),
        KeyCode::End => ("end", String::new()),
        KeyCode::PageUp => ("pgup", String::new()),
        KeyCode::PageDown => ("pgdn", String::new()),
        _ => return None,
    };
    Some(fzt::KeyEvent { key, char })
}

/// Hand a key press to the palette, and run what the user picks
pub fn handle_key_sequence(
    key_sequence: &KeySequence,
    client_pub: &flume::Sender<ClientRequest>,
    state: &SharedState,
    ui: &mut UIStateGuard,
) -> Result<bool> {
    let Some(PopupState::CommandPalette(palette)) = ui.popup.as_mut() else {
        anyhow::bail!("expect a command palette");
    };
    let CommandPalette::Open { session, frame } = palette.as_mut() else {
        // only says why it couldn't open, or shows another PC's palette; any key closes it
        ui.popup = None;
        return Ok(true);
    };
    let Some(event) = key_sequence.keys.last().copied().and_then(fzt_key) else {
        return Ok(true);
    };
    *frame = session.key(&event)?;
    let Some(pick) = frame.result.take() else {
        return Ok(true);
    };

    match pick.kind.as_str() {
        "select" => {
            if let Some(prompt) = pick.action.strip_prefix("prompt:") {
                let placeholder = match prompt {
                    "search" => "search Spotify…",
                    "filter" => "filter this list…",
                    _ => anyhow::bail!("unknown prompt `{prompt}` in {MENU_FILE}"),
                };
                *frame = session.prompt(&fzt::Prompt {
                    mode: prompt.to_string(),
                    action: prompt.to_string(),
                    placeholder: placeholder.to_string(),
                })?;
                return Ok(true);
            }
            ui.popup = None;
            run_action(&pick.action, client_pub, state, ui)?;
        }
        "prompt" => {
            ui.popup = None;
            match pick.action.as_str() {
                "search" => search_spotify(&pick.text, client_pub, state, ui)?,
                "filter" => {
                    ui.new_search_popup();
                    ui.popup = Some(PopupState::Search { query: pick.text });
                }
                other => anyhow::bail!("unknown prompt `{other}`"),
            }
        }
        "command" => {
            ui.popup = None;
            if pick.action == EDIT_MENU {
                edit_menu()?;
            }
        }
        _ => ui.popup = None,
    }
    Ok(true)
}

fn run_action(
    action: &str,
    client_pub: &flume::Sender<ClientRequest>,
    state: &SharedState,
    ui: &mut UIStateGuard,
) -> Result<()> {
    if action.is_empty() {
        return Ok(());
    }
    if let Some(uri) = action.strip_prefix("play:") {
        let context = if uri.starts_with("spotify:album:") {
            ContextId::Album(AlbumId::from_uri(uri)?.into_static())
        } else {
            ContextId::Playlist(
                PlaylistId::from_uri(uri)
                    .with_context(|| format!("`{action}` needs a playlist or album URI"))?
                    .into_static(),
            )
        };
        client_pub.send(ClientRequest::Player(PlayerRequest::StartPlayback(
            Playback::Context(context, None),
            None,
        )))?;
        return Ok(());
    }
    if let Some(id) = action.strip_prefix("radio:") {
        let playlist_id = PlaylistId::from_id(id)
            .with_context(|| format!("`{action}` needs a playlist id"))?
            .into_static();
        client_pub.send(ClientRequest::StartRandomTrackRadio(playlist_id))?;
        return Ok(());
    }
    if let Some(name) = action.strip_prefix("action:") {
        let action: Action = serde_json::from_value(serde_json::Value::String(name.to_string()))
            .with_context(|| format!("unknown action `{name}` in {MENU_FILE}"))?;
        handle_global_action(action, ActionTarget::PlayingTrack, client_pub, state, ui)?;
        return Ok(());
    }
    let command: Command = serde_json::from_value(serde_json::Value::String(action.to_string()))
        .with_context(|| format!("unknown command `{action}` in {MENU_FILE}"))?;
    // as if its key was pressed: page-specific commands are handled by the page
    let keymaps = &config::get_config().keymap_config.keymaps;
    if let Some(keymap) = keymaps.iter().find(|k| k.command == command) {
        if page::handle_key_sequence_for_page(&keymap.key_sequence, client_pub, state, ui)? {
            return Ok(());
        }
    }
    if !handle_global_command(command, client_pub, state, ui, None)? {
        tracing::warn!("{command:?} has nothing to act on here");
    }
    Ok(())
}

fn search_spotify(
    query: &str,
    client_pub: &flume::Sender<ClientRequest>,
    state: &SharedState,
    ui: &mut UIStateGuard,
) -> Result<()> {
    let query = query.trim().to_string();
    if query.is_empty() {
        return Ok(());
    }
    let mut page_state = SearchPageUIState::new();
    page_state.focus = SearchFocusState::Tracks;
    ui.new_page(PageState::Search {
        line_input: LineInput::new(query.chars().collect()),
        current_query: query.clone(),
        state: page_state,
    });
    if state.data.write().caches.begin_search(&query) {
        if let Err(err) = client_pub.send(ClientRequest::Search(query.clone())) {
            state.data.write().caches.fail_search(query);
            return Err(err.into());
        }
    }
    Ok(())
}

/// Open `menu.yaml` with the system's default app, writing the default menu there first
fn edit_menu() -> Result<()> {
    let path = menu_path();
    if !path.exists() {
        std::fs::write(&path, DEFAULT_MENU).with_context(|| format!("write {}", path.display()))?;
    }
    open::that_detached(&path).with_context(|| format!("open {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_menu_names_only_real_commands_and_actions() {
        let actions = regex::Regex::new(r#"action: "([^"]*)""#).unwrap();
        for found in actions.captures_iter(DEFAULT_MENU) {
            let action = &found[1];
            if ["play:", "radio:", "prompt:"]
                .iter()
                .any(|p| action.starts_with(p))
            {
                continue;
            }
            let value = |name: &str| serde_json::Value::String(name.to_string());
            match action.strip_prefix("action:") {
                Some(name) => assert!(
                    serde_json::from_value::<Action>(value(name)).is_ok(),
                    "unknown action {name}"
                ),
                None => assert!(
                    serde_json::from_value::<Command>(value(action)).is_ok(),
                    "unknown command {action}"
                ),
            }
        }
    }
}
