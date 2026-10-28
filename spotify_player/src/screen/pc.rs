use super::{FromSession, Pc, SessionPlayback, ToSession, PORT};
use crate::client::ClientRequest;
use crate::config;
use crate::state::{
    BrowsePageUIState, ContextId, LibraryPageUIState, PageState, PopupState, SharedState,
};
use anyhow::Context as _;
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use std::io::{BufRead, BufReader, Write};
use std::net::TcpStream;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::OnceLock;
use std::time::Duration;

const RECONNECT_INTERVAL: Duration = Duration::from_secs(2);
/// How often the PC in control checks for changes nothing announced, such as a page opened
/// once its data arrived
const POLL_INTERVAL: Duration = Duration::from_millis(100);

/// What every PC shows
#[derive(Serialize)]
struct Screen<'a> {
    history: &'a [PageState],
    popup: Option<&'a PopupState>,
    visualization: bool,
    #[cfg(feature = "ascii-cover")]
    ascii_cover: config::AsciiCoverMode,
}

#[derive(Deserialize)]
struct OwnedScreen {
    history: Vec<PageState>,
    popup: Option<PopupState>,
    visualization: bool,
    #[cfg(feature = "ascii-cover")]
    ascii_cover: config::AsciiCoverMode,
}

impl Default for OwnedScreen {
    /// The screen before any PC has taken control: the visualization
    fn default() -> Self {
        Self {
            history: vec![PageState::Library {
                state: LibraryPageUIState::new(),
            }],
            popup: None,
            visualization: true,
            #[cfg(feature = "ascii-cover")]
            ascii_cover: config::get_config().app_config.ascii_cover,
        }
    }
}

struct Link {
    pc: String,
    connected: AtomicBool,
    in_control: AtomicBool,
    writer: Mutex<Option<TcpStream>>,
    /// Wakes the sender after a key press, so the other PCs follow without waiting for a poll
    wake: flume::Sender<()>,
    pcs: Mutex<Vec<Pc>>,
    /// Whether this PC knows what it shows: the session's screen arrived, or the session is
    /// out of reach and the PC is its own screen
    settled: AtomicBool,
}

static LINK: OnceLock<Link> = OnceLock::new();

/// Connect to the session's screen, in the background
pub fn start(state: &SharedState, client_pub: &flume::Sender<ClientRequest>) -> anyhow::Result<()> {
    let server = config::get_config().app_config.cast_server.clone();
    // `cast_server`'s port is Snapcast's
    let server = match server.rsplit_once(':') {
        Some((host, _)) => host.to_string(),
        None => server,
    };
    let (wake, woken) = flume::bounded(1);
    let link = LINK.get_or_init(|| Link {
        pc: format!(
            "{}-{}",
            gethostname::gethostname().to_string_lossy(),
            std::process::id()
        ),
        connected: AtomicBool::new(false),
        in_control: AtomicBool::new(false),
        writer: Mutex::new(None),
        wake,
        pcs: Mutex::new(Vec::new()),
        settled: AtomicBool::new(false),
    });
    std::thread::Builder::new()
        .name("screen-connection".to_string())
        .spawn({
            let state = state.clone();
            let client_pub = client_pub.clone();
            move || loop {
                if let Err(err) = follow(link, &server, &state, &client_pub) {
                    tracing::warn!("The shared screen's connection to {server} ended: {err:#}");
                }
                link.settled.store(true, Ordering::Relaxed);
                link.connected.store(false, Ordering::Relaxed);
                *link.writer.lock() = None;
                std::thread::sleep(RECONNECT_INTERVAL);
            }
        })?;
    std::thread::Builder::new()
        .name("screen-sender".to_string())
        .spawn({
            let state = state.clone();
            move || send_changes(link, &state, &woken)
        })?;
    Ok(())
}

/// Whether this PC knows what it shows yet: until then its own page isn't on screen
pub fn is_settled() -> bool {
    LINK.get()
        .is_some_and(|link| link.settled.load(Ordering::Relaxed))
}

/// Whether another PC is in control, so this one only shows the screen
pub fn is_locked() -> bool {
    LINK.get().is_some_and(|link| {
        link.connected.load(Ordering::Relaxed) && !link.in_control.load(Ordering::Relaxed)
    })
}

/// Have the session follow Spotify instead of driving it
pub fn follow_spotify() {
    let Some(link) = LINK.get() else {
        return;
    };
    if let Err(err) = send(link, &ToSession::Follow) {
        tracing::warn!("Failed to ask the session to follow Spotify: {err:#}");
    }
}

/// Make this PC the one in control
pub fn take_control() {
    let Some(link) = LINK.get() else {
        return;
    };
    link.in_control.store(true, Ordering::Relaxed);
    if let Err(err) = send(
        link,
        &ToSession::Take {
            pc: link.pc.clone(),
        },
    ) {
        tracing::warn!("Failed to take control of the shared screen: {err:#}");
    }
    changed();
}

/// The PCs playing the session's audio
pub fn pcs() -> Vec<Pc> {
    LINK.get()
        .map(|link| link.pcs.lock().clone())
        .unwrap_or_default()
}

/// Mute or unmute a PC: this one directly, another through the session's relay
pub fn set_muted(pc: &Pc, muted: bool, client_pub: &flume::Sender<ClientRequest>) {
    if pc.id == gethostname::gethostname().to_string_lossy() {
        crate::cast::set_muted(muted);
        return;
    }
    let _ = client_pub.send(ClientRequest::MutePc {
        pc: pc.id.clone(),
        muted,
    });
}

/// Which PCs play the session here, set for every PC at once
#[derive(Clone, Copy, Debug)]
pub enum Group {
    Everywhere,
    Nowhere,
    OnlyHere,
    AllButHere,
}

impl Group {
    /// In the order the PC list shows them
    pub const ALL: [Group; 4] = [
        Group::Everywhere,
        Group::Nowhere,
        Group::OnlyHere,
        Group::AllButHere,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Group::Everywhere => "play here on every PC",
            Group::Nowhere => "play here on no PC",
            Group::OnlyHere => "play here only on this PC",
            Group::AllButHere => "play here on every PC but this one",
        }
    }

    fn plays(self, here: bool) -> bool {
        match self {
            Group::Everywhere => true,
            Group::Nowhere => false,
            Group::OnlyHere => here,
            Group::AllButHere => !here,
        }
    }
}

/// Turn play here on or off for every PC playing the session, as `group` says
pub fn set_group(group: Group, client_pub: &flume::Sender<ClientRequest>) {
    let here = gethostname::gethostname().to_string_lossy().into_owned();
    for pc in pcs() {
        let play = group.plays(pc.id == here);
        if pc.muted == play {
            set_muted(&pc, !play, client_pub);
        }
    }
}

/// Send the screen now, after a key press may have changed it
pub fn changed() {
    if let Some(link) = LINK.get() {
        let _ = link.wake.try_send(());
    }
}

fn send(link: &Link, message: &ToSession) -> anyhow::Result<()> {
    let mut writer = link.writer.lock();
    let Some(writer) = writer.as_mut() else {
        return Ok(());
    };
    let mut line = serde_json::to_string(message)?;
    line.push('\n');
    writer.write_all(line.as_bytes())?;
    Ok(())
}

fn follow(
    link: &Link,
    server: &str,
    state: &SharedState,
    client_pub: &flume::Sender<ClientRequest>,
) -> anyhow::Result<()> {
    let stream = TcpStream::connect((server, PORT))
        .with_context(|| format!("connect to {server}:{PORT}"))?;
    stream.set_nodelay(true)?;
    *link.writer.lock() = Some(stream.try_clone()?);
    // a PC in control when the connection dropped keeps control
    if link.in_control.load(Ordering::Relaxed) {
        send(
            link,
            &ToSession::Take {
                pc: link.pc.clone(),
            },
        )?;
    }
    link.connected.store(true, Ordering::Relaxed);
    tracing::info!("Shared screen: connected to {server}:{PORT}");
    let mut last = None;
    let mut last_playback = None;
    for line in BufReader::new(stream).lines() {
        let mut message: FromSession =
            serde_json::from_str(&line?).context("parse the session's message")?;
        *link.pcs.lock() = std::mem::take(&mut message.pcs);
        if message.playback != last_playback {
            last_playback.clone_from(&message.playback);
            follow_playback(client_pub, message.playback.take());
        }
        // a change to the PCs alone leaves the screen as it is
        let shown = Some((message.controller.clone(), message.screen.clone()));
        if shown == last {
            continue;
        }
        last = shown;
        let in_control = message.controller.as_deref() == Some(link.pc.as_str());
        link.in_control.store(in_control, Ordering::Relaxed);
        if in_control {
            link.settled.store(true, Ordering::Relaxed);
            changed();
            continue;
        }
        // a screen saved by an older release may not parse; the next one sent replaces it
        let screen = message
            .screen
            .and_then(|screen| {
                serde_json::from_value(screen)
                    .inspect_err(|err| {
                        tracing::warn!("Failed to parse the shared screen: {err:#}");
                    })
                    .ok()
            })
            .unwrap_or_default();
        show(state, client_pub, screen);
        link.settled.store(true, Ordering::Relaxed);
    }
    anyhow::bail!("the session closed the connection")
}

/// Show the session's playback here, as it is now
fn follow_playback(client_pub: &flume::Sender<ClientRequest>, playback: Option<serde_json::Value>) {
    let Some(playback) = playback else {
        return;
    };
    let mut playback: SessionPlayback = match serde_json::from_value(playback) {
        Ok(playback) => playback,
        Err(err) => {
            tracing::warn!("Failed to parse the session's playback: {err:#}");
            return;
        }
    };
    if let Some(current) = playback.playback.as_mut().filter(|p| p.is_playing) {
        let elapsed = chrono::Utc::now().timestamp_millis() - playback.at_ms;
        current.progress = current
            .progress
            .map(|progress| progress + chrono::Duration::milliseconds(elapsed.max(0)));
    }
    let _ = client_pub.send(ClientRequest::FollowSessionPlayback(Box::new(playback)));
}

/// Show another PC's screen here, and fetch what its page shows
fn show(state: &SharedState, client_pub: &flume::Sender<ClientRequest>, screen: OwnedScreen) {
    if screen.history.is_empty() {
        return;
    }
    if let Some(bands) = &state.vis_bands {
        bands.lock().enabled = screen.visualization;
    }
    let mut ui = state.ui.lock();
    #[cfg(feature = "ascii-cover")]
    {
        ui.ascii_cover.mode = screen.ascii_cover;
    }
    ui.history = screen.history;
    ui.popup = screen.popup;
    let requests = data_requests(state, ui.current_page());
    drop(ui);
    for request in requests {
        let _ = client_pub.send(request);
    }
}

/// What this PC hasn't loaded of a page another PC opened. Context pages and lyrics load on
/// their own once shown.
fn data_requests(state: &SharedState, page: &PageState) -> Vec<ClientRequest> {
    let mut requests = Vec::new();
    let mut data = state.data.write();
    match page {
        PageState::Search { current_query, .. } if !current_query.is_empty() => {
            if data.caches.begin_search(current_query) {
                requests.push(ClientRequest::Search(current_query.clone()));
            }
        }
        PageState::Browse {
            state: BrowsePageUIState::CategoryList { .. },
        } if data.browse.categories.is_none() => {
            requests.push(ClientRequest::GetBrowseCategories);
        }
        PageState::Browse {
            state: BrowsePageUIState::CategoryPlaylistList { category, .. },
        } if !data.browse.category_playlists.contains_key(&category.id) => {
            requests.push(ClientRequest::GetBrowseCategoryPlaylists(category.clone()));
        }
        // a radio page's tracks come from the shared session, not from Spotify
        PageState::Context {
            id: Some(ContextId::Tracks(id)),
            ..
        } if !id.uri.starts_with("radio:") && !data.caches.context.contains_key(&id.uri) => {
            requests.push(ClientRequest::GetContext(ContextId::Tracks(id.clone())));
        }
        _ => {}
    }
    requests
}

/// While in control, send the screen whenever it changes
fn send_changes(link: &Link, state: &SharedState, woken: &flume::Receiver<()>) {
    let mut last = String::new();
    loop {
        let _ = woken.recv_timeout(POLL_INTERVAL);
        if !link.connected.load(Ordering::Relaxed) || !link.in_control.load(Ordering::Relaxed) {
            // sent again in full once this PC is in control
            last.clear();
            continue;
        }
        let visualization = state
            .vis_bands
            .as_ref()
            .is_some_and(|bands| bands.lock().enabled);
        let ui = state.ui.lock();
        let screen = serde_json::to_value(Screen {
            history: &ui.history,
            popup: ui.popup.as_ref().filter(|popup| {
                !matches!(
                    popup,
                    PopupState::ThemeList(..) | PopupState::OutputList(..)
                )
            }),
            visualization,
            #[cfg(feature = "ascii-cover")]
            ascii_cover: ui.ascii_cover.mode,
        });
        drop(ui);
        let screen = match screen {
            Ok(screen) => screen,
            Err(err) => {
                tracing::error!("Failed to serialize the shared screen: {err:#}");
                continue;
            }
        };
        let text = screen.to_string();
        if text == last {
            continue;
        }
        if let Err(err) = send(
            link,
            &ToSession::Screen {
                pc: link.pc.clone(),
                screen,
            },
        ) {
            tracing::warn!("Failed to send the shared screen: {err:#}");
            continue;
        }
        last = text;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::{CommandPalette, ContextPageType, TracksId};

    #[test]
    fn a_screen_survives_the_trip_to_another_pc() {
        let id = ContextId::Tracks(TracksId::new("radio:abc", "Radio"));
        let history = vec![
            PageState::Library {
                state: LibraryPageUIState::new(),
            },
            PageState::Context {
                id: Some(id.clone()),
                context_page_type: ContextPageType::Browsing(id),
                state: None,
            },
        ];
        let palette = PopupState::CommandPalette(Box::new(CommandPalette::Shown(
            crate::fzt::Frame::default(),
        )));
        let json = serde_json::to_value(Screen {
            history: &history,
            popup: Some(&palette),
            visualization: true,
            #[cfg(feature = "ascii-cover")]
            ascii_cover: config::AsciiCoverMode::Still,
        })
        .unwrap();
        let screen: OwnedScreen = serde_json::from_value(json).unwrap();
        assert_eq!(screen.history, history);
        assert!(matches!(
            screen.popup,
            Some(PopupState::CommandPalette(palette)) if matches!(*palette, CommandPalette::Shown(_))
        ));
        assert!(screen.visualization);
    }
}
