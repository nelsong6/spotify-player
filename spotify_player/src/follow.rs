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
    /// When the device last reported its position, as Spotify passed it on
    reported_at: i64,
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
    // temporary (OpenProject #89): which of Spotify's position fields stays current for a phone
    tracing::info!(
        "Connect state from {device}: track={track_uri} playing={is_playing} \
         position={} position_as_of_timestamp={} timestamp={} duration={} \
         server_timestamp_ms={} changed_timestamp_ms={} now={now}",
        player.position,
        player.position_as_of_timestamp,
        player.timestamp,
        player.duration,
        cluster.server_timestamp_ms,
        cluster.changed_timestamp_ms,
    );
    let reported = Report {
        position_ms: player.position_as_of_timestamp,
        at: player.timestamp,
        duration_ms: player.duration,
    };

    let context_uri = Some(player.context_uri.clone()).filter(|uri| !uri.is_empty());

    let mut session = state.session.lock();
    session.sync();
    let held = session
        .session
        .holds_uris(context_uri.as_deref(), &track_uri);
    let mut follow = state.follow.lock();
    let remote = Remote {
        device,
        context_uri,
        position_ms: position(
            follow.remote.as_ref(),
            &track_uri,
            &reported,
            is_playing,
            now,
        ),
        track_uri,
        at_ms: now,
        is_playing,
        volume,
        reported_at: reported.at,
    };
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

/// A device's position as Spotify passes it on: the position as of a moment
struct Report {
    position_ms: i64,
    at: i64,
    duration_ms: i64,
}

/// Where the followed device is in its song now. A phone can go on reporting a new song, or play
/// and pause, without updating when its position was taken; that stale moment carried forward
/// runs the position past the song's end. A report counts only when its moment changed: otherwise
/// the position is the session's own, from 0 at a new song and moving while it plays.
fn position(
    last: Option<&Remote>,
    track_uri: &str,
    reported: &Report,
    is_playing: bool,
    now: i64,
) -> i64 {
    let fresh = last.is_none_or(|last| last.reported_at != reported.at);
    let position = if fresh {
        let elapsed = if is_playing {
            (now - reported.at).max(0)
        } else {
            0
        };
        reported.position_ms + elapsed
    } else {
        match last {
            Some(last) if last.track_uri == track_uri => {
                let elapsed = if last.is_playing { now - last.at_ms } else { 0 };
                last.position_ms + elapsed.max(0)
            }
            _ => 0,
        }
    };
    if reported.duration_ms > 0 && position > reported.duration_ms {
        // past the song's end: nothing reported here is usable
        0
    } else {
        position.max(0)
    }
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
    use super::{follows, position, Remote, Report, Seen};

    fn last(
        track_uri: &str,
        position_ms: i64,
        at_ms: i64,
        is_playing: bool,
        reported_at: i64,
    ) -> Remote {
        Remote {
            device: "Pixel 6".to_string(),
            context_uri: None,
            track_uri: track_uri.to_string(),
            position_ms,
            at_ms,
            is_playing,
            volume: None,
            reported_at,
        }
    }

    fn report(position_ms: i64, at: i64) -> Report {
        Report {
            position_ms,
            at,
            duration_ms: 300_000,
        }
    }

    #[test]
    fn a_fresh_report_is_carried_forward_while_playing() {
        assert_eq!(
            position(None, "a", &report(10_000, 1_000), true, 6_000),
            15_000
        );
        assert_eq!(
            position(None, "a", &report(10_000, 1_000), false, 6_000),
            10_000
        );
    }

    #[test]
    fn a_stale_report_gives_way_to_the_sessions_own_estimate() {
        // the phone moved to a new song without a new report: from 0
        let was = last("a", 120_000, 50_000, true, 1_000);
        assert_eq!(
            position(Some(&was), "b", &report(0, 1_000), true, 60_000),
            0
        );
        // the same song, still unreported: the estimate moves on while playing
        assert_eq!(
            position(Some(&was), "a", &report(0, 1_000), true, 60_000),
            130_000
        );
        // and holds while paused
        let paused = last("a", 120_000, 50_000, false, 1_000);
        assert_eq!(
            position(Some(&paused), "a", &report(0, 1_000), true, 60_000),
            120_000
        );
    }

    #[test]
    fn a_position_past_the_songs_end_is_never_used() {
        assert_eq!(
            position(None, "a", &report(290_000, 1_000), true, 600_000),
            0
        );
    }

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
