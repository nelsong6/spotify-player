//! The relay: the PCs' only way to Spotify. The session holds the house's one Spotify sign-in; a
//! PC sends it the Web API requests it would have sent Spotify, plus the lyrics, radios and covers
//! that need the session's sign-in or reach. Every request carries a key the session keeps beside
//! the saved session, in a folder only Nelson's NAS account can read, so a PC proves itself
//! through his Windows login. Deleting the key revokes it; the session makes a new one.

use std::path::PathBuf;

/// The session's port for the relay
pub const PORT: u16 = 1707;
/// The header carrying the key
const KEY_HEADER: &str = "x-spt-key";
/// Where the Web API's endpoints start, on Spotify and on the relay
const API_PREFIX: &str = "/v1/";

/// The key's file, beside the saved session
fn key_path() -> PathBuf {
    crate::config::get_config()
        .session_folder()
        .join("relay.key")
}

/// The session's side: serves the relay while this slot is the active one, and keeps the
/// playlists from there, so only one slot ever does either.
#[cfg(feature = "streaming")]
pub mod session;

/// A PC's side: the session's address and the key, sent with every request
#[cfg(feature = "cast")]
pub mod pc;
