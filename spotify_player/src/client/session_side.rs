//! The session's side of the client: its player, and keeping the radio going.

use anyhow::Result;
use rspotify::prelude::*;

use super::AppClient;
use crate::{
    config,
    state::{session, Device, PlaylistId, SharedState, TrackId},
};

impl AppClient {
    /// Create a new streaming connection
    pub async fn new_streaming_connection(
        &self,
        state: SharedState,
        session: librespot_core::Session,
        creds: librespot_core::authentication::Credentials,
    ) -> Result<()> {
        let new_conn =
            crate::streaming::new_connection(self.clone(), state, session, creds).await?;
        let mut stream_conn = self.stream_conn.lock();
        // shutdown old streaming connection and replace it with a new connection
        if let Some(conn) = stream_conn.as_ref() {
            if let Err(err) = conn.shutdown() {
                log::error!("Failed to shutdown old streaming connection: {err:#}");
            }
        }
        *stream_conn = Some(new_conn);
        Ok(())
    }

    /// Move the session's player to `position_ms` in the song it has loaded
    pub fn seek_player(&self, position_ms: u32) {
        if let Some(spirc) = self.stream_conn.lock().as_ref() {
            if let Err(err) = spirc.set_position_ms(position_ms) {
                tracing::warn!("Failed to seek the player: {err:#}");
            }
        }
    }

    /// Pause the session's player, if a connection exists.
    ///
    /// Returns `true` if a streaming connection was present and the pause
    /// command was issued.
    pub fn pause_player(&self) -> bool {
        match self.stream_conn.lock().as_ref() {
            Some(spirc) => {
                if let Err(err) = spirc.pause() {
                    tracing::warn!("Failed to pause the player: {err:#}");
                }
                true
            }
            None => false,
        }
    }

    /// Ensures the integrated librespot device (of *this* running instance) is present in `devices`.
    ///
    /// The integrated device may not show up in the device list returned by the Spotify API because
    /// 1. The device is just initialized and hasn't been registered in Spotify server.
    ///    Related issue/discussion: <https://github.com/aome510/spotify-player/issues/79>
    /// 2. The device list is empty. This might be because user doesn't specify their own client ID.
    ///    By default, the application uses Spotify web app's client ID, which doesn't have
    ///    access to user's active devices.
    ///
    /// Callers must check [`crate::state::State::is_session`] first. The `streaming` feature being compiled in
    /// does not mean *this* instance runs the player: only the session does, so another instance
    /// would otherwise advertise a device that does not exist.
    pub(super) async fn ensure_integrated_device(&self, devices: &mut Vec<Device>) {
        let session = self.spotify.session().await;
        let session_device_id = session.device_id().to_string();

        // Mark the integrated device if it's already in the list; otherwise, add it, so it's
        // always present without duplicating an entry the API already returned.
        match devices.iter_mut().find(|d| d.id == session_device_id) {
            Some(device) => device.is_integrated = true,
            None => devices.insert(
                0,
                Device {
                    id: session_device_id,
                    name: config::get_config().app_config.device.name.clone(),
                    is_integrated: true,
                },
            ),
        }
    }

    /// The URIs of a playlist's items, in order
    pub async fn playlist_item_uris(&self, playlist_id: PlaylistId<'_>) -> Result<Vec<String>> {
        let items = self
            .all_paging_items::<rspotify::model::PlaylistItem>(
                &format!("playlists/{}/items", playlist_id.id()),
                0,
            )
            .await?;
        Ok(items
            .into_iter()
            .filter_map(|item| match item.item? {
                rspotify::model::PlayableItem::Track(track) => track.id.map(|id| id.uri()),
                rspotify::model::PlayableItem::Episode(episode) => Some(episode.id.uri()),
                rspotify::model::PlayableItem::Unknown(_) => None,
            })
            .collect())
    }

    /// Keep the music going after the playback runs out, as the session's continue mode says.
    /// Called when a track ends; it does nothing if the player went on to another track.
    pub async fn continue_after_end(&self, state: &SharedState, ended_uri: String) -> Result<()> {
        if state.session.lock().session.continue_mode == session::ContinueMode::Stop {
            return Ok(());
        }
        // the player starts the next track right away when there is one
        tokio::time::sleep(std::time::Duration::from_secs(3)).await;
        if self
            .current_playback2()
            .await?
            .is_some_and(|p| p.is_playing)
        {
            return Ok(());
        }

        let (mode, source_playlist) = {
            let session = state.session.lock();
            (
                session.session.continue_mode,
                session.session.source_playlist(),
            )
        };
        tracing::info!(
            "Playback ran out after {ended_uri}: continue mode {mode:?}, source playlist {source_playlist:?}"
        );
        match (mode, &source_playlist) {
            (session::ContinueMode::Stop, _) => Ok(()),
            (session::ContinueMode::PlaylistRadio, Some(uri)) => {
                let playlist_id = PlaylistId::from_uri(uri)?.into_static();
                self.start_random_track_radio(state, playlist_id).await
            }
            _ => {
                let seed = self.track(TrackId::from_uri(&ended_uri)?).await?;
                self.start_track_radio(state, seed, source_playlist).await
            }
        }
    }

    /// Load the playback saved by the previous run onto `device_id`, paused at the saved
    /// position. Its radio page is opened at startup by `show_saved_radio_page`.
    pub(super) async fn restore_session(&self, state: &SharedState, device_id: &str) -> Result<()> {
        // only on the first connection after startup, not when the session reconnects
        static RESTORED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
        if RESTORED.load(std::sync::atomic::Ordering::SeqCst) {
            return Ok(());
        }
        let Some(saved) = session::load() else {
            return Ok(());
        };
        tracing::info!(
            "Restoring the previous session: context={:?}, {} tracks, at {:?} {}ms",
            saved.context_uri,
            saved.uris.len(),
            saved.current_uri,
            saved.progress_ms
        );
        #[cfg(feature = "streaming")]
        {
            state.session.lock().expected.clone_from(&saved.current_uri);
        }
        self.start_saved_session(&saved, device_id).await?;
        self.pause_playback(Some(device_id)).await?;
        // done only once Spotify has taken it, so a refusal is sent again
        RESTORED.store(true, std::sync::atomic::Ordering::SeqCst);
        state.session.lock().sync();
        Ok(())
    }
}
