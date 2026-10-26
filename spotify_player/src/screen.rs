//! The shared screen: every PC shows the same page, popup and visualization. The session on the
//! NAS keeps what the screen shows and passes it to every PC. One PC at a time is in control and
//! changes the screen; the others show it locked until space takes control. The session also
//! lists the PCs playing its audio, and mutes one when another asks.

use serde::{Deserialize, Serialize};

/// The session's port for the shared screen
pub const PORT: u16 = 1706;

/// From a PC to the session, one JSON object per line
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum ToSession {
    /// This PC takes control.
    Take { pc: String },
    /// What the screen shows now, from the PC in control
    Screen {
        pc: String,
        screen: serde_json::Value,
    },
    /// Mute or unmute a PC, with an auth.romaine.life access token (`home_auth`)
    Mute {
        pc: String,
        muted: bool,
        token: String,
    },
}

/// A PC playing the session's audio, as the cast server knows it
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct Pc {
    /// The cast server's id for it: its host name
    pub id: String,
    pub name: String,
    pub muted: bool,
}

/// From the session to every PC, one JSON object per line, sent whenever any of it changes
#[derive(Serialize, Deserialize, Clone, Default)]
struct FromSession {
    controller: Option<String>,
    /// `None` until a PC first takes control
    screen: Option<serde_json::Value>,
    #[serde(default)]
    pcs: Vec<Pc>,
}

/// The session's side: keeps the screen, saved to `path` so a restart keeps it, and passes it on.
/// The screen is opaque here, so the session needs no update when its contents change.
#[cfg(feature = "streaming")]
pub mod session {
    use super::{FromSession, Pc, ToSession, PORT};
    use anyhow::Context as _;
    use std::path::{Path, PathBuf};
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    use tokio::sync::watch;

    /// Of the session's two slots, the one that gets the port serves the screen; the other waits
    /// for it to exit.
    pub async fn serve(path: PathBuf) {
        let mut logged = false;
        loop {
            match tokio::net::TcpListener::bind(("0.0.0.0", PORT)).await {
                Ok(listener) => return relay(listener, path, crate::listeners::pcs()).await,
                Err(err) if !logged => {
                    tracing::info!("Waiting for port {PORT} for the shared screen: {err:#}");
                    logged = true;
                }
                Err(_) => {}
            }
            tokio::time::sleep(std::time::Duration::from_secs(1)).await;
        }
    }

    async fn relay(
        listener: tokio::net::TcpListener,
        path: PathBuf,
        mut pcs: watch::Receiver<Vec<Pc>>,
    ) {
        let screen = std::fs::read_to_string(&path)
            .ok()
            .and_then(|saved| serde_json::from_str(&saved).ok());
        let (shared, _) = watch::channel(FromSession {
            controller: None,
            screen,
            pcs: pcs.borrow_and_update().clone(),
        });
        tokio::spawn({
            let shared = shared.clone();
            async move {
                while pcs.changed().await.is_ok() {
                    let now = pcs.borrow_and_update().clone();
                    shared.send_modify(|shared| shared.pcs = now);
                }
            }
        });
        loop {
            match listener.accept().await {
                Ok((stream, addr)) => {
                    let shared = shared.clone();
                    let path = path.clone();
                    tokio::spawn(async move {
                        if let Err(err) = serve_pc(stream, &shared, &path).await {
                            tracing::warn!(
                                "The shared screen's connection to {addr} ended: {err:#}"
                            );
                        }
                    });
                }
                Err(err) => tracing::warn!("Failed to accept a shared screen connection: {err:#}"),
            }
        }
    }

    async fn serve_pc(
        stream: tokio::net::TcpStream,
        shared: &watch::Sender<FromSession>,
        path: &Path,
    ) -> anyhow::Result<()> {
        let (reader, mut writer) = stream.into_split();
        let mut lines = BufReader::new(reader).lines();
        let mut changes = shared.subscribe();
        changes.mark_changed();
        loop {
            tokio::select! {
                changed = changes.changed() => {
                    changed?;
                    let mut line = serde_json::to_string(&*changes.borrow_and_update())?;
                    line.push('\n');
                    writer.write_all(line.as_bytes()).await?;
                }
                line = lines.next_line() => {
                    let Some(line) = line? else {
                        return Ok(());
                    };
                    match serde_json::from_str(&line).context("parse a PC's message")? {
                        ToSession::Take { pc } => {
                            tracing::info!("Shared screen: {pc} takes control");
                            shared.send_modify(|shared| shared.controller = Some(pc));
                        }
                        ToSession::Screen { pc, screen } => {
                            if shared.borrow().controller.as_ref() != Some(&pc) {
                                continue;
                            }
                            if let Err(err) = std::fs::write(path, screen.to_string()) {
                                tracing::warn!("Failed to save the shared screen: {err:#}");
                            }
                            shared.send_modify(|shared| shared.screen = Some(screen));
                        }
                        ToSession::Mute { pc, muted, token } => {
                            tokio::spawn(async move {
                                if let Err(err) = mute(&pc, muted, &token).await {
                                    tracing::warn!("Failed to mute {pc}: {err:#}");
                                }
                            });
                        }
                    }
                }
            }
        }
    }

    async fn mute(pc: &str, muted: bool, token: &str) -> anyhow::Result<()> {
        crate::home_auth::session::check(token)
            .await
            .context("refuse the request")?;
        crate::listeners::set_muted(pc, muted).await?;
        tracing::info!("{} {pc}", if muted { "Muted" } else { "Unmuted" });
        Ok(())
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, Lines};
        use tokio::net::tcp::OwnedReadHalf;

        async fn pc(
            addr: std::net::SocketAddr,
        ) -> (
            Lines<BufReader<OwnedReadHalf>>,
            tokio::net::tcp::OwnedWriteHalf,
        ) {
            let (reader, writer) = tokio::net::TcpStream::connect(addr)
                .await
                .unwrap()
                .into_split();
            (BufReader::new(reader).lines(), writer)
        }

        async fn next(lines: &mut Lines<BufReader<OwnedReadHalf>>) -> serde_json::Value {
            let line = tokio::time::timeout(std::time::Duration::from_secs(5), lines.next_line())
                .await
                .unwrap()
                .unwrap()
                .unwrap();
            serde_json::from_str(&line).unwrap()
        }

        #[tokio::test]
        async fn only_the_pc_in_control_changes_the_screen_and_it_is_kept() {
            let dir = std::env::temp_dir().join(format!("spt-screen-test-{}", std::process::id()));
            std::fs::create_dir_all(&dir).unwrap();
            let path = dir.join("screen.json");
            let _ = std::fs::remove_file(&path);
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let addr = listener.local_addr().unwrap();
            let (pcs, pcs_rx) = watch::channel(Vec::new());
            tokio::spawn(relay(listener, path.clone(), pcs_rx));

            let (mut a_in, mut a_out) = pc(addr).await;
            let (mut b_in, mut b_out) = pc(addr).await;
            assert_eq!(
                next(&mut a_in).await,
                serde_json::json!({"controller": null, "screen": null, "pcs": []})
            );
            next(&mut b_in).await;

            a_out
                .write_all(
                    b"{\"take\":{\"pc\":\"a\"}}
",
                )
                .await
                .unwrap();
            assert_eq!(next(&mut b_in).await["controller"], "a");
            b_out
                .write_all(
                    b"{\"screen\":{\"pc\":\"b\",\"screen\":\"ignored\"}}
",
                )
                .await
                .unwrap();
            a_out
                .write_all(
                    b"{\"screen\":{\"pc\":\"a\",\"screen\":\"library\"}}
",
                )
                .await
                .unwrap();
            assert_eq!(
                next(&mut b_in).await,
                serde_json::json!({"controller": "a", "screen": "library", "pcs": []})
            );
            assert_eq!(std::fs::read_to_string(&path).unwrap(), "\"library\"");

            // a PC that opens later starts from it
            let (mut c_in, _c_out) = pc(addr).await;
            assert_eq!(next(&mut c_in).await["screen"], "library");

            // and every PC hears of the PCs playing the audio
            pcs.send(vec![Pc {
                id: "upstairspc".to_string(),
                name: "upstairspc".to_string(),
                muted: true,
            }])
            .unwrap();
            assert_eq!(
                next(&mut c_in).await["pcs"],
                serde_json::json!([{"id": "upstairspc", "name": "upstairspc", "muted": true}])
            );
            std::fs::remove_dir_all(&dir).unwrap();
        }
    }
}

/// A PC's side: follows the session's screen while locked, and sends its own while in control.
/// With the session out of reach, the PC is its own screen.
#[cfg(feature = "cast")]
pub mod pc {
    use super::{FromSession, Pc, ToSession, PORT};
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
    }

    static LINK: OnceLock<Link> = OnceLock::new();

    /// Connect to the session's screen, in the background
    pub fn start(
        state: &SharedState,
        client_pub: &flume::Sender<ClientRequest>,
    ) -> anyhow::Result<()> {
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

    /// Whether another PC is in control, so this one only shows the screen
    pub fn is_locked() -> bool {
        LINK.get().is_some_and(|link| {
            link.connected.load(Ordering::Relaxed) && !link.in_control.load(Ordering::Relaxed)
        })
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

    /// Mute or unmute a PC: this one directly, another through the session, signing in to
    /// auth.romaine.life first if needed
    pub fn set_muted(pc: &Pc, muted: bool) {
        if pc.id == gethostname::gethostname().to_string_lossy() {
            if crate::cast::is_listening() == muted {
                crate::cast::toggle_listening();
            }
            return;
        }
        let Some(link) = LINK.get() else {
            return;
        };
        let pc = pc.id.clone();
        let sent = std::thread::Builder::new()
            .name("mute-a-pc".to_string())
            .spawn(move || {
                let sent = crate::home_auth::pc::access_token().and_then(|token| {
                    anyhow::ensure!(
                        link.connected.load(Ordering::Relaxed),
                        "the session is out of reach"
                    );
                    send(
                        link,
                        &ToSession::Mute {
                            pc: pc.clone(),
                            muted,
                            token,
                        },
                    )
                });
                if let Err(err) = sent {
                    tracing::error!("Failed to ask the session to mute {pc}: {err:#}");
                }
            });
        if let Err(err) = sent {
            tracing::error!("Failed to start muting a PC: {err:#}");
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
        for line in BufReader::new(stream).lines() {
            let mut message: FromSession =
                serde_json::from_str(&line?).context("parse the session's message")?;
            *link.pcs.lock() = std::mem::take(&mut message.pcs);
            // a change to the PCs alone leaves the screen as it is
            let shown = Some((message.controller.clone(), message.screen.clone()));
            if shown == last {
                continue;
            }
            last = shown;
            let in_control = message.controller.as_deref() == Some(link.pc.as_str());
            link.in_control.store(in_control, Ordering::Relaxed);
            if in_control {
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
        }
        anyhow::bail!("the session closed the connection")
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
        let requests = data_requests(state, ui.current_page(), ui.popup.as_ref());
        drop(ui);
        for request in requests {
            let _ = client_pub.send(request);
        }
    }

    /// What this PC hasn't loaded of a page another PC opened. Context pages and lyrics load on
    /// their own once shown.
    fn data_requests(
        state: &SharedState,
        page: &PageState,
        popup: Option<&PopupState>,
    ) -> Vec<ClientRequest> {
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
        if matches!(popup, Some(PopupState::DeviceList(_))) {
            requests.push(ClientRequest::GetDevices);
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
                popup: ui
                    .popup
                    .as_ref()
                    .filter(|popup| !matches!(popup, PopupState::ThemeList(..))),
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
}
