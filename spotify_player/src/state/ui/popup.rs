use super::LineInput;
use crate::{
    command,
    state::{
        model::{Album, Artist, Episode, EpisodeId, Playlist, Show, Track, TrackId},
        ItemId,
    },
};
use ratatui::widgets::ListState;
use rspotify::model::PlaylistId;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PlaylistCreateCurrentField {
    Name,
    Desc,
}

#[derive(Debug, Serialize, Deserialize)]
pub enum PopupState {
    Search {
        query: String,
    },
    UserPlaylistList(PlaylistPopupAction, ListState),
    UserFollowedArtistList(ListState),
    UserSavedAlbumList(ListState),
    DeviceList(ListState),
    /// The PCs playing the home cast, to mute or unmute one
    PcList(ListState),
    ArtistList(ArtistPopupAction, Vec<Artist>, ListState),
    /// Not shared with the other PCs, since a theme is applied to one PC only
    #[serde(skip)]
    ThemeList(Vec<crate::config::Theme>, ListState),
    ActionList(Box<ActionListItem>, ListState),
    PlaylistCreate {
        name: LineInput,
        desc: LineInput,
        current_field: PlaylistCreateCurrentField,
    },
    ConfirmAction {
        message: String,
        action: ConfirmableAction,
    },
    #[cfg(feature = "tui")]
    CommandPalette(Box<CommandPalette>),
}

/// The command palette: an fzt session over the menu, or why it couldn't open
#[cfg(feature = "tui")]
#[derive(Debug, Deserialize)]
pub enum CommandPalette {
    #[serde(skip)]
    Open {
        session: crate::fzt::Session,
        frame: crate::fzt::Frame,
    },
    /// Another PC's palette, as it draws it
    Shown(crate::fzt::Frame),
    Failed(String),
}

/// The menu session stays on the PC that opened it; the other PCs get what it draws.
#[cfg(feature = "tui")]
impl Serialize for CommandPalette {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            Self::Open { frame, .. } | Self::Shown(frame) => {
                serializer.serialize_newtype_variant("CommandPalette", 1, "Shown", frame)
            }
            Self::Failed(err) => {
                serializer.serialize_newtype_variant("CommandPalette", 2, "Failed", err)
            }
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ConfirmableAction {
    DeleteTrackFromPlaylist {
        playlist_id: PlaylistId<'static>,
        track_id: TrackId<'static>,
    },
    DeleteFromLibrary(ItemId),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ActionListItem {
    Track(Track, Vec<command::Action>),
    Artist(Artist, Vec<command::Action>),
    Album(Album, Vec<command::Action>),
    Playlist(Playlist, Vec<command::Action>),
    Show(Show, Vec<command::Action>),
    Episode(Episode, Vec<command::Action>),
}

/// An action on an item in a playlist popup list
#[derive(Debug, Serialize, Deserialize)]
pub enum PlaylistPopupAction {
    Browse {
        folder_id: usize,
        search_query: String,
    },
    AddTrack {
        folder_id: usize,
        track_id: TrackId<'static>,
        search_query: String,
    },
    AddEpisode {
        folder_id: usize,
        episode_id: EpisodeId<'static>,
        search_query: String,
    },
}

/// An action on an item in an artist popup list
#[derive(Copy, Clone, Debug, Serialize, Deserialize)]
pub enum ArtistPopupAction {
    Browse,
    ShowActions,
}

impl PopupState {
    /// gets the (immutable) list state of a (list) popup
    pub fn list_state(&self) -> Option<&ListState> {
        match self {
            Self::DeviceList(list_state)
            | Self::PcList(list_state)
            | Self::UserPlaylistList(.., list_state)
            | Self::UserFollowedArtistList(list_state)
            | Self::UserSavedAlbumList(list_state)
            | Self::ArtistList(.., list_state)
            | Self::ThemeList(.., list_state)
            | Self::ActionList(.., list_state) => Some(list_state),
            // the search, playlist-create, confirm and palette popups
            _ => None,
        }
    }

    /// gets the (mutable) list state of a (list) popup
    pub fn list_state_mut(&mut self) -> Option<&mut ListState> {
        match self {
            Self::DeviceList(list_state)
            | Self::PcList(list_state)
            | Self::UserPlaylistList(.., list_state)
            | Self::UserFollowedArtistList(list_state)
            | Self::UserSavedAlbumList(list_state)
            | Self::ArtistList(.., list_state)
            | Self::ThemeList(.., list_state)
            | Self::ActionList(.., list_state) => Some(list_state),
            // the search, playlist-create, confirm and palette popups
            _ => None,
        }
    }

    /// gets the selected position of a (list) popup
    pub fn list_selected(&self) -> Option<usize> {
        match self.list_state() {
            None => None,
            Some(state) => state.selected(),
        }
    }

    /// selects a position in a (list) popup
    pub fn list_select(&mut self, id: Option<usize>) {
        match self.list_state_mut() {
            None => {}
            Some(state) => state.select(id),
        }
    }
}

impl ActionListItem {
    pub fn n_actions(&self) -> usize {
        match self {
            ActionListItem::Track(.., actions)
            | ActionListItem::Artist(.., actions)
            | ActionListItem::Album(.., actions)
            | ActionListItem::Playlist(.., actions)
            | ActionListItem::Show(.., actions)
            | ActionListItem::Episode(.., actions) => actions.len(),
        }
    }

    pub fn name(&self) -> &str {
        match self {
            ActionListItem::Track(track, ..) => &track.name,
            ActionListItem::Artist(artist, ..) => &artist.name,
            ActionListItem::Album(album, ..) => &album.name,
            ActionListItem::Playlist(playlist, ..) => &playlist.name,
            ActionListItem::Show(show, ..) => &show.name,
            ActionListItem::Episode(episode, ..) => &episode.name,
        }
    }

    pub fn actions_desc(&self) -> Vec<String> {
        match self {
            ActionListItem::Track(.., actions)
            | ActionListItem::Artist(.., actions)
            | ActionListItem::Album(.., actions)
            | ActionListItem::Playlist(.., actions)
            | ActionListItem::Show(.., actions)
            | ActionListItem::Episode(.., actions) => {
                actions.iter().map(|a| format!("{a:?}")).collect::<Vec<_>>()
            }
        }
    }
}
