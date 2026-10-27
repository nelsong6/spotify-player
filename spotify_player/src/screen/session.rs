use super::{FromSession, Pc, SessionPlayback, ToSession, PORT};
use crate::client::AppClient;
use crate::state::SharedState;
use anyhow::Context as _;
use rspotify::model::{
    Actions, Context, CurrentPlaybackContext, CurrentlyPlayingType, Device, DeviceType, FullTrack,
    PlayableItem, RepeatState, TrackId,
};
use rspotify::prelude::*;
use std::path::{Path, PathBuf};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::watch;

/// Of the session's two slots, the one that gets the port serves the screen; the other waits
/// for it to exit.
pub async fn serve(path: PathBuf, state: SharedState, client: AppClient) {
    let mut logged = false;
    loop {
        match tokio::net::TcpListener::bind(("0.0.0.0", PORT)).await {
            Ok(listener) => {
                let follow: OnFollow = std::sync::Arc::new({
                    let state = state.clone();
                    move || crate::follow::start(&state)
                });
                let playback = playback(state, client);
                return relay(listener, path, crate::listeners::pcs(), playback, follow).await;
            }
            Err(err) if !logged => {
                tracing::info!("Waiting for port {PORT} for the shared screen: {err:#}");
                logged = true;
            }
            Err(_) => {}
        }
        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
    }
}

/// The session's playback, kept current as it changes
fn playback(state: SharedState, client: AppClient) -> watch::Receiver<Option<serde_json::Value>> {
    let (sender, receiver) = watch::channel(None);
    tokio::spawn(async move {
        let mut track = None;
        loop {
            if sender
                .send(snapshot(&state, &client, &mut track).await)
                .is_err()
            {
                return;
            }
            state.playback_changed.notified().await;
        }
    });
    receiver
}

/// `track` keeps the last song shown that the session's own player doesn't report, so Spotify
/// is asked for each such song once
async fn snapshot(
    state: &SharedState,
    client: &AppClient,
    track: &mut Option<FullTrack>,
) -> Option<serde_json::Value> {
    let followed = {
        let follow = state.follow.lock();
        follow.following.then(|| follow.remote.clone())
    };
    if let Some(remote) = followed {
        let playback = match remote {
            Some(remote) => followed_playback(client, track, &remote).await,
            None => None,
        };
        return to_json(SessionPlayback {
            playback,
            queue: None,
            at_ms: chrono::Utc::now().timestamp_millis(),
            left_off: false,
            following: true,
        });
    }
    // Spotify's playback is the radio only on the session's own player, playing the saved
    // session; anything else (a phone, what Spotify loads on connecting) isn't
    let (own, saved) = {
        let mut session = state.session.lock();
        session.sync();
        (session.own_device.clone(), session.session.clone())
    };
    let (playback, queue) = {
        let player = state.player.read();
        match player.current_playback() {
            Some(playback)
                if own.is_some() && playback.device.id == own && saved.holds(&playback) =>
            {
                (Some(playback), player.queue.clone())
            }
            _ => (None, None),
        }
    };
    let playback = match playback {
        Some(playback) => SessionPlayback {
            playback: Some(playback),
            queue,
            at_ms: chrono::Utc::now().timestamp_millis(),
            left_off: false,
            following: false,
        },
        None => SessionPlayback {
            playback: left_off(state, client, track).await,
            queue: None,
            at_ms: chrono::Utc::now().timestamp_millis(),
            left_off: true,
            following: false,
        },
    };
    to_json(playback)
}

fn to_json(playback: SessionPlayback) -> Option<serde_json::Value> {
    serde_json::to_value(playback)
        .inspect_err(|err| tracing::error!("Failed to serialize the playback: {err:#}"))
        .ok()
}

/// The saved session's song, paused where it was left, as a playback the PCs can show
async fn left_off(
    state: &SharedState,
    client: &AppClient,
    track: &mut Option<FullTrack>,
) -> Option<CurrentPlaybackContext> {
    let (saved, device_id) = {
        let mut session = state.session.lock();
        session.sync();
        (session.session.clone(), session.own_device.clone())
    };
    let song = song(client, track, saved.current_uri.as_deref()?).await?;
    Some(shown_playback(Shown {
        song,
        context_uri: saved.context_uri,
        progress_ms: saved.progress_ms,
        device_id,
        device_name: crate::config::get_config().app_config.device.name.clone(),
        volume: None,
        is_playing: false,
    }))
}

/// What Spotify plays on the device driving it, as a playback the PCs can show
async fn followed_playback(
    client: &AppClient,
    track: &mut Option<FullTrack>,
    remote: &crate::follow::Remote,
) -> Option<CurrentPlaybackContext> {
    let song = song(client, track, &remote.track_uri).await?;
    let elapsed = if remote.is_playing {
        chrono::Utc::now().timestamp_millis() - remote.at_ms
    } else {
        0
    };
    Some(shown_playback(Shown {
        song,
        context_uri: remote.context_uri.clone(),
        progress_ms: remote.position_ms + elapsed.max(0),
        device_id: None,
        device_name: remote.device.clone(),
        volume: remote.volume,
        is_playing: remote.is_playing,
    }))
}

/// A song's details, asking Spotify only for a song not shown last
async fn song(client: &AppClient, track: &mut Option<FullTrack>, uri: &str) -> Option<FullTrack> {
    let id = TrackId::from_uri(uri).ok()?.into_static();
    if track.as_ref().and_then(|t| t.id.as_ref()) != Some(&id) {
        match BaseClient::track(&**client, id, None).await {
            Ok(song) => *track = Some(song),
            Err(err) => {
                tracing::warn!("Failed to get the song {uri}: {err:#}");
                return None;
            }
        }
    }
    track.clone()
}

struct Shown {
    song: FullTrack,
    context_uri: Option<String>,
    progress_ms: i64,
    device_id: Option<String>,
    device_name: String,
    volume: Option<u32>,
    is_playing: bool,
}

fn shown_playback(shown: Shown) -> CurrentPlaybackContext {
    let context = shown.context_uri.and_then(|uri| {
        let kind = uri.split(':').nth(1)?.to_string();
        Some(Context {
            _type: serde_json::from_value(serde_json::Value::String(kind)).ok()?,
            uri,
            href: String::new(),
            external_urls: std::collections::HashMap::new(),
        })
    });
    CurrentPlaybackContext {
        device: Device {
            id: shown.device_id,
            is_active: shown.is_playing,
            is_private_session: false,
            is_restricted: false,
            name: shown.device_name,
            _type: DeviceType::Speaker,
            volume_percent: shown.volume,
        },
        repeat_state: RepeatState::Off,
        shuffle_state: false,
        context,
        timestamp: chrono::Utc::now(),
        progress: Some(chrono::Duration::milliseconds(shown.progress_ms)),
        is_playing: shown.is_playing,
        item: Some(PlayableItem::Track(shown.song)),
        currently_playing_type: CurrentlyPlayingType::Track,
        actions: Actions::default(),
    }
}

/// What the session does when a PC asks it to follow Spotify
type OnFollow = std::sync::Arc<dyn Fn() + Send + Sync>;

async fn relay(
    listener: tokio::net::TcpListener,
    path: PathBuf,
    mut pcs: watch::Receiver<Vec<Pc>>,
    mut playback: watch::Receiver<Option<serde_json::Value>>,
    follow: OnFollow,
) {
    let screen = std::fs::read_to_string(&path)
        .ok()
        .and_then(|saved| serde_json::from_str(&saved).ok());
    let (shared, _) = watch::channel(FromSession {
        controller: None,
        screen,
        pcs: pcs.borrow_and_update().clone(),
        playback: playback.borrow_and_update().clone(),
    });
    tokio::spawn({
        let shared = shared.clone();
        async move {
            loop {
                tokio::select! {
                    changed = pcs.changed() => {
                        if changed.is_err() {
                            return;
                        }
                        let now = pcs.borrow_and_update().clone();
                        shared.send_modify(|shared| shared.pcs = now);
                    }
                    changed = playback.changed() => {
                        if changed.is_err() {
                            return;
                        }
                        let now = playback.borrow_and_update().clone();
                        shared.send_modify(|shared| shared.playback = now);
                    }
                }
            }
        }
    });
    loop {
        match listener.accept().await {
            Ok((stream, addr)) => {
                let shared = shared.clone();
                let path = path.clone();
                let follow = follow.clone();
                tokio::spawn(async move {
                    if let Err(err) = serve_pc(stream, &shared, &path, &follow).await {
                        tracing::warn!("The shared screen's connection to {addr} ended: {err:#}");
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
    follow: &OnFollow,
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
                    ToSession::Follow => follow(),
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
        let (playback, playback_rx) = watch::channel(None);
        tokio::spawn(relay(
            listener,
            path.clone(),
            pcs_rx,
            playback_rx,
            std::sync::Arc::new(|| {}),
        ));

        let (mut a_in, mut a_out) = pc(addr).await;
        let (mut b_in, mut b_out) = pc(addr).await;
        assert_eq!(
            next(&mut a_in).await,
            serde_json::json!({"controller": null, "screen": null, "pcs": [], "playback": null})
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
            serde_json::json!({"controller": "a", "screen": "library", "pcs": [], "playback": null})
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

        // and of what the session plays
        playback.send(Some(serde_json::json!("playing"))).unwrap();
        assert_eq!(next(&mut c_in).await["playback"], "playing");
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
