//! The shared screen: every PC shows the same page, popup and visualization. The session on the
//! NAS keeps what the screen shows and passes it to every PC. One PC at a time is in control and
//! changes the screen; the others show it locked until space takes control. The session also
//! passes on its playback, lists the PCs playing its audio, and mutes one when another asks.

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
    /// Follow Spotify instead of driving it, until spt drives again
    Follow,
}

/// A PC playing the session's audio, as the cast server knows it
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct Pc {
    /// The cast server's id for it: its host name
    pub id: String,
    pub name: String,
    pub muted: bool,
}

/// What the session plays, which the PCs show instead of asking Spotify
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct SessionPlayback {
    pub playback: Option<rspotify::model::CurrentPlaybackContext>,
    pub queue: Option<rspotify::model::CurrentUserQueue>,
    /// When the playback's progress was read, in milliseconds since the Unix epoch
    pub at_ms: i64,
    /// `playback` is where the session left off, which play resumes, because Spotify has
    /// forgotten the idle device's own
    #[serde(default)]
    pub left_off: bool,
    /// Something else drives Spotify and `playback` is what it plays; play takes control back and
    /// resumes the radio
    #[serde(default)]
    pub following: bool,
}

/// From the session to every PC, one JSON object per line, sent whenever any of it changes
#[derive(Serialize, Deserialize, Clone, Default)]
struct FromSession {
    controller: Option<String>,
    /// `None` until a PC first takes control
    screen: Option<serde_json::Value>,
    #[serde(default)]
    pcs: Vec<Pc>,
    /// A [`SessionPlayback`]
    #[serde(default)]
    playback: Option<serde_json::Value>,
}

/// The session's side: keeps the screen, saved to `path` so a restart keeps it, and passes it on.
/// The screen is opaque here, so the session needs no update when its contents change.
#[cfg(feature = "streaming")]
pub mod session;

/// A PC's side: follows the session's screen while locked, and sends its own while in control.
/// With the session out of reach, the PC is its own screen.
#[cfg(feature = "cast")]
pub mod pc;
