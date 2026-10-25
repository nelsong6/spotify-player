//! Reports what the integrated player plays to Spotify, as Spotify's own apps do, so its plays
//! show in the account's listening history. `librespot` doesn't send these reports itself; the
//! events and their format are the ones librespot-java sends to `hm://event-service/v1/events`.

use librespot_core::{packet::PacketType, session::Session, SpotifyUri};
use librespot_playback::player::PlayerEvent;
use librespot_protocol::mercury::{Header, UserField};
use protobuf::Message;
use rand::RngExt;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

const EVENTS_URI: &str = "hm://event-service/v1/events";
/// How long a skipped song has to play to count as played, as in Spotify's apps
const MIN_PLAYED_MS: u64 = 30_000;

/// The song playing on the integrated player and how long it has played
struct Listen {
    track: SpotifyUri,
    started_at_ms: u128,
    duration_ms: u32,
    played_ms: u64,
    playing_since: Option<Instant>,
}

impl Listen {
    fn played_ms(&self) -> u64 {
        self.played_ms
            + self
                .playing_since
                .map_or(0, |since| since.elapsed().as_millis() as u64)
    }
}

/// Follows the integrated player's events and reports each song once it ends
#[derive(Default)]
pub struct PlayReporter {
    listen: Option<Listen>,
    sequence: u32,
}

impl PlayReporter {
    /// Handle a player event. `context_uri` is what the playback is playing from.
    pub fn handle(&mut self, event: &PlayerEvent, session: &Session, context_uri: &str) {
        match event {
            PlayerEvent::TrackChanged { audio_item } => {
                if let Some(listen) = self.listen.as_mut() {
                    if listen.track == audio_item.track_id {
                        listen.duration_ms = audio_item.duration_ms;
                    }
                }
            }
            PlayerEvent::Playing { track_id, .. } => {
                if self.listen.as_ref().is_some_and(|l| &l.track != track_id) {
                    self.finish(session, context_uri, "fwdbtn");
                }
                let listen = self.listen.get_or_insert_with(|| Listen {
                    track: track_id.clone(),
                    started_at_ms: now_ms(),
                    duration_ms: 0,
                    played_ms: 0,
                    playing_since: None,
                });
                listen.playing_since.get_or_insert_with(Instant::now);
            }
            PlayerEvent::Paused { .. } => {
                if let Some(listen) = self.listen.as_mut() {
                    listen.played_ms = listen.played_ms();
                    listen.playing_since = None;
                }
            }
            PlayerEvent::EndOfTrack { .. } => self.finish(session, context_uri, "trackdone"),
            PlayerEvent::Stopped { .. } => self.finish(session, context_uri, "endplay"),
            _ => {}
        }
    }

    fn finish(&mut self, session: &Session, context_uri: &str, reason_end: &str) {
        let Some(listen) = self.listen.take() else {
            return;
        };
        let played_ms = listen.played_ms();
        if reason_end != "trackdone" && played_ms < MIN_PLAYED_MS {
            return;
        }
        if let Err(err) = self.report(session, context_uri, &listen, played_ms, reason_end) {
            tracing::warn!("Failed to report a play to Spotify: {err:#}");
        }
    }

    fn report(
        &mut self,
        session: &Session,
        context_uri: &str,
        listen: &Listen,
        played_ms: u64,
        reason_end: &str,
    ) -> anyhow::Result<()> {
        let SpotifyUri::Track { id } = &listen.track else {
            // only songs show in the listening history
            return Ok(());
        };
        let gid = id.to_base16()?;
        let device_id = session.device_id().to_string();
        let session_id = random_hex();
        let playback_id = random_hex();
        let now = now_ms().to_string();
        let played = played_ms.to_string();
        let duration = listen.duration_ms.to_string();
        let context_url = format!("context://{context_uri}");
        self.sequence += 1;

        let new_session = [
            "557",
            "3",
            &session_id,
            context_uri,
            context_uri,
            &now,
            "",
            "0",
            &context_url,
        ];
        let new_playback = ["558", "1", &playback_id, &session_id, &now];
        let sequence = self.sequence.to_string();
        let started_at = listen.started_at_ms.to_string();
        // field for field as in librespot-java's TrackTransitionEvent
        #[rustfmt::skip]
        let track_transition = [
            "12", "38", &sequence, &device_id, &playback_id, "00000000000000000000000000000000",
            "playlist", "clickrow", "playlist", reason_end,
            "4800000", "4800000", &played, &played, &duration,
            "0", "0", "0", "0", "0", "0",
            "0", "-1", "context", "10", "0",
            "0", "0", "0", "0",
            &played, &played, "0", "160000",
            context_uri, "vorbis", &gid, "",
            "0", &started_at, "0",
            "context", "", "",
            "com.spotify", reason_end, "none",
            &device_id, "na", "none",
        ];
        for fields in [&new_session[..], &new_playback[..], &track_transition[..]] {
            send_event(session, &fields.join("\t"))?;
        }
        tracing::info!(
            "Reported a play of {} ({played_ms}ms, {reason_end}) from {context_uri}",
            listen.track.to_uri().unwrap_or_default()
        );
        Ok(())
    }
}

/// Send one event. `librespot` only sends Mercury requests with `GET`/`SEND`, and the event
/// service only takes `POST`, so the request is built here; its reply is not awaited.
fn send_event(session: &Session, event: &str) -> anyhow::Result<()> {
    let mut header = Header::new();
    header.set_uri(EVENTS_URI.to_string());
    header.set_method("POST".to_string());
    for (key, value) in [
        ("Accept-Language", "en".to_string()),
        ("X-ClientTimeStamp", now_ms().to_string()),
    ] {
        let mut field = UserField::new();
        field.set_key(key.to_string());
        field.set_value(value.into_bytes());
        header.user_fields.push(field);
    }
    let header = header.write_to_bytes()?;
    // a sequence number librespot's own Mercury requests never reach, so their replies don't mix
    let seq = (u64::from(u32::MAX) + u64::from(rand::rng().random::<u32>())).to_be_bytes();

    let mut packet = Vec::with_capacity(16 + header.len() + event.len());
    packet.extend_from_slice(&u16::try_from(seq.len())?.to_be_bytes());
    packet.extend_from_slice(&seq);
    packet.push(1); // flags: final
    packet.extend_from_slice(&2u16.to_be_bytes()); // parts: header and payload
    packet.extend_from_slice(&u16::try_from(header.len())?.to_be_bytes());
    packet.extend_from_slice(&header);
    packet.extend_from_slice(&u16::try_from(event.len())?.to_be_bytes());
    packet.extend_from_slice(event.as_bytes());
    session.send_packet(PacketType::MercuryReq, packet)?;
    Ok(())
}

fn now_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_millis())
}

fn random_hex() -> String {
    let mut rng = rand::rng();
    let bytes: [u8; 16] = rng.random();
    bytes
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect::<Vec<_>>()
        .concat()
}
