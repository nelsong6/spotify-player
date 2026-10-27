use crate::{client::AppClient, config, state::SharedState};
use anyhow::Context;
use librespot_connect::{ConnectConfig, Spirc};
use librespot_core::authentication::Credentials;
use librespot_core::config::DeviceType;
use librespot_core::{spotify_uri, Session, SpotifyUri};
use librespot_playback::mixer::MixerConfig;
use librespot_playback::{audio_backend, config::AudioFormat};
use librespot_playback::{
    config::{Bitrate, PlayerConfig},
    mixer::{self, Mixer},
    player,
};
use rspotify::model::{EpisodeId, Id, PlayableId, TrackId};
use serde::Serialize;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

/// Whether the next streaming connection is the first one of the process.
///
/// Used to scope `pause_on_startup` to application startup only, so that
/// reconnecting mid-session (e.g. via `RestartIntegratedClient`) does not
/// pause an intentionally playing track.
static IS_FIRST_CONNECTION: AtomicBool = AtomicBool::new(true);

#[derive(Debug, Serialize)]
enum PlayerEvent {
    Changed {
        playable_id: PlayableId<'static>,
    },
    Playing {
        playable_id: PlayableId<'static>,
        position_ms: u32,
    },
    Paused {
        playable_id: PlayableId<'static>,
        position_ms: u32,
    },
    EndOfTrack {
        playable_id: PlayableId<'static>,
    },
}

impl PlayerEvent {
    /// gets the event's arguments
    pub fn args(&self) -> Vec<String> {
        match self {
            PlayerEvent::Changed { playable_id } => {
                vec!["Changed".to_string(), playable_id.uri()]
            }
            PlayerEvent::Playing {
                playable_id,
                position_ms,
            } => vec![
                "Playing".to_string(),
                playable_id.uri(),
                position_ms.to_string(),
            ],
            PlayerEvent::Paused {
                playable_id,
                position_ms,
            } => vec![
                "Paused".to_string(),
                playable_id.uri(),
                position_ms.to_string(),
            ],
            PlayerEvent::EndOfTrack { playable_id } => {
                vec!["EndOfTrack".to_string(), playable_id.uri()]
            }
        }
    }
}

fn spotify_id_to_playable_id(uri: &spotify_uri::SpotifyUri) -> anyhow::Result<PlayableId<'static>> {
    match uri {
        SpotifyUri::Track { .. } => {
            let uri = uri.to_uri()?;
            Ok(TrackId::from_uri(&uri)?.into_static().into())
        }
        SpotifyUri::Episode { .. } => {
            let uri = uri.to_uri()?;
            Ok(EpisodeId::from_uri(&uri)?.into_static().into())
        }
        _ => anyhow::bail!("unexpected spotify_id {uri:?}"),
    }
}

impl PlayerEvent {
    pub fn from_librespot_player_event(e: player::PlayerEvent) -> anyhow::Result<Option<Self>> {
        Ok(match e {
            player::PlayerEvent::TrackChanged { audio_item } => Some(PlayerEvent::Changed {
                playable_id: spotify_id_to_playable_id(&audio_item.track_id)?,
            }),
            player::PlayerEvent::Playing {
                track_id,
                position_ms,
                ..
            } => Some(PlayerEvent::Playing {
                playable_id: spotify_id_to_playable_id(&track_id)?,
                position_ms,
            }),
            player::PlayerEvent::Paused {
                track_id,
                position_ms,
                ..
            } => Some(PlayerEvent::Paused {
                playable_id: spotify_id_to_playable_id(&track_id)?,
                position_ms,
            }),
            player::PlayerEvent::EndOfTrack { track_id, .. } => Some(PlayerEvent::EndOfTrack {
                playable_id: spotify_id_to_playable_id(&track_id)?,
            }),
            _ => None,
        })
    }
}

fn execute_player_event_hook_command(
    cmd: &config::Command,
    event: &PlayerEvent,
) -> anyhow::Result<()> {
    cmd.execute(Some(event.args()))?;

    Ok(())
}

/// Create a new streaming connection
pub async fn new_connection(
    client: AppClient,
    state: SharedState,
    session: Session,
    creds: Credentials,
) -> anyhow::Result<Spirc> {
    let configs = config::get_config();
    let device = &configs.app_config.device;

    // `librespot` volume is a u16 number ranging from 0 to 65535,
    // while a percentage volume value (from 0 to 100) is used for the device configuration.
    // So we need to convert from one format to another
    let volume = (f64::from(std::cmp::min(device.volume, 100_u8)) / 100.0 * 65535.0).round() as u16;

    let connect_config = ConnectConfig {
        name: device.name.clone(),
        device_type: device.device_type.parse::<DeviceType>().unwrap_or_default(),
        initial_volume: volume,

        // non-configurable fields, use default values.
        // We may allow users to configure these fields in a future release
        is_group: false,
        disable_volume: false,
        volume_steps: 64,
    };

    tracing::info!("Application's connect configurations: {:?}", connect_config);

    let mixer = Arc::new(
        mixer::softmixer::SoftMixer::open(MixerConfig::default()).context("opening softmixer")?,
    );
    mixer.set_volume(volume);

    // the Snapcast server runs this process and reads its audio from stdout
    let backend =
        audio_backend::find(Some("pipe".to_string())).context("find librespot's pipe output")?;
    let player_config = PlayerConfig {
        bitrate: device
            .bitrate
            .to_string()
            .parse::<Bitrate>()
            .unwrap_or_default(),
        normalisation: device.normalization,
        ..Default::default()
    };

    tracing::info!(
        "Initializing a new integrated player with device_id={}",
        session.device_id()
    );
    {
        let mut recorded = state.session.lock();
        recorded.own_device = Some(session.device_id().to_string());
        recorded.own_volume = Some(volume_percent(mixer.volume()));
    }
    if let Err(err) = crate::handover::connected(session.device_id()) {
        tracing::error!("Failed to ask the session to hand over: {err:#}");
    }

    let player = player::Player::new(
        player_config.clone(),
        session.clone(),
        mixer.get_soft_volume(),
        move || backend(None, AudioFormat::S16),
    );
    // not a Spotify Connect device, so playing along never takes over from the device followed
    let along = player::Player::new(
        player_config,
        session.clone(),
        mixer.get_soft_volume(),
        move || backend(None, AudioFormat::S16),
    );

    // When `pause_on_startup` is enabled, suppress Spotify's auto-resume of the
    // previous session by pausing the first auto-started playback. Scoped to the
    // first connection of the process so mid-session reconnects are unaffected.
    let pause_on_startup =
        configs.app_config.pause_on_startup && IS_FIRST_CONNECTION.swap(false, Ordering::SeqCst);

    crate::follow::watch(&session, state.clone());
    crate::follow::play_along(along, state.clone());

    let player_event_task = tokio::task::spawn({
        let mut channel = player.get_player_event_channel();
        let session = session.clone();
        async move {
            let mut pause_armed = pause_on_startup;
            let mut play_reporter = crate::play_report::PlayReporter::default();
            while let Some(event) = channel.recv().await {
                // Suppress Spotify's auto-resume of the previous session on
                // startup. The `librespot` connect transfer finalizes the
                // play state asynchronously, so a single reactive pause is not
                // reliable on its own:
                if pause_armed {
                    match &event {
                        // Best-effort: pause as the track starts loading, before
                        // the audio sink starts, so no audible blip occurs. This
                        // is a no-op if the transfer has not set the play state
                        // yet, so we do NOT disarm here.
                        player::PlayerEvent::Loading { .. } => {
                            client.pause_player();
                        }
                        // Authoritative: playback actually started (the transfer
                        // finalized into "playing"). Pause and stop interfering.
                        player::PlayerEvent::Playing { .. } => {
                            if client.pause_player() {
                                pause_armed = false;
                            }
                        }
                        // The track finished loading already paused, i.e. the
                        // `Loading` pause above took effect and no audio played.
                        player::PlayerEvent::Paused { .. } => {
                            pause_armed = false;
                        }
                        _ => {}
                    }
                }

                let context_uri = state
                    .player
                    .read()
                    .playback
                    .as_ref()
                    .and_then(|p| p.context.as_ref())
                    .map(|c| c.uri.clone())
                    .unwrap_or_default();
                play_reporter.handle(&event, &session, &context_uri);
                if let player::PlayerEvent::Playing { .. } = event {
                    crate::follow::radio_started();
                }
                if let player::PlayerEvent::VolumeChanged { volume } = event {
                    state.session.lock().own_volume = Some(volume_percent(volume));
                }
                // the session asks Spotify for its playback only when its player reports a
                // change; these have no `spotify_player` event of their own
                if matches!(
                    event,
                    player::PlayerEvent::Seeked { .. }
                        | player::PlayerEvent::PositionCorrection { .. }
                        | player::PlayerEvent::VolumeChanged { .. }
                        | player::PlayerEvent::ShuffleChanged { .. }
                        | player::PlayerEvent::RepeatChanged { .. }
                        | player::PlayerEvent::Stopped { .. }
                        | player::PlayerEvent::SessionDisconnected { .. }
                ) {
                    client.update_playback_non_blocking(&state);
                }
                // the player stopped (another device took over, or it was told to): it plays
                // nothing, whatever Spotify last said, so a refused refresh can't leave it
                // "playing" and hold up a handover
                if matches!(
                    event,
                    player::PlayerEvent::Stopped { .. }
                        | player::PlayerEvent::SessionDisconnected { .. }
                ) {
                    if let Some(playback) = state.player.write().buffered_playback.as_mut() {
                        playback.is_playing = false;
                    }
                    state.playback_changed.notify_one();
                }
                match PlayerEvent::from_librespot_player_event(event) {
                    Err(err) => {
                        tracing::warn!("Failed to convert a `librespot` player event into `spotify_player` player event: {err:#}");
                    }
                    Ok(Some(event)) => {
                        tracing::info!("Got a new player event: {event:?}");
                        match event {
                            PlayerEvent::Playing { .. } => {
                                if crate::listeners::is_unheard() {
                                    tracing::info!("No PC plays the session aloud; pausing");
                                    client.pause_player();
                                }
                                let mut player = state.player.write();
                                if let Some(playback) = player.buffered_playback.as_mut() {
                                    playback.is_playing = true;
                                }
                            }
                            PlayerEvent::Paused { .. } => {
                                let mut player = state.player.write();
                                if let Some(playback) = player.buffered_playback.as_mut() {
                                    playback.is_playing = false;
                                }
                            }
                            PlayerEvent::EndOfTrack { ref playable_id } => {
                                let client = client.clone();
                                let state = state.clone();
                                let ended_uri = playable_id.uri();
                                tokio::task::spawn(async move {
                                    // between songs: where a new release takes over
                                    if let Some(device_id) = crate::handover::pending(&state) {
                                        crate::handover::hand_over(
                                            &client, &state, &device_id, true,
                                        )
                                        .await;
                                        return;
                                    }
                                    if let Err(err) = crate::client::until_accepted(
                                        "continue after the playback ran out",
                                        || client.continue_after_end(&state, ended_uri.clone()),
                                    )
                                    .await
                                    {
                                        tracing::warn!(
                                            "Failed to continue after the playback ran out: {err:#}"
                                        );
                                    }
                                });
                            }
                            PlayerEvent::Changed { ref playable_id } => {
                                let expected = state.session.lock().expected.take();
                                if expected.is_some_and(|uri| uri != playable_id.uri()) {
                                    tracing::warn!(
                                        "The saved song isn't where it was; starting {} from its beginning",
                                        playable_id.uri()
                                    );
                                    client.seek_player(0);
                                }
                            }
                        }
                        state.playback_changed.notify_one();
                        client.update_playback_non_blocking(&state);

                        // execute a player event hook command
                        if let Some(ref cmd) = configs.app_config.player_event_hook_command {
                            if let Err(err) = execute_player_event_hook_command(cmd, &event) {
                                tracing::warn!(
                                    "Failed to execute player event hook command: {err:#}"
                                );
                            }
                        }
                    }
                    Ok(None) => {}
                }
            }
        }
    });

    tracing::info!("Starting an integrated Spotify player using librespot's spirc protocol");

    let (spirc, spirc_task) = Spirc::new(connect_config, session, creds, player, mixer)
        .await
        .context("initialize spirc")?;

    tokio::task::spawn(async move {
        tokio::select! {
            () = spirc_task => {},
            _ = player_event_task => {}
        }
    });

    tracing::info!("New streaming connection has been established!");

    Ok(spirc)
}

/// Keep the radio's place in the saved session while the session's player plays it. A pause or
/// resume is saved at once, so play picks up where it paused.
pub async fn keep_place(state: SharedState) {
    let mut ticks = tokio::time::interval(std::time::Duration::from_secs(1));
    let mut was_playing = None;
    loop {
        ticks.tick().await;
        let mut session = state.session.lock();
        let player = state.player.read();
        let playing = player.playback.as_ref().map(|playback| playback.is_playing);
        let toggled = was_playing.is_some() && playing != was_playing;
        was_playing = playing;
        session.record_progress(&player, toggled);
    }
}

fn volume_percent(volume: u16) -> u32 {
    u32::from(volume) * 100 / 65535
}
