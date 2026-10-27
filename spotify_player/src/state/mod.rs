mod constant;
mod data;
mod model;
mod player;
mod queue;
pub mod requests;
pub mod session;
mod ui;

use std::{collections::VecDeque, sync::Arc};

pub use constant::*;
pub use data::*;
pub use model::*;
pub use player::*;
#[allow(unused_imports)]
pub use queue::*;
pub use ui::*;

use crate::config;

pub use parking_lot::{Mutex, RwLock};

/// Application's shared state
pub type SharedState = Arc<State>;

/// Application's state
pub struct State {
    pub ui: Mutex<UIState>,
    pub player: RwLock<PlayerState>,
    pub data: RwLock<AppData>,
    /// The playback session saved for the next run
    pub session: Mutex<session::SessionState>,

    /// Whether this instance is the home cast's session: headless, running the player
    pub is_session: bool,

    /// Shared FFT frequency-band data written by the cast's analyzer and read by the UI.
    #[cfg(feature = "cast")]
    pub vis_bands: Option<Arc<Mutex<crate::ui::streaming::VisBands>>>,

    /// In the session, raised when its playback or queue changes, so the PCs hear of it
    #[cfg(feature = "streaming")]
    pub playback_changed: tokio::sync::Notify,

    /// In the session, whether it follows Spotify rather than driving it
    #[cfg(feature = "streaming")]
    pub follow: Mutex<crate::follow::Follow>,

    pub logs: Arc<Mutex<VecDeque<String>>>,

    /// Text of the last frame drawn, for `get key app`
    pub screen: Mutex<Vec<String>>,
    /// Client requests being handled and recent failures, for `get key app`
    pub requests: Mutex<requests::RequestTracker>,
}

impl State {
    pub fn new(is_session: bool, log_buffer: Arc<Mutex<VecDeque<String>>>) -> Self {
        let mut ui = UIState::default();
        let configs = config::get_config();

        if let Some(theme) = configs.theme_config.find_theme(&configs.app_config.theme) {
            // update the UI's theme based on the `theme` config option
            ui.theme = theme;
        }

        let app_data = AppData::new(&configs.cache_folder);

        Self {
            ui: Mutex::new(ui),
            player: RwLock::new(PlayerState::default()),
            data: RwLock::new(app_data),
            session: Mutex::new(session::SessionState::default()),
            is_session,
            #[cfg(feature = "cast")]
            vis_bands: Some(Arc::new(Mutex::new(
                crate::ui::streaming::VisBands::default(),
            ))),
            #[cfg(feature = "streaming")]
            playback_changed: tokio::sync::Notify::new(),
            #[cfg(feature = "streaming")]
            follow: Mutex::new(crate::follow::Follow::default()),

            logs: log_buffer,
            screen: Mutex::new(Vec::new()),
            requests: Mutex::new(requests::RequestTracker::default()),
        }
    }

    /// Whether this is a PC of the home cast, which shows the session's playback and reaches
    /// Spotify only through the session
    pub fn follows_session(&self) -> bool {
        cfg!(feature = "cast") && !self.is_session
    }

    /// Returns `true` when the custom queue system should be used for new playback.
    ///
    /// Requires the session's player and the `custom_queue` config option to be
    /// `true`.
    #[cfg(feature = "streaming")]
    #[allow(dead_code)]
    pub fn should_use_custom_queue(&self) -> bool {
        self.is_session && config::get_config().app_config.custom_queue
    }

    /// Whether the audio visualization is switched on. It stays up while nothing plays, with its
    /// bars flat.
    #[cfg(feature = "cast")]
    pub fn is_visualization_shown(&self) -> bool {
        self.vis_bands
            .as_ref()
            .is_some_and(|bands| bands.lock().enabled)
    }
}
