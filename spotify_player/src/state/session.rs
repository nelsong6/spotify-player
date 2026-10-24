//! The last playback session, saved to the cache folder (or `session_file`) so that restarting
//! the app picks up where it left off: the same tracks (or context), the same track, the same
//! position, and the radio page if the playback was a radio. Several instances can share one
//! session file: only the one whose integrated player is playing records progress, and each takes
//! in what another wrote.

use std::time::{Duration, Instant, SystemTime};

use serde::{Deserialize, Serialize};

use super::{Id as _, Playback, PlayerState, Track};
use crate::config;

const SESSION_FILE: &str = "last_session.json";
/// How often the playing position is written to disk while playing.
const SAVE_INTERVAL: Duration = Duration::from_secs(10);

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct SavedSession {
    /// URI of the playlist, album, artist or show the playback started from, if any
    pub context_uri: Option<String>,
    /// URIs of the tracks the playback started from, when it was not started from a context
    /// (a radio, liked songs, a single track, ...)
    pub uris: Vec<String>,
    /// The radio page shown for this playback, if it is a radio
    pub radio: Option<SavedRadio>,
    /// URI of the item playing when the session was last saved
    pub current_uri: Option<String>,
    /// Position in the current item, in milliseconds
    pub progress_ms: i64,
    /// What plays when the playback runs out
    #[serde(default)]
    pub continue_mode: ContinueMode,
}

/// What plays when the playback runs out
#[derive(Serialize, Deserialize, Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ContinueMode {
    /// A radio from a new random song of the playlist the playback came from, or from the last
    /// song when it didn't come from a playlist
    #[default]
    PlaylistRadio,
    /// A radio from the last song played
    SongRadio,
    Stop,
}

impl ContinueMode {
    pub fn next(self) -> Self {
        match self {
            Self::PlaylistRadio => Self::SongRadio,
            Self::SongRadio => Self::Stop,
            Self::Stop => Self::PlaylistRadio,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::PlaylistRadio => "playlist radio",
            Self::SongRadio => "song radio",
            Self::Stop => "stop",
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct SavedRadio {
    pub uri: String,
    pub kind: String,
    pub tracks: Vec<Track>,
    /// URI of the playlist the radio was made from, directly or from one of its songs
    #[serde(default)]
    pub source_playlist: Option<String>,
}

/// The session being recorded, and when it was last written to disk
#[derive(Debug, Default)]
pub struct SessionState {
    pub session: SavedSession,
    last_saved: Option<Instant>,
    /// This instance's integrated player, the only device whose playback it records
    pub own_device: Option<String>,
    /// The session file's modified time when this instance last wrote or read it
    seen: Option<SystemTime>,
}

impl SavedSession {
    pub fn is_empty(&self) -> bool {
        self.context_uri.is_none() && self.uris.is_empty()
    }

    /// URI of the playlist the playback came from: the radio's source, or the playlist played
    pub fn source_playlist(&self) -> Option<String> {
        match &self.radio {
            Some(radio) => radio.source_playlist.clone(),
            None => self
                .context_uri
                .clone()
                .filter(|uri| uri.starts_with("spotify:playlist:")),
        }
    }
}

fn session_path() -> std::path::PathBuf {
    let configs = config::get_config();
    configs
        .app_config
        .session_file
        .clone()
        .unwrap_or_else(|| configs.cache_folder.join(SESSION_FILE))
}

fn modified() -> Option<SystemTime> {
    std::fs::metadata(session_path())
        .and_then(|m| m.modified())
        .ok()
}

/// The playing item's URI and position
pub fn playing_item(player: &PlayerState) -> Option<(String, i64)> {
    let uri = player.currently_playing().and_then(|item| match item {
        rspotify::model::PlayableItem::Track(track) => {
            track.id.as_ref().map(rspotify::prelude::Id::uri)
        }
        rspotify::model::PlayableItem::Episode(episode) => Some(episode.id.uri()),
        rspotify::model::PlayableItem::Unknown(_) => None,
    })?;
    let progress_ms = player
        .playback_progress()
        .map_or(0, |p| p.num_milliseconds());
    Some((uri, progress_ms))
}

/// Load the session saved by the previous run, if any
pub fn load() -> Option<SavedSession> {
    let data = std::fs::read(session_path()).ok()?;
    match serde_json::from_slice::<SavedSession>(&data) {
        Ok(session) if !session.is_empty() => Some(session),
        Ok(_) => None,
        Err(err) => {
            tracing::warn!("Failed to parse the saved session: {err:#}");
            None
        }
    }
}

impl SessionState {
    /// Record a newly started playback. A radio page recorded just before keeps its place in
    /// the session when the playback is that radio's tracks.
    pub fn record_start(&mut self, playback: &Playback) {
        // picks up another instance's continue mode
        let radio = self.session.radio.take();
        self.sync();
        let (context_uri, uris) = match playback {
            Playback::Context(id, _) => (Some(id.uri()), vec![]),
            Playback::URIs(ids, _) => (
                None,
                ids.iter()
                    .map(rspotify::prelude::Id::uri)
                    .collect::<Vec<_>>(),
            ),
        };
        let radio = radio
            .filter(|radio| radio.tracks.iter().map(|t| t.id.uri()).collect::<Vec<_>>() == uris);
        self.session = SavedSession {
            context_uri,
            uris,
            radio,
            current_uri: None,
            progress_ms: 0,
            continue_mode: self.session.continue_mode,
        };
        self.save();
    }

    /// Record the radio page about to be played
    pub fn record_radio(
        &mut self,
        uri: String,
        kind: String,
        tracks: Vec<Track>,
        source_playlist: Option<String>,
    ) {
        self.session.radio = Some(SavedRadio {
            uri,
            kind,
            tracks,
            source_playlist,
        });
    }

    /// Switch to the next continue mode
    pub fn cycle_continue_mode(&mut self) {
        self.sync();
        self.session.continue_mode = self.session.continue_mode.next();
        self.save();
    }

    /// Take in the session file when another instance wrote it since this one last did
    pub fn sync(&mut self) {
        let modified = modified();
        if modified.is_some() && modified != self.seen {
            if let Some(session) = load() {
                self.session = session;
            }
            self.seen = modified;
        }
    }

    /// Record the playing item and position, when it plays on this instance's integrated
    /// player. Written to disk when the item changes, at most every `SAVE_INTERVAL` otherwise, or
    /// always when `force` is set.
    pub fn record_progress(&mut self, player: &PlayerState, force: bool) {
        let device = player
            .playback
            .as_ref()
            .and_then(|p| p.device.id.as_deref());
        if self.own_device.is_none() || device != self.own_device.as_deref() {
            return;
        }
        let Some((current_uri, progress_ms)) = playing_item(player) else {
            return;
        };
        let item_changed = self.session.current_uri.as_deref() != Some(current_uri.as_str());
        let due = self.last_saved.is_none_or(|t| t.elapsed() >= SAVE_INTERVAL);
        if !(force || item_changed || due) {
            return;
        }
        self.sync();
        self.session.current_uri = Some(current_uri);
        self.session.progress_ms = progress_ms;
        if self.session.is_empty() {
            self.last_saved = Some(Instant::now());
        } else {
            self.save();
        }
    }

    fn save(&mut self) {
        self.last_saved = Some(Instant::now());
        let result = serde_json::to_vec(&self.session)
            .map_err(anyhow::Error::from)
            .and_then(|data| std::fs::write(session_path(), data).map_err(anyhow::Error::from));
        if let Err(err) = result {
            tracing::warn!("Failed to save the session: {err:#}");
        }
        self.seen = modified();
    }
}
