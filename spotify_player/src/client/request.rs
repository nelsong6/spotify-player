use crate::state::{
    AlbumId, Category, ContextId, Item, ItemId, PlayableId, Playback, PlaylistId, TrackId,
};

#[derive(Clone, Debug)]
/// A request that modifies the player's playback
pub enum PlayerRequest {
    NextTrack,
    PreviousTrack,
    Resume,
    Pause,
    ResumePause,
    SeekTrack(chrono::Duration),
    Repeat,
    Shuffle,
    Volume(u8),
    ToggleMute,
    StartPlayback(Playback, Option<bool>),
}

#[derive(Clone, Debug)]
/// A request to the client
pub enum ClientRequest {
    GetCurrentUser,
    GetBrowseCategories,
    GetBrowseCategoryPlaylists(Category),
    /// Forget the kept library and load it from Spotify again
    RefreshLibrary,
    GetUserPlaylists,
    GetUserSavedAlbums,
    GetUserSavedShows,
    GetUserFollowedArtists,
    GetContext(ContextId),
    /// Start a radio seeded by a random track from a playlist
    StartRandomTrackRadio(PlaylistId<'static>),
    GetCurrentPlayback,
    /// On a PC, the session's playback, as the session pushed it
    #[cfg(feature = "cast")]
    FollowSessionPlayback(Box<crate::screen::SessionPlayback>),
    /// On a PC, mute or unmute another PC, through the session's relay
    #[cfg(feature = "cast")]
    MutePc {
        pc: String,
        muted: bool,
    },
    Search(String),
    AddPlayableToQueue(PlayableId<'static>),
    AddAlbumToQueue(AlbumId<'static>),
    AddPlayableToPlaylist(PlaylistId<'static>, PlayableId<'static>),
    DeleteTrackFromPlaylist(PlaylistId<'static>, TrackId<'static>),
    ReorderPlaylistItems {
        playlist_id: PlaylistId<'static>,
        insert_index: usize,
        range_start: usize,
        range_length: Option<usize>,
        snapshot_id: Option<String>,
    },
    AddToLibrary(Item),
    DeleteFromLibrary(ItemId),
    Player(PlayerRequest),
    GetCurrentUserQueue,
    GetLyrics {
        track_id: TrackId<'static>,
    },
    CreatePlaylist {
        playlist_name: String,
        public: bool,
        collab: bool,
        desc: String,
    },
}
