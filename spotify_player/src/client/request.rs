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

impl ClientRequest {
    /// What the request does, as the screen names it, for one a person asked for; `None` for
    /// the app's own background reads
    pub fn action(&self) -> Option<&'static str> {
        Some(match self {
            Self::GetCurrentUser | Self::GetCurrentPlayback | Self::GetCurrentUserQueue => {
                return None
            }
            #[cfg(feature = "cast")]
            Self::FollowSessionPlayback(_) => return None,
            #[cfg(feature = "cast")]
            Self::MutePc { .. } => "mute a PC",
            Self::GetBrowseCategories | Self::GetBrowseCategoryPlaylists(_) => "load browse",
            Self::RefreshLibrary => "refresh the library",
            Self::GetUserPlaylists
            | Self::GetUserSavedAlbums
            | Self::GetUserSavedShows
            | Self::GetUserFollowedArtists => "load the library",
            Self::GetContext(_) => "load the page",
            Self::StartRandomTrackRadio(_) => "start a radio",
            Self::Search(_) => "search",
            Self::AddPlayableToQueue(_) | Self::AddAlbumToQueue(_) => "add to the queue",
            Self::AddPlayableToPlaylist(..) => "add to the playlist",
            Self::DeleteTrackFromPlaylist(..) => "remove from the playlist",
            Self::ReorderPlaylistItems { .. } => "reorder the playlist",
            Self::AddToLibrary(_) => "save to the library",
            Self::DeleteFromLibrary(_) => "remove from the library",
            Self::GetLyrics { .. } => "load the lyrics",
            Self::CreatePlaylist { .. } => "create the playlist",
            Self::Player(request) => match request {
                PlayerRequest::NextTrack => "skip to the next song",
                PlayerRequest::PreviousTrack => "go back a song",
                PlayerRequest::Resume
                | PlayerRequest::ResumePause
                | PlayerRequest::StartPlayback(..) => "play",
                PlayerRequest::Pause => "pause",
                PlayerRequest::SeekTrack(_) => "seek",
                PlayerRequest::Repeat => "change repeat",
                PlayerRequest::Shuffle => "change shuffle",
                PlayerRequest::Volume(_) => "change the volume",
                PlayerRequest::ToggleMute => "mute",
            },
        })
    }
}
