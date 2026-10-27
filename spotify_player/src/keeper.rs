//! The session keeps playlists in step with what has been played from them, so playing one from
//! its top always picks up where you left off. Each pass removes the songs Spotify's listening
//! history shows were played from a kept playlist, and, for the playlist the radio is on, the
//! songs before the radio's song; a radio playlist is then refilled with radios seeded by random
//! songs of its source playlist. The radio's song itself is never removed: where the radio is
//! comes from the session, playing or paused, not from Spotify. The keeper looks every
//! `INTERVAL` and asks Spotify for nothing unless a kept playlist has moved on: the radio's, or the
//! one Spotify is seen playing elsewhere (a phone).

use crate::{
    client::AppClient,
    config::{self, KeptPlaylist},
    state::SharedState,
};
use anyhow::{Context as _, Result};
use chrono::{DateTime, Utc};
use rspotify::{
    model::{EpisodeId, PlayHistory, PlayableId, PlaylistId, TrackId},
    prelude::*,
};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::time::Duration;

const INTERVAL: Duration = Duration::from_mins(10);
const STATE_FILE: &str = "keeper.json";
/// The most radios one pass fetches to fill a radio playlist's buffer
const RADIOS_PER_PASS: usize = 10;
/// The most items one playlist request takes
const CHUNK: usize = 100;

#[derive(Serialize, Deserialize, Default)]
struct KeeperState {
    playlists: HashMap<String, PlaylistState>,
}

#[derive(Serialize, Deserialize, Default)]
struct PlaylistState {
    /// The newest play from this playlist already acted on
    history_after: Option<DateTime<Utc>>,
    last_kept: Option<DateTime<Utc>>,
    /// The song playing from this playlist at the last pass, the radio's or Spotify's elsewhere
    #[serde(default)]
    kept_at: Option<String>,
}

/// Beside the saved session, so a new release's slot carries on from the last one's
fn state_path() -> std::path::PathBuf {
    config::get_config().session_folder().join(STATE_FILE)
}

fn load_state() -> KeeperState {
    std::fs::read(state_path())
        .ok()
        .and_then(|data| serde_json::from_slice(&data).ok())
        .unwrap_or_default()
}

fn save_state(state: &KeeperState) {
    let result = serde_json::to_vec_pretty(state)
        .map_err(anyhow::Error::from)
        .and_then(|data| std::fs::write(state_path(), data).map_err(anyhow::Error::from));
    if let Err(err) = result {
        tracing::warn!("Failed to save the keeper's state: {err:#}");
    }
}

/// Keep the configured playlists until the session stops
pub async fn run(client: AppClient, session: SharedState) {
    let kept = config::get_config().app_config.kept_playlists.clone();
    if kept.is_empty() {
        return;
    }
    let mut state = load_state();
    loop {
        if let Err(err) = pass(&client, &session, &kept, &mut state).await {
            tracing::error!("Failed to keep playlists: {err:#}");
        }
        save_state(&state);
        tokio::time::sleep(INTERVAL).await;
    }
}

async fn pass(
    client: &AppClient,
    session: &SharedState,
    kept: &[KeptPlaylist],
    state: &mut KeeperState,
) -> Result<()> {
    // where the radio is, from the session that owns it
    let (playing_context, playing_uri) = {
        let mut session = session.session.lock();
        session.sync();
        (
            session.session.context_uri.clone(),
            session.session.current_uri.clone(),
        )
    };
    // what Spotify plays elsewhere, when the session follows it
    let followed = {
        let follow = session.follow.lock();
        follow
            .remote
            .as_ref()
            .filter(|_| follow.following)
            .and_then(|remote| Some((remote.context_uri.clone()?, remote.track_uri.clone())))
    };
    let now = Utc::now();

    let mut history: Option<Vec<PlayHistory>> = None;
    for playlist in kept {
        let uri = PlaylistId::from_id(&playlist.id)
            .with_context(|| format!("kept playlist id {}", playlist.id))?
            .uri();
        let playing_here = playing_context.as_deref() == Some(uri.as_str());
        let followed_song = followed
            .as_ref()
            .filter(|(context, _)| *context == uri)
            .map(|(_, song)| song.clone());
        let playlist_state = state.playlists.entry(playlist.id.clone()).or_default();
        // a playlist is kept again once what plays from it has moved on
        let song = if playing_here {
            playing_uri.clone()
        } else {
            followed_song
        };
        if song.is_none() || playlist_state.kept_at == song {
            continue;
        }
        if history.is_none() {
            history = Some(
                client
                    .current_user_recently_played(Some(50), None)
                    .await?
                    .items,
            );
        }
        let current = if playing_here {
            playing_uri.as_deref()
        } else {
            None
        };
        let history = history.as_deref().unwrap_or_default();
        if let Err(err) = keep(client, playlist, current, history, playlist_state).await {
            tracing::error!("Failed to keep playlist {uri}: {err:#}");
            continue;
        }
        playlist_state.last_kept = Some(now);
        playlist_state.kept_at = song;
    }
    Ok(())
}

/// Keep one playlist. `current` is the song playing from it, if it is the one playing.
async fn keep(
    client: &AppClient,
    playlist: &KeptPlaylist,
    current: Option<&str>,
    history: &[PlayHistory],
    state: &mut PlaylistState,
) -> Result<()> {
    let playlist_id = PlaylistId::from_id(&playlist.id)?.into_static();
    let uri = playlist_id.uri();
    let items = client.playlist_item_uris(playlist_id.as_ref()).await?;

    let plays_here: Vec<(DateTime<Utc>, String)> = history
        .iter()
        .filter(|h| h.context.as_ref().is_some_and(|c| c.uri == uri))
        .filter_map(|h| Some((h.played_at, h.track.id.as_ref()?.uri())))
        .collect();
    let played = played_songs(&items, &plays_here, state.history_after, current);
    if let Some(newest) = plays_here.iter().map(|(at, _)| *at).max() {
        state.history_after = Some(state.history_after.map_or(newest, |t| t.max(newest)));
    }

    let remove: Vec<PlayableId<'static>> = items
        .iter()
        .filter(|u| played.contains(*u))
        .collect::<HashSet<_>>()
        .into_iter()
        .filter_map(|u| playable_id(u))
        .collect();
    for chunk in remove.chunks(CHUNK) {
        client
            .playlist_remove_all_occurrences_of_items(playlist_id.as_ref(), chunk.to_vec(), None)
            .await
            .context("remove played songs")?;
    }
    if !remove.is_empty() {
        tracing::info!("Removed {} played songs from {uri}", remove.len());
    }

    let Some(source) = &playlist.radio_from else {
        return Ok(());
    };
    let source = PlaylistId::from_id(source)
        .with_context(|| format!("radio_from playlist id {source}"))?
        .into_static();
    let mut have: HashSet<String> = items.into_iter().filter(|u| !played.contains(u)).collect();
    for _ in 0..RADIOS_PER_PASS {
        if have.len() >= playlist.buffer {
            break;
        }
        let seed = client.random_playlist_track(None, source.clone()).await?;
        let mut ids = vec![seed.id.clone()];
        ids.extend(client.radio_track_ids(&seed.id.uri()).await?);
        // songs already in the playlist, played or ahead, aren't added again
        let new: Vec<PlayableId<'static>> = ids
            .into_iter()
            .filter(|id| have.insert(id.uri()))
            .map(PlayableId::Track)
            .collect();
        for chunk in new.chunks(CHUNK) {
            client
                .playlist_add_items(playlist_id.as_ref(), chunk.to_vec(), None)
                .await
                .context("add radio songs")?;
        }
        tracing::info!(
            "Added {} songs to {uri} from a radio of {}",
            new.len(),
            seed.name
        );
    }
    Ok(())
}

/// The songs of a kept playlist (`items`, in order) that have been played: those `plays` from it
/// show played after `after`, and, when the radio is on it at `current`, those before `current`,
/// which backs up the history that keeps only the last 50 plays. `current` itself stays, played in
/// part or not: it's where the radio is.
fn played_songs(
    items: &[String],
    plays: &[(DateTime<Utc>, String)],
    after: Option<DateTime<Utc>>,
    current: Option<&str>,
) -> HashSet<String> {
    let mut played: HashSet<String> = plays
        .iter()
        .filter(|(at, _)| after.is_none_or(|after| *at > after))
        .map(|(_, uri)| uri.clone())
        .collect();
    if let Some(index) = current.and_then(|c| items.iter().position(|u| u == c)) {
        played.extend(items[..index].iter().cloned());
    }
    if let Some(current) = current {
        played.remove(current);
    }
    played
}

fn playable_id(uri: &str) -> Option<PlayableId<'static>> {
    if let Ok(id) = TrackId::from_uri(uri) {
        return Some(PlayableId::Track(id.into_static()));
    }
    EpisodeId::from_uri(uri)
        .ok()
        .map(|id| PlayableId::Episode(id.into_static()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(minute: u32) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(&format!("2026-10-24T20:{minute:02}:00Z"))
            .unwrap()
            .with_timezone(&Utc)
    }

    fn uris(names: &[&str]) -> Vec<String> {
        names.iter().map(|n| (*n).to_string()).collect()
    }

    #[test]
    fn played_songs_are_history_after_the_mark_and_songs_before_the_current_one() {
        let items = uris(&["a", "b", "c", "d", "e"]);
        let plays = vec![(at(1), "a".to_string()), (at(5), "c".to_string())];

        // all of the history counts the first time
        let played = played_songs(&items, &plays, None, None);
        assert_eq!(played, ["a", "c"].map(String::from).into());

        // a play already acted on doesn't count again, even if the song came back
        let played = played_songs(&items, &plays, Some(at(1)), None);
        assert_eq!(played, ["c"].map(String::from).into());

        // the songs before the playing one count, whatever the history says
        let played = played_songs(&items, &[], None, Some("d"));
        assert_eq!(played, ["a", "b", "c"].map(String::from).into());

        // a current song that isn't in the playlist removes nothing more
        let played = played_songs(&items, &[], None, Some("z"));
        assert!(played.is_empty());

        // the radio's song stays though the history shows it played
        let played = played_songs(&items, &plays, None, Some("c"));
        assert_eq!(played, ["a", "b"].map(String::from).into());
    }
}
