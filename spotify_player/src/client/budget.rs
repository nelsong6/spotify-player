//! How the session spends Spotify's Web API: every request waits its turn under a budget held well
//! below any rate Spotify has refused, and library answers are kept on disk until refreshed, so
//! loading the library again (a restart, a handover, another PC) asks Spotify nothing.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::Duration;

use base64::Engine as _;
use tokio::sync::Mutex;
use tokio::time::Instant;

/// Requests sent in any 30 seconds, Spotify's own window
const PER_WINDOW: usize = 10;
const WINDOW: Duration = Duration::from_secs(30);
/// Requests sent in any hour
const PER_HOUR: usize = 200;
const HOUR: Duration = Duration::from_secs(3600);

/// Endpoints whose answers are the library: they change only when spt changes them or on a
/// refresh. Playback, devices, the queue, search and listening history stay live.
const LIBRARY: &[&str] = &[
    "me/tracks",
    "me/playlists",
    "me/following",
    "me/albums",
    "me/shows",
    "playlists/",
    "albums/",
    "artists/",
    "shows/",
    "tracks/",
];

/// Requests already sent, newest last, for as long as the budget counts them
static SENT: Mutex<VecDeque<Instant>> = Mutex::const_new(VecDeque::new());

/// Wait until the budget allows another request, then count it as sent
pub(super) async fn wait_turn(what: &str) {
    loop {
        let wait = {
            let mut sent = SENT.lock().await;
            let now = Instant::now();
            while sent.front().is_some_and(|at| now - *at >= HOUR) {
                sent.pop_front();
            }
            let in_window = sent.iter().filter(|at| now - **at < WINDOW).count();
            let window_wait =
                (in_window >= PER_WINDOW).then(|| sent[sent.len() - PER_WINDOW] + WINDOW - now);
            let hour_wait =
                (sent.len() >= PER_HOUR).then(|| sent[sent.len() - PER_HOUR] + HOUR - now);
            let Some(wait) = window_wait.into_iter().chain(hour_wait).max() else {
                sent.push_back(now);
                return;
            };
            wait
        };
        tracing::info!(
            "Holding {what} {} ms for the request budget",
            wait.as_millis()
        );
        tokio::time::sleep(wait).await;
    }
}

/// Where library answers are kept: beside the saved session, so both of the session's slots share
/// them
static KEPT: OnceLock<PathBuf> = OnceLock::new();

/// Keep library answers in `folder`, for this process
pub(super) fn keep_library_in(folder: PathBuf) {
    if let Err(err) = std::fs::create_dir_all(&folder) {
        tracing::warn!(
            "Failed to make the library folder {}: {err:#}",
            folder.display()
        );
        return;
    }
    let _ = KEPT.set(folder);
}

/// An endpoint relative to the API, with its query in a fixed order, since rspotify sends a
/// query's pairs in any order
pub(super) fn endpoint_key(endpoint: &str) -> String {
    let Some((path, query)) = endpoint.split_once('?') else {
        return endpoint.to_string();
    };
    let mut pairs: Vec<&str> = query.split('&').collect();
    pairs.sort_unstable();
    format!("{path}?{}", pairs.join("&"))
}

pub(super) fn is_library(key: &str) -> bool {
    LIBRARY.iter().any(|prefix| key.starts_with(prefix))
}

fn file(folder: &Path, key: &str) -> PathBuf {
    folder.join(base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(key))
}

/// The kept answer for a library endpoint
pub(super) fn kept(key: &str) -> Option<Vec<u8>> {
    let folder = KEPT.get()?;
    std::fs::read(file(folder, key)).ok()
}

pub(super) fn keep(key: &str, body: &[u8]) {
    let Some(folder) = KEPT.get() else {
        return;
    };
    let path = file(folder, key);
    // a name too long for the file system is left unkept
    if path.file_name().is_some_and(|name| name.len() > 240) {
        return;
    }
    if let Err(err) = std::fs::write(&path, body) {
        tracing::warn!("Failed to keep {key}: {err:#}");
    }
}

/// spt changed something on Spotify: forget the kept answers it may have changed, those under the
/// changed resource (`playlists/<id>`, `me/tracks`, ...) and the list of playlists
pub(super) fn changed(key: &str) {
    let Some(folder) = KEPT.get() else {
        return;
    };
    let path = key.split('?').next().unwrap_or(key);
    let resource: String = path.split('/').take(2).collect::<Vec<_>>().join("/");
    forget(folder, |kept| {
        kept.starts_with(&resource) || kept.starts_with("me/playlists")
    });
}

/// Forget every kept library answer, so the library loads from Spotify again
pub fn refresh_library() {
    if let Some(folder) = KEPT.get() {
        forget(folder, |_| true);
        tracing::info!("Refreshing the library: kept answers forgotten");
    }
}

fn forget(folder: &Path, matches: impl Fn(&str) -> bool) {
    let Ok(entries) = std::fs::read_dir(folder) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let key = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(name.as_encoded_bytes())
            .ok()
            .and_then(|key| String::from_utf8(key).ok());
        if key.is_some_and(|key| matches(&key)) {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{changed, endpoint_key, is_library, keep, keep_library_in, kept, refresh_library};

    #[test]
    fn keeps_answers_until_spt_changes_them_or_a_refresh() {
        let folder = std::env::temp_dir().join(format!("spt-library-{}", std::process::id()));
        keep_library_in(folder.clone());
        keep("playlists/abc/items?offset=0", b"items");
        keep("playlists/xyz/items?offset=0", b"other");
        keep("me/playlists?limit=50", b"list");
        keep("me/tracks?limit=50", b"liked");
        assert_eq!(
            kept("playlists/abc/items?offset=0").as_deref(),
            Some(&b"items"[..])
        );

        // removing songs from one playlist forgets that playlist and the list of playlists
        changed("playlists/abc/items");
        assert_eq!(kept("playlists/abc/items?offset=0"), None);
        assert_eq!(kept("me/playlists?limit=50"), None);
        assert_eq!(
            kept("playlists/xyz/items?offset=0").as_deref(),
            Some(&b"other"[..])
        );
        assert_eq!(kept("me/tracks?limit=50").as_deref(), Some(&b"liked"[..]));

        refresh_library();
        assert_eq!(kept("playlists/xyz/items?offset=0"), None);
        assert_eq!(kept("me/tracks?limit=50"), None);
        std::fs::remove_dir_all(&folder).unwrap();
    }

    #[test]
    fn keeps_the_library_and_nothing_live() {
        assert!(is_library("me/tracks?market=from_token&offset=0&limit=50"));
        assert!(is_library(
            "playlists/7surudSXNGQsQLp3Fh7DtY/items?offset=0"
        ));
        assert!(!is_library("me/player/devices"));
        assert!(!is_library("me/player/queue"));
        assert!(!is_library("me/player/recently-played?limit=50"));
        assert!(!is_library("search?q=abc&type=track"));
    }

    #[test]
    fn a_query_in_any_order_is_one_key() {
        assert_eq!(
            endpoint_key("me/tracks?offset=50&limit=50"),
            endpoint_key("me/tracks?limit=50&offset=50")
        );
    }
}
