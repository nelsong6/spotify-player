//! Drive and follow. spt drives the session: our radio, our position. When
//! something else drives Spotify (a phone, a Jam, any remote), the session follows it: the PCs
//! show what Spotify plays, and the radio stays where it was. Spotify pushes its connect state to
//! the session's player, so following costs no requests.

use crate::config;
use crate::state::SharedState;
use futures::StreamExt as _;
use librespot_core::{dealer::protocol::Message, Session};
use librespot_protocol::connect::ClusterUpdate;

/// What Spotify plays on the device driving it
#[derive(Clone, Debug)]
pub struct Remote {
    pub device: String,
    pub context_uri: Option<String>,
    pub track_uri: String,
    /// The position as of `at_ms`, in milliseconds since the Unix epoch
    pub position_ms: i64,
    pub at_ms: i64,
    pub is_playing: bool,
    /// The device's volume, in percent
    pub volume: Option<u32>,
}

#[derive(Debug, Default)]
pub struct Follow {
    /// Whether the session follows Spotify rather than driving it
    pub following: bool,
    /// What Spotify last said it plays
    pub remote: Option<Remote>,
}

/// Follow Spotify's connect state for as long as `session` lasts
pub fn watch(session: &Session, state: SharedState) {
    let updates = match session.dealer().listen_for(
        "hm://connect-state/v1/cluster",
        Message::from_raw::<ClusterUpdate>,
    ) {
        Ok(updates) => updates,
        Err(err) => {
            tracing::error!("Failed to listen for Spotify's connect state: {err:#}");
            return;
        }
    };
    tokio::spawn(async move {
        let mut updates = updates;
        while let Some(update) = updates.next().await {
            match update {
                Ok(update) => handle(&state, update),
                Err(err) => tracing::warn!("Failed to read Spotify's connect state: {err:#}"),
            }
        }
    });
}

/// Follow on purpose, until spt drives again
pub fn start(state: &SharedState) {
    state.follow.lock().following = true;
    tracing::info!("Following Spotify, as a PC asked");
    state.playback_changed.notify_one();
}

fn handle(state: &SharedState, update: ClusterUpdate) {
    let Some(cluster) = update.cluster.into_option() else {
        return;
    };
    let active = cluster.active_device_id.as_str();
    if active.is_empty() {
        return;
    }
    let info = cluster.device.get(active);
    let device = info.map(|device| device.name.clone()).unwrap_or_default();
    // Spotify's connect state carries volume from 0 to 65535
    let volume = info.map(|device| (u64::from(device.volume) * 100 / 65535) as u32);
    // either of the session's slots, which share the name
    let ours = device == config::get_config().app_config.device.name;
    let Some(player) = cluster.player_state.into_option() else {
        return;
    };
    let Some(track_uri) = player
        .track
        .as_ref()
        .map(|track| track.uri.clone())
        .filter(|uri| !uri.is_empty())
    else {
        return;
    };
    let now = chrono::Utc::now().timestamp_millis();
    let is_playing = player.is_playing && !player.is_paused;
    let position_ms = if is_playing {
        player.position_as_of_timestamp + (now - player.timestamp).max(0)
    } else {
        player.position_as_of_timestamp
    };
    let remote = Remote {
        device,
        context_uri: Some(player.context_uri.clone()).filter(|uri| !uri.is_empty()),
        track_uri,
        position_ms,
        at_ms: now,
        is_playing,
        volume,
    };

    let mut session = state.session.lock();
    session.sync();
    let held = session
        .session
        .holds_uris(remote.context_uri.as_deref(), &remote.track_uri);
    let mut follow = state.follow.lock();
    let seen = match (ours, held, is_playing) {
        (true, true, _) => Seen::Radio,
        (true, false, true) => Seen::OtherOnOurSpeaker,
        (true, false, false) => Seen::LeftoverOnOurSpeaker,
        (false, ..) => Seen::Elsewhere,
    };
    let following = follows(follow.following, seen);
    if following != follow.following {
        if following {
            tracing::info!("Following {}: something else drives Spotify", remote.device);
        } else {
            tracing::info!("spt drives the session again");
        }
    }
    follow.following = following;
    // catching up: Spotify playing inside the radio while the session follows moves the radio on
    if following && held {
        session.record_position(&remote.track_uri, remote.position_ms);
    }
    follow.remote = Some(remote);
    drop(follow);
    drop(session);
    state.playback_changed.notify_one();
}

/// What Spotify says plays on its active device
#[derive(Clone, Copy)]
enum Seen {
    /// The radio, on our speaker: spt driving
    Radio,
    /// Something else playing on our speaker, such as a Jam
    OtherOnOurSpeaker,
    /// Spotify's own leftover on our speaker, paused: nobody driving, and the saved session
    /// replaces it
    LeftoverOnOurSpeaker,
    /// Another device, playing or paused
    Elsewhere,
}

/// Whether the session follows, from whether it did and what Spotify now says plays
fn follows(following: bool, seen: Seen) -> bool {
    match seen {
        Seen::Radio => false,
        Seen::OtherOnOurSpeaker | Seen::Elsewhere => true,
        Seen::LeftoverOnOurSpeaker => following,
    }
}

#[cfg(test)]
mod tests {
    use super::{follows, Seen};

    #[test]
    fn follows_whatever_else_drives_spotify_until_the_radio_plays_on_our_speaker() {
        assert!(follows(false, Seen::Elsewhere));
        assert!(follows(false, Seen::OtherOnOurSpeaker));
        assert!(!follows(true, Seen::Radio));
        // Spotify's own leftover on our speaker changes nothing
        assert!(!follows(false, Seen::LeftoverOnOurSpeaker));
        assert!(follows(true, Seen::LeftoverOnOurSpeaker));
    }
}
