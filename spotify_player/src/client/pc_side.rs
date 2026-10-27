//! A PC's side of the client: no Spotify sign-in, every request through the session's relay,
//! and the session's playback followed rather than asked for.

use std::sync::Arc;

use anyhow::Result;
#[cfg(feature = "streaming")]
use parking_lot::Mutex;

use super::{spotify, AppClient, WebApiClient};
use crate::{auth::AuthConfig, config, state::SharedState};

impl AppClient {
    /// Construct a PC's client, which never signs in to Spotify: every request goes through the
    /// session's relay, and the session pushes its playback (`screen`).
    pub fn relayed() -> Result<Self> {
        let config = rspotify::Config {
            api_base_url: crate::relay::pc::api_base_url(),
            token_refreshing: false,
            ..Default::default()
        };
        let mut primary = rspotify::AuthCodePkceSpotify::with_config(
            rspotify::Credentials::default(),
            rspotify::OAuth::default(),
            config,
        )
        .with_middleware(crate::relay::pc::KeyMiddleware::new());
        // the relay goes by its key; rspotify only needs a token to send
        primary.token = Arc::new(rspotify::sync::Mutex::new(Some(rspotify::Token {
            access_token: "relay".to_string(),
            ..Default::default()
        })));
        Ok(Self {
            spotify: Arc::new(spotify::Spotify::new()),
            http: reqwest::Client::new(),
            auth_config: AuthConfig::new(config::get_config())?,
            api_client: WebApiClient::new(primary, None).with_ncspot_only_get_endpoints(Vec::new()),
            #[cfg(feature = "streaming")]
            stream_conn: Arc::new(Mutex::new(None)),
            relay: Some(crate::relay::pc::Relay::new()),
        })
    }

    /// Whether this is a PC's client, which follows the session's playback instead of asking
    /// Spotify for it
    pub(super) fn follows_session(&self) -> bool {
        self.relay.is_some()
    }

    /// Show the session's playback on a PC
    pub(super) async fn follow_session_playback(
        &self,
        state: &SharedState,
        playback: crate::screen::SessionPlayback,
    ) -> Result<()> {
        crate::cast::set_following(playback.following);
        state.player.write().queue = playback.queue;
        let new_playback = Self::apply_playback(state, playback.playback, true);
        if playback.left_off || playback.following {
            // nothing of ours is loaded on Spotify, so play resumes the saved session, taking
            // control back
            state.player.write().buffered_playback = None;
        }
        if new_playback {
            self.handle_new_playback_event(state).await?;
        }
        Ok(())
    }
}
