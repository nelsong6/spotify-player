//! Drive and follow. spt drives the session: our radio, our position. When
//! something else drives Spotify (a phone, a Jam, any remote), the session follows it: the PCs
//! show what Spotify plays, and the radio stays where it was. Spotify pushes its connect state to
//! the session's player, so following costs no requests. While following, a second player plays
//! along with the followed device, so the PCs visualize it and can play it aloud.

use crate::config;
use crate::state::SharedState;
use futures::StreamExt as _;
use librespot_core::{dealer::protocol::Message, Session};
use librespot_playback::player::{Player, PlayerEvent};
use librespot_protocol::connect::ClusterUpdate;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Notify;

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
    /// The device's last report, as Spotify passed it on: the position, and the moment it was
    /// taken
    reported_position_ms: i64,
    reported_at: i64,
}

#[derive(Debug, Default)]
pub struct Follow {
    /// Whether the session follows Spotify rather than driving it
    pub following: bool,
    /// What Spotify last said it plays
    pub remote: Option<Remote>,
    /// Woken on each change to what's followed
    changed: Arc<Notify>,
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
    state.follow.lock().changed.notify_one();
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
        reported_position_ms: reported.position_ms,
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
    follow.changed.notify_one();
    drop(follow);
    drop(session);
    state.playback_changed.notify_one();
}

/// Play along with the followed device on `player`, which shares the session's audio output, for
/// as long as this slot is the session. The radio's own player starting stops it first, through
/// [`radio_started`].
pub fn play_along(player: Arc<Player>, state: SharedState) {
    /// How often the player is checked against the followed device without a change
    const CHECK_INTERVAL: Duration = Duration::from_secs(5);
    /// How far the player may drift from the followed device before it seeks
    const DRIFT_MS: i64 = 400;
    /// How long a seek is given to land before the player is judged again
    const SEEK_SETTLE: Duration = Duration::from_secs(2);
    *ALONG.lock() = Some(Arc::clone(&player));
    let changed = Arc::clone(&state.follow.lock().changed);
    let mut events = player.get_player_event_channel();
    tokio::spawn(async move {
        crate::handover::until_session().await;
        let lead = output_lead_ms();
        tracing::info!("Playing along {lead} ms ahead, for the PCs' output delay");
        let mut playing: Option<Along> = None;
        let mut last_seek: Option<tokio::time::Instant> = None;
        loop {
            let wanted = {
                let follow = state.follow.lock();
                follow
                    .remote
                    .clone()
                    .filter(|remote| follow.following && !is_ours(remote))
            };
            let now = chrono::Utc::now().timestamp_millis();
            playing = match (wanted, playing.take()) {
                (None, Some(_)) => {
                    player.stop();
                    None
                }
                (None, None) => None,
                (Some(remote), along) => {
                    // the PCs play what the player plays `lead` later
                    let target = (remote.position_ms
                        + if remote.is_playing {
                            (now - remote.at_ms).max(0)
                        } else {
                            0
                        }
                        + lead)
                        .max(0);
                    match along.filter(|along| along.track_uri == remote.track_uri) {
                        Some(along) => {
                            let settled = last_seek.is_none_or(|at| at.elapsed() >= SEEK_SETTLE);
                            let drifted = along.started
                                && settled
                                && (along.position_at(now) - target).abs() > DRIFT_MS;
                            if drifted {
                                player.seek(u32::try_from(target).unwrap_or(u32::MAX));
                                last_seek = Some(tokio::time::Instant::now());
                            }
                            let toggled = along.is_playing != remote.is_playing;
                            if toggled {
                                if remote.is_playing {
                                    player.play();
                                } else {
                                    player.pause();
                                }
                            }
                            if drifted || toggled {
                                Some(Along::at(&remote, target, now, along.started))
                            } else {
                                Some(along)
                            }
                        }
                        None => match librespot_core::SpotifyUri::from_uri(&remote.track_uri) {
                            Ok(uri) => {
                                tracing::info!(
                                    "Playing along with {}: {} from {target} ms",
                                    remote.device,
                                    remote.track_uri
                                );
                                player.load(
                                    uri,
                                    remote.is_playing,
                                    u32::try_from(target).unwrap_or(u32::MAX),
                                );
                                Some(Along::at(&remote, target, now, false))
                            }
                            Err(err) => {
                                tracing::warn!(
                                    "Can't play along with {}: {err:#}",
                                    remote.track_uri
                                );
                                player.stop();
                                None
                            }
                        },
                    }
                }
            };
            tokio::select! {
                () = tokio::time::sleep(CHECK_INTERVAL) => {}
                () = changed.notified() => {}
                event = events.recv() => {
                    let Some(event) = event else { break };
                    // the player's own word on where it is, as of now: a song starts only once
                    // it has loaded, and a seek lands only once its audio has arrived
                    if let (
                        PlayerEvent::Playing { position_ms, .. }
                        | PlayerEvent::Paused { position_ms, .. }
                        | PlayerEvent::Seeked { position_ms, .. }
                        | PlayerEvent::PositionCorrection { position_ms, .. },
                        Some(along),
                    ) = (event, playing.as_mut())
                    {
                        along.position_ms = i64::from(position_ms);
                        along.at_ms = chrono::Utc::now().timestamp_millis();
                        along.started = true;
                    }
                }
            }
        }
    });
}

/// How far the session's player runs ahead of what the PCs play: Snapcast's buffer, and the pipe
/// into Snapcast, which stays full while the player waits on it
fn output_lead_ms() -> i64 {
    /// The session's audio: 44.1 kHz, 16-bit stereo
    const BYTES_PER_SECOND: i64 = 44_100 * 2 * 2;
    let buffer_ms = std::fs::read_to_string("/etc/snapserver.conf")
        .ok()
        .and_then(|conf| {
            conf.lines().find_map(|line| {
                let (key, value) = line.split_once('=')?;
                (key.trim() == "buffer").then(|| value.trim().parse::<i64>().ok())?
            })
        })
        .unwrap_or(0);
    // SAFETY: F_GETPIPE_SZ only reads the size of the pipe behind stdout
    #[cfg(target_os = "linux")]
    let pipe = i64::from(unsafe { libc::fcntl(libc::STDOUT_FILENO, libc::F_GETPIPE_SZ) }.max(0));
    #[cfg(not(target_os = "linux"))]
    let pipe = 0;
    buffer_ms + pipe * 1000 / BYTES_PER_SECOND
}

/// The player that plays along, stopped when the radio's own player starts, since both write the
/// session's one audio output
static ALONG: parking_lot::Mutex<Option<Arc<Player>>> = parking_lot::Mutex::new(None);

/// The radio's own player started: playing along stops at once, before following ends
pub fn radio_started() {
    if let Some(player) = ALONG.lock().as_ref() {
        player.stop();
    }
}

/// Whether `remote` is the session's own player, which needs no one playing along
fn is_ours(remote: &Remote) -> bool {
    remote.device == config::get_config().app_config.device.name
}

/// What the player playing along was last set to
struct Along {
    track_uri: String,
    /// Whether the player has said where it is since loading the song
    started: bool,
    position_ms: i64,
    at_ms: i64,
    is_playing: bool,
}

impl Along {
    fn at(remote: &Remote, position_ms: i64, now: i64, started: bool) -> Self {
        Self {
            track_uri: remote.track_uri.clone(),
            started,
            position_ms,
            at_ms: now,
            is_playing: remote.is_playing,
        }
    }

    fn position_at(&self, now: i64) -> i64 {
        self.position_ms
            + if self.is_playing {
                (now - self.at_ms).max(0)
            } else {
                0
            }
    }
}

/// A device's position as Spotify passes it on: the position as of a moment
struct Report {
    position_ms: i64,
    at: i64,
    duration_ms: i64,
}

/// Where the followed device is in its song now. A device reports its position when its state
/// changes (a new song, a pause, a resume, a seek), but a phone can leave the moment it gives for
/// that position stale, minutes old, so counting from that moment runs past the song's end. A
/// report is taken as of when it arrives instead, unless its moment is that recent anyway, and the
/// position moves on from there while the device plays. Updates that bring no new report (another
/// device's change) leave the count running.
fn position(
    last: Option<&Remote>,
    track_uri: &str,
    reported: &Report,
    is_playing: bool,
    now: i64,
) -> i64 {
    /// How far a report's moment may lag its arrival and still be taken as given
    const RECENT_MS: i64 = 5_000;
    let new_report = last.is_none_or(|last| {
        last.track_uri != track_uri
            || last.is_playing != is_playing
            || last.reported_position_ms != reported.position_ms
            || last.reported_at != reported.at
    });
    let position = if new_report {
        let lag = now - reported.at;
        if is_playing && (0..=RECENT_MS).contains(&lag) {
            reported.position_ms + lag
        } else {
            reported.position_ms
        }
    } else {
        last.map_or(reported.position_ms, |last| {
            let elapsed = if last.is_playing { now - last.at_ms } else { 0 };
            last.position_ms + elapsed.max(0)
        })
    };
    if reported.duration_ms > 0 {
        position.clamp(0, reported.duration_ms)
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
        reported: &Report,
    ) -> Remote {
        Remote {
            device: "Pixel 6".to_string(),
            context_uri: None,
            track_uri: track_uri.to_string(),
            position_ms,
            at_ms,
            is_playing,
            volume: None,
            reported_position_ms: reported.position_ms,
            reported_at: reported.at,
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
    fn a_recent_report_counts_from_its_moment() {
        assert_eq!(
            position(None, "a", &report(10_000, 1_000), true, 4_000),
            13_000
        );
        assert_eq!(
            position(None, "a", &report(10_000, 1_000), false, 4_000),
            10_000
        );
    }

    #[test]
    fn a_report_with_a_stale_moment_counts_from_its_arrival() {
        // a new song, its moment minutes old: as of now, not run past the end
        let was = last("a", 120_000, 50_000, true, &report(0, 1_000));
        assert_eq!(
            position(Some(&was), "b", &report(0, 1_000), true, 600_000),
            0
        );
        // a resume at 2:16 with the same stale moment
        let paused = last("b", 136_000, 50_000, false, &report(136_000, 1_000));
        assert_eq!(
            position(Some(&paused), "b", &report(136_000, 1_000), true, 600_000),
            136_000
        );
    }

    #[test]
    fn an_update_without_a_new_report_leaves_the_count_running() {
        let reported = report(30_000, 1_000);
        let playing = last("a", 30_000, 50_000, true, &reported);
        assert_eq!(
            position(Some(&playing), "a", &reported, true, 60_000),
            40_000
        );
        let paused = last("a", 30_000, 50_000, false, &reported);
        assert_eq!(
            position(Some(&paused), "a", &reported, false, 60_000),
            30_000
        );
    }

    #[test]
    fn a_position_never_runs_past_the_songs_end() {
        let reported = report(290_000, 1_000);
        let playing = last("a", 295_000, 50_000, true, &reported);
        assert_eq!(
            position(Some(&playing), "a", &reported, true, 80_000),
            300_000
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
