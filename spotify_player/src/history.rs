//! The radio's play history: a row for every song the session's player plays, in SQLite beside the
//! session's slots, kept forever. A row is written as the song starts and completed as it ends,
//! so a song cut off by a restart is kept too, marked `cut_off` once the next session plays.

use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use anyhow::Context as _;
use librespot_core::SpotifyUri;
use librespot_metadata::audio::{AudioItem, UniqueFields};
use librespot_playback::player::PlayerEvent;
use parking_lot::Mutex;
use rusqlite::{params, Connection};

use crate::state::SharedState;

const FILE: &str = "history.db";
/// How often a playing song's progress is saved, bounding what a restart loses
const CHECKPOINT: Duration = Duration::from_secs(10);
const BUSY_TIMEOUT: Duration = Duration::from_secs(5);

/// The schema's versions, in order; `PRAGMA user_version` says how many have been applied
const MIGRATIONS: &[&str] = &["
    CREATE TABLE plays (
        id INTEGER PRIMARY KEY,
        -- UTC, RFC 3339 with milliseconds
        started_at TEXT NOT NULL,
        ended_at TEXT,
        -- finished, skipped, stopped or cut_off (the process ended); NULL while it plays
        end_reason TEXT,
        start_position_ms INTEGER NOT NULL,
        played_ms INTEGER NOT NULL,
        track_uri TEXT NOT NULL,
        name TEXT,
        -- JSON arrays of {uri, name}
        artists TEXT,
        album TEXT,
        -- JSON array of names
        album_artists TEXT,
        duration_ms INTEGER,
        explicit INTEGER,
        popularity INTEGER,
        -- what Spotify played it from: the radio's playlist, an album, ...
        context_uri TEXT,
        radio_uri TEXT,
        radio_kind TEXT,
        -- the song's place in the radio's list, from 0
        radio_position INTEGER,
        continue_mode TEXT,
        following INTEGER NOT NULL,
        -- JSON array of the names of the PCs playing it aloud as it started
        heard_by TEXT NOT NULL,
        release TEXT
    );
    CREATE INDEX plays_started_at ON plays (started_at);
    CREATE INDEX plays_track_uri ON plays (track_uri);
"];

struct Place {
    dir: PathBuf,
    release: Option<String>,
}

static PLACE: OnceLock<Place> = OnceLock::new();

/// Keep the history in `dir`, recording `release` as what played each song
pub fn init(dir: &Path, release: Option<&str>) {
    let _ = PLACE.set(Place {
        dir: dir.to_path_buf(),
        release: release.map(str::to_string),
    });
}

/// How a play ended
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum End {
    Finished,
    Skipped,
    Stopped,
}

impl End {
    fn as_str(self) -> &'static str {
        match self {
            End::Finished => "finished",
            End::Skipped => "skipped",
            End::Stopped => "stopped",
        }
    }
}

/// What's known about a play as it starts
#[derive(Default, Debug)]
struct Start {
    track_uri: String,
    position_ms: u32,
    name: Option<String>,
    artists: Option<serde_json::Value>,
    album: Option<String>,
    album_artists: Option<serde_json::Value>,
    duration_ms: Option<u32>,
    explicit: Option<bool>,
    popularity: Option<u8>,
    context_uri: Option<String>,
    radio_uri: Option<String>,
    radio_kind: Option<String>,
    radio_position: Option<i64>,
    continue_mode: Option<String>,
    following: bool,
    heard_by: Vec<String>,
}

struct Current {
    id: i64,
    track: SpotifyUri,
    played_ms: u64,
    playing_since: Option<Instant>,
}

impl Current {
    fn played_ms(&self) -> u64 {
        self.played_ms
            + self
                .playing_since
                .map_or(0, |since| since.elapsed().as_millis() as u64)
    }
}

struct Store {
    conn: Connection,
    release: Option<String>,
    /// Whether rows a previous process left playing have been marked cut off
    swept: bool,
}

impl Store {
    fn open(path: &Path, release: Option<String>) -> anyhow::Result<Self> {
        let conn = Connection::open(path).with_context(|| format!("open {}", path.display()))?;
        Self::new(conn, release)
    }

    fn new(conn: Connection, release: Option<String>) -> anyhow::Result<Self> {
        conn.busy_timeout(BUSY_TIMEOUT)?;
        // both slots open the database; WAL lets the waiting one read while the session writes
        conn.pragma_update(None, "journal_mode", "WAL")?;
        migrate(&conn)?;
        Ok(Self {
            conn,
            release,
            swept: false,
        })
    }

    fn start(&mut self, start: &Start) -> anyhow::Result<i64> {
        if !self.swept {
            // only the session plays, so a row still playing is one an ended process left
            self.conn.execute(
                "UPDATE plays SET end_reason = 'cut_off' WHERE end_reason IS NULL",
                [],
            )?;
            self.swept = true;
        }
        self.conn.execute(
            "INSERT INTO plays (started_at, start_position_ms, played_ms, track_uri, name, \
             artists, album, album_artists, duration_ms, explicit, popularity, context_uri, \
             radio_uri, radio_kind, radio_position, continue_mode, following, heard_by, release) \
             VALUES (?1, ?2, 0, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, \
             ?17, ?18)",
            params![
                now(),
                start.position_ms,
                start.track_uri,
                start.name,
                start.artists.as_ref().map(ToString::to_string),
                start.album,
                start.album_artists.as_ref().map(ToString::to_string),
                start.duration_ms,
                start.explicit,
                start.popularity,
                start.context_uri,
                start.radio_uri,
                start.radio_kind,
                start.radio_position,
                start.continue_mode,
                start.following,
                serde_json::to_string(&start.heard_by)?,
                self.release,
            ],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    fn progress(&self, id: i64, played_ms: u64) -> anyhow::Result<()> {
        self.conn.execute(
            "UPDATE plays SET played_ms = ?2 WHERE id = ?1",
            params![id, ms(played_ms)],
        )?;
        Ok(())
    }

    fn end(&self, id: i64, played_ms: u64, end: End) -> anyhow::Result<()> {
        self.conn.execute(
            "UPDATE plays SET played_ms = ?2, ended_at = ?3, end_reason = ?4 WHERE id = ?1",
            params![id, ms(played_ms), now(), end.as_str()],
        )?;
        Ok(())
    }
}

fn migrate(conn: &Connection) -> anyhow::Result<()> {
    let applied: i64 = conn.pragma_query_value(None, "user_version", |row| row.get(0))?;
    let latest = i64::try_from(MIGRATIONS.len())?;
    if applied > latest {
        anyhow::bail!(
            "the history's schema is version {applied}, newer than this release's {latest}"
        );
    }
    for (version, migration) in (1..).zip(MIGRATIONS).skip(usize::try_from(applied)?) {
        let tx = conn.unchecked_transaction()?;
        tx.execute_batch(migration)?;
        tx.pragma_update(None, "user_version", version)?;
        tx.commit()?;
    }
    Ok(())
}

/// Milliseconds as SQLite stores integers
fn ms(ms: u64) -> i64 {
    i64::try_from(ms).unwrap_or(i64::MAX)
}

fn now() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

/// Records the session player's plays from its events
pub struct Recorder {
    store: Option<Store>,
    current: Option<Current>,
    /// The song the player loaded last, for its details as it starts
    loaded: Option<Box<AudioItem>>,
}

impl Recorder {
    /// A recorder for one player connection, saving progress while it lasts. Without a history
    /// folder (outside the home cast) it records nothing.
    pub fn new() -> Arc<Mutex<Self>> {
        let store = PLACE.get().map(|place| {
            let path = place.dir.join(FILE);
            Store::open(&path, place.release.clone())
        });
        let store = match store {
            None => None,
            Some(Ok(store)) => {
                crate::problem::clear(crate::problem::Kind::History);
                Some(store)
            }
            Some(Err(err)) => {
                failed(&err);
                None
            }
        };
        let recorder = Arc::new(Mutex::new(Self {
            store,
            current: None,
            loaded: None,
        }));
        let weak = Arc::downgrade(&recorder);
        tokio::spawn(async move {
            let mut ticks = tokio::time::interval(CHECKPOINT);
            loop {
                ticks.tick().await;
                let Some(recorder) = weak.upgrade() else {
                    return;
                };
                recorder.lock().checkpoint();
            }
        });
        recorder
    }

    /// Handle a player event. `context_uri` is what the playback is playing from.
    pub fn handle(&mut self, event: &PlayerEvent, state: &SharedState, context_uri: &str) {
        if self.store.is_none() {
            return;
        }
        match event {
            PlayerEvent::TrackChanged { audio_item } => self.loaded = Some(audio_item.clone()),
            PlayerEvent::Playing {
                track_id,
                position_ms,
                ..
            } => {
                if self.current.as_ref().is_some_and(|c| &c.track != track_id) {
                    self.end(End::Skipped);
                }
                if self.current.is_none() {
                    let start = self.describe(track_id, *position_ms, state, context_uri);
                    match self.store.as_mut().map(|store| store.start(&start)) {
                        Some(Ok(id)) => {
                            self.current = Some(Current {
                                id,
                                track: track_id.clone(),
                                played_ms: 0,
                                playing_since: None,
                            });
                        }
                        Some(Err(err)) => failed(&err),
                        None => {}
                    }
                }
                if let Some(current) = self.current.as_mut() {
                    current.playing_since.get_or_insert_with(Instant::now);
                }
            }
            PlayerEvent::Paused { .. } => {
                if let Some(current) = self.current.as_mut() {
                    current.played_ms = current.played_ms();
                    current.playing_since = None;
                }
                self.checkpoint();
            }
            PlayerEvent::EndOfTrack { .. } => self.end(End::Finished),
            PlayerEvent::Stopped { .. } | PlayerEvent::SessionDisconnected { .. } => {
                self.end(End::Stopped);
            }
            _ => {}
        }
    }

    fn describe(
        &self,
        track: &SpotifyUri,
        position_ms: u32,
        state: &SharedState,
        context_uri: &str,
    ) -> Start {
        let track_uri = track.to_uri().unwrap_or_default();
        let mut start = Start {
            position_ms,
            context_uri: Some(context_uri.to_string()).filter(|uri| !uri.is_empty()),
            following: state.follow.lock().following,
            heard_by: crate::listeners::pcs()
                .borrow()
                .iter()
                .filter(|pc| !pc.muted)
                .map(|pc| pc.name.clone())
                .collect(),
            ..Start::default()
        };
        {
            let session = state.session.lock();
            let saved = &session.session;
            start.continue_mode = Some(saved.continue_mode.label().to_string());
            if let Some(radio) = &saved.radio {
                start.radio_uri = Some(radio.uri.clone());
                start.radio_kind = Some(radio.kind.clone());
                start.radio_position = saved
                    .uris
                    .iter()
                    .position(|uri| *uri == track_uri)
                    .and_then(|i| i64::try_from(i).ok());
            }
        }
        if let Some(item) = self.loaded.as_ref().filter(|item| &item.track_id == track) {
            start.name = Some(item.name.clone());
            start.duration_ms = Some(item.duration_ms);
            start.explicit = Some(item.is_explicit);
            if let UniqueFields::Track {
                artists,
                album,
                album_artists,
                popularity,
                ..
            } = &item.unique_fields
            {
                start.artists = Some(serde_json::Value::Array(
                    artists
                        .iter()
                        .map(|artist| {
                            serde_json::json!({
                                "uri": artist.id.to_uri().unwrap_or_default(),
                                "name": artist.name,
                            })
                        })
                        .collect(),
                ));
                start.album = Some(album.clone());
                start.album_artists = Some(serde_json::json!(album_artists));
                start.popularity = Some(*popularity);
            }
        }
        start.track_uri = track_uri;
        start
    }

    fn checkpoint(&mut self) {
        let (Some(store), Some(current)) = (self.store.as_ref(), self.current.as_ref()) else {
            return;
        };
        if let Err(err) = store.progress(current.id, current.played_ms()) {
            failed(&err);
        }
    }

    fn end(&mut self, end: End) {
        let (Some(store), Some(current)) = (self.store.as_ref(), self.current.take()) else {
            return;
        };
        if let Err(err) = store.end(current.id, current.played_ms(), end) {
            failed(&err);
        }
    }
}

fn failed(err: &anyhow::Error) {
    tracing::error!("Failed to keep the play history: {err:#}");
    crate::problem::set(
        crate::problem::Kind::History,
        format!(
            "The play history isn't being kept ({})",
            crate::problem::reason(err)
        ),
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store() -> Store {
        Store::new(Connection::open_in_memory().unwrap(), Some("v1".into())).unwrap()
    }

    fn row(store: &Store, id: i64) -> (Option<String>, i64, Option<String>) {
        store
            .conn
            .query_row(
                "SELECT end_reason, played_ms, release FROM plays WHERE id = ?1",
                [id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap()
    }

    fn start(uri: &str) -> Start {
        Start {
            track_uri: uri.into(),
            heard_by: vec!["nelsonpc".into()],
            ..Start::default()
        }
    }

    #[test]
    fn a_play_is_kept_from_its_start_to_its_end() {
        let mut store = store();
        let id = store.start(&start("spotify:track:a")).unwrap();
        assert_eq!(row(&store, id), (None, 0, Some("v1".into())));
        store.progress(id, 5_000).unwrap();
        assert_eq!(row(&store, id).1, 5_000);
        store.end(id, 240_000, End::Finished).unwrap();
        assert_eq!(row(&store, id).0.as_deref(), Some("finished"));
        assert_eq!(row(&store, id).1, 240_000);
    }

    #[test]
    fn a_play_an_ended_process_left_is_cut_off_when_the_next_starts() {
        let mut first = store();
        let left = first.start(&start("spotify:track:a")).unwrap();
        // the next process opens the same database
        first.swept = false;
        let next = first.start(&start("spotify:track:b")).unwrap();
        assert_eq!(row(&first, left).0.as_deref(), Some("cut_off"));
        assert_eq!(row(&first, next).0, None);
    }

    #[test]
    fn migrating_twice_changes_nothing() {
        let store = store();
        migrate(&store.conn).unwrap();
        let version: i64 = store
            .conn
            .pragma_query_value(None, "user_version", |r| r.get(0))
            .unwrap();
        assert_eq!(version, MIGRATIONS.len() as i64);
    }

    #[test]
    fn a_newer_schema_is_refused() {
        let conn = Connection::open_in_memory().unwrap();
        conn.pragma_update(None, "user_version", MIGRATIONS.len() as i64 + 1)
            .unwrap();
        assert!(Store::new(conn, None).is_err());
    }
}
