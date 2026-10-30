# spotify_player

## Table of Contents

- [Introduction](#introduction)
- [Examples](#examples)
- [Installation](#installation)
- [Authentication](#authentication)
  - [How authentication works](#how-authentication-works)
  - [Why you may be asked to authenticate multiple times](#why-you-may-be-asked-to-authenticate-multiple-times)
  - [Client ID and rate limits](#client-id-and-rate-limits)
  - [Using a custom client ID](#using-a-custom-client-id)
- [Features](#features)
  - [Command palette](#command-palette)
  - [Kept playlists](#kept-playlists)
  - [Home cast](#home-cast)
  - [Audio Visualization](#audio-visualization)
  - [Media Control](#media-control)
  - [Image](#image)
  - [Notify](#notify)
  - [Mouse support](#mouse-support)
  - [Fuzzy search](#fuzzy-search)
  - [CLI commands](#cli-commands)
- [Commands](#commands)
- [Configurations](#configurations)
- [Caches](#caches)
  - [Logging](#logging)
- [Acknowledgement](#acknowledgement)

## Introduction

`spotify_player` is a fast, easy to use, and configurable terminal music player.

**Features**

- Minimalist UI with an intuitive paging and popup system.
- Highly [configurable](https://github.com/aome510/spotify-player/blob/master/docs/config.md)
- Feature parity with the official Spotify application.
- A [home cast](#home-cast): one session plays on every PC that has the app open, in sync.
- Support [audio visualization](#audio-visualization).
- Support synced lyrics.
- Support [cross-platform media control](#media-control).
- Support [image rendering](#image).
- Support [desktop notification](#notify).
- Offer a wide range of [CLI commands](#cli-commands)

## Examples

A demo of `spotify_player` `v0.5.0-pre-release` on [youtube](https://www.youtube.com/watch/Jbfe9GLNWbA) or on [asciicast](https://asciinema.org/a/446913):

Checkout [examples/README.md](https://github.com/aome510/spotify-player/blob/master/examples/README.md) for more examples.

## Installation

By default, the application's installed binary is `spotify_player`.

### Requirements

A Spotify Premium account is **required**.

#### Dependencies

##### Windows and MacOS

- [Rust and cargo](https://www.rust-lang.org/tools/install) as the build dependencies

##### Linux

- [Rust and cargo](https://www.rust-lang.org/tools/install) as the build dependencies
- install `openssl`, `alsa-lib` (`streaming` feature), `libdbus` (`media-control` feature).
  - For example, on Debian based systems, run the below command to install application's dependencies:

    ```shell
    sudo apt install libssl-dev libasound2-dev libdbus-1-dev
    ```

  - On RHEL/Fedora based systems, run the below command to install application's dependencies :

    ```shell
    sudo dnf install openssl-devel alsa-lib-devel dbus-devel
    ```

    or if you're using `yum`:

    ```shell
    sudo yum install openssl-devel alsa-lib-devel dbus-devel
    ```

##### Termux (Android)

- The default TLS backend (`rustls-platform-verifier`) requires a JVM context that
  Termux processes don't have and panics during the auth token exchange. Build with
  the `native-tls` feature instead, and without the cast:

  ```shell
  pkg install rust openssl
  cargo install spotify_player --no-default-features --features tui,media-control,native-tls
  ```

  This makes authentication, the Web API, and remote device control work.

### Binaries

Application's prebuilt binaries can be found in the [Releases Page](https://github.com/aome510/spotify-player/releases).

**Note**: to run the application, Linux systems need to install additional dependencies as specified in the [Dependencies section](#linux).

### Homebrew

Run `brew install spotify_player` to install the application.

### Scoop

Run `scoop install spotify-player` to install the application.

### Cargo

Install via Cargo:

```
cargo install spotify_player --locked
```

### Arch Linux

Install via Arch Linux:

```
pacman -S spotify-player
```

**Note**: The Arch package is built from upstream `spotify-player`, not this fork.

### Void Linux

Install via Void Linux:

```
xbps-install -S spotify-player
```

### FreeBSD

Install via FreeBSD:

```
pkg install spotify-player
```

### NetBSD

Install via NetBSD:

```
pkgin install spotify-player
```

Build from source on NetBSD:

```
cd /usr/pkgsrc/audio/spotify-player
make install
```

### NixOS

[spotify-player](https://search.nixos.org/packages?channel=unstable&show=spotify-player&from=0&size=50&sort=relevance&type=packages&query=spotify-player) is available as a Nix package. Install via:

```
nix-shell -p spotify-player
```

To build from source locally, run `nix-shell` in the root of the source checkout. The provided `shell.nix` will install prerequisites.

### Docker

**Note**: The streaming feature is disabled in the Docker image.

Download the latest Docker image:

```
docker pull aome510/spotify_player:latest
```

Run the Docker container:

```
docker run --rm -it aome510/spotify_player:latest
```

To use your local config and cache folders:

```
docker run --rm \
-v $APP_CONFIG_FOLDER:/app/config/ \
-v $APP_CACHE_FOLDER:/app/cache/ \
-it aome510/spotify_player:latest
```

## Authentication

`spotify_player` requires a **Spotify Premium** account and authenticates against the Spotify Web API using the [OAuth 2.0 authorization code flow with PKCE](https://developer.spotify.com/documentation/web-api/tutorials/code-pkce-flow). No client secret is stored or required.

The simplest way to authenticate is to just **run the application** — on first use it prompts for whichever credentials are not yet cached. Each prompt opens the Spotify authorization page in your browser; after you approve access, Spotify redirects to a local loopback address (`login_redirect_uri`, default `http://127.0.0.1:8989/login`) where `spotify_player` captures the authorization code and exchanges it for an access token. Credentials are cached in the application's [cache folder](#caches), so this is a one-time step per machine.

Alternatively, run the `spotify_player authenticate` CLI command to authenticate all required credentials up front — useful for setting things up ahead of a headless launch such as the session. Unlike a normal launch, `authenticate` always forces fresh interactive logins, ignoring cached credentials, so it can also be used to re-authenticate from scratch.

### How authentication works

Two kinds of credentials are involved:

- A **Web API token**, used for REST calls (playback control, library, search, playlists, etc.), under the configured `client_id` (ncspot's by default).
- A **librespot session**, used for the [streaming](#streaming) feature (direct playback and Spotify Connect device registration).

They authenticate through your Spotify account; the _client ID_ presented to Spotify differs between credentials (see below).

### Why you may be asked to authenticate multiple times

With the [streaming](#streaming) feature enabled (the default), the first launch normally opens the Spotify authorization page **twice**, in this order:

1. The **Web API token**, presented under the configured `client_id` (ncspot's by default). This is cached as `<client_id>_token.json`.
2. The **librespot session** credentials, presented under Spotify's official client ID. These are cached as `credentials.json`.

These are two independent OAuth flows with two different client IDs, so Spotify requires a separate approval for each, and each credential is cached separately in the [cache folder](#caches). This is a one-time step per machine — subsequent launches reuse and silently refresh them unless a cache is cleared or a credential is revoked.

Web API token cache filenames include their client IDs, so changing `client_id` requires a new approval instead of reusing a token issued to another client.

The `spotify_player authenticate` command runs every required flow in one go, forcing a fresh login for each. Without streaming, the librespot flow is omitted, leaving one Web API approval.

### Client ID and rate limits

Every request to the Spotify Web API is attributed to a Spotify _application_, identified by a **client ID**. The client ID — not your account — determines the [API quota](https://developer.spotify.com/documentation/web-api/concepts/rate-limits) you are subject to.

By default, `spotify_player` uses [ncspot](https://github.com/hrkfdn/ncspot)'s client ID. This client ID is shared by many users, so its API quota can be exhausted by aggregate usage and cause `429 Too Many Requests` responses. **Registering and configuring your own client ID is strongly recommended** so routine requests use a quota dedicated to your Spotify application.

Requests go only through the configured client; there is no fallback to another. While Spotify has asked it to wait (`Retry-After`), it sends nothing and answers for Spotify with that refusal. Every refusal is logged with the body Spotify sent, whose `reason` tells a development-mode quota running out (`QUOTA_EXCEEDED`) from the 30-second rate limit.

### Using a custom client ID

Use a custom client ID to avoid competing for the shared ncspot client's rate limit; every request is then attributed to your own Spotify application. Newly registered applications use the restricted development mode, so endpoints Spotify reserves for extended quota mode are unavailable to it.

To configure one, [register an application](https://developer.spotify.com/dashboard) on the Spotify developer dashboard, add your `login_redirect_uri` (default `http://127.0.0.1:8989/login`) to the app's allowed redirect URIs, then set `client_id` (or `client_id_command`) in `app.toml`. See the [Client id command](https://github.com/aome510/spotify-player/blob/master/docs/config.md#client-id-command) section of the configuration docs for details.

After changing the client ID, re-run `spotify_player authenticate` to get a token for it.

## Features

### Command palette

Press `/` to open the command palette: type to search a menu of what you can do, and press Enter to run it. The menu is a tree in [fzt](https://github.com/romaine-life/fzt)'s format, read from `menu.yaml` in the config folder each time the palette opens; until that file exists, the palette uses [`examples/menu.yaml`](examples/menu.yaml), and "edit menu" under `:` writes it there and opens it. Each entry runs a command, an action on the playing track, a radio from a random song of a playlist, or takes a search term for Spotify or for the list on screen; see [config docs](docs/config.md#command-palette-menu).

The palette is fzt itself, loaded from `fzt.dll` (from [fzt-terminal](https://github.com/romaine-life/fzt-terminal)'s releases) beside the executable. Without it, the palette says it couldn't load the library.

### Kept playlists

The [home cast's](#home-cast) session keeps the playlists listed as `kept_playlists` in the config, so playing one from its top always picks up where you left off, on any device. Every 10 minutes for the playlist that's playing and every hour for the others, it removes the songs Spotify's listening history shows were played from the playlist, and, while it plays, the songs before the playing one. A playlist with `radio_from` is a rolling radio: it's also refilled with radios seeded by random songs of that playlist, so `buffer` songs (default 200) stay ahead. See [config docs](docs/config.md#kept-playlists).

### Home cast

One session, running on an always-on machine, plays the music; every PC with `spotify_player` open plays it aloud, in sync with the others, and controls it. Which PC you sit at doesn't matter: close the app in one room and open it in the next, and the same queue keeps playing.

- **The session** is `spotify_player session`, built with the `streaming` feature and without `tui` (the terminal UI), since it has no screen. It runs the [librespot](https://github.com/librespot-org/librespot) player, the one Spotify Connect device for the house, named by `name` under `[device]`. It reports what it plays to Spotify, as Spotify's own apps do, so its plays show in your listening history. It also keeps its own history of every song it plays, in a SQLite database (`history.db` in its `--slots` folder): when it started and ended, how much played and how it ended (`finished`, `skipped`, `stopped`, or `cut_off` by a restart), the song's details, the radio it came from, and which PCs played it aloud. It writes its audio to stdout for [Snapcast](https://github.com/badaix/snapcast)'s server, which runs it as a `process` source (`codec=pcm`, `sampleformat=44100:16:2`) and streams it to the PCs. [`nas/spt-session`](nas/spt-session) runs both in a container on a Synology NAS, deployed with `nas/deploy.sh`.
- **A PC** runs `spotify_player` built with the `cast` feature (on by default). It attaches to the Snapcast server named by `cast_server`, plays the session's audio on the system's default output (on Windows it follows a change of default output) and on any other outputs chosen on that PC, and controls the session like any Connect device. It never starts a player of its own and never picks another device to play on.
- **One Spotify sign-in**: only the session signs in to Spotify. A PC sends every Spotify request (the library, search, play, lyrics, covers) to the session's relay, on port 1707 of the `cast_server` host, which sends it on with the session's sign-in, merges identical requests and holds the whole house to Spotify's waits. The session doesn't poll what it plays: it asks Spotify when its player reports a change, and passes the playback to every PC with the [shared screen](#home-cast). The relay takes only requests carrying the key the session keeps beside `session_file` (`relay.key`), so a PC needs read access to that folder and nothing else; deleting the key revokes it, and the session makes a new one.
- **Play here**: the session is the one output device, and the music keeps playing there. Each PC chooses whether it plays it here: `PlayHere` (`H`) turns play here on or off. With it off, the PC still shows and controls the radio, and the other rooms play on; the playback window shows `play here: off`, and the visualization's transport line `play here off`. A PC with play here off still draws the [audio visualization](#audio-visualization).
- **Several outputs on one PC**: `PlayHereOutputs` (`D`) lists the PC's audio outputs; **enter** switches one other than Windows' default on or off, and the PC remembers the choice (`outputs.json` in its cache folder). Every output switched on plays the session too, in step with the default and the other PCs; one that isn't there yet (a TV that's off) starts playing when it appears. Play here still covers the whole PC: off silences every output on it.
- **Drive and follow**: spt drives the session: the radio, and where it is (the session's saved session). `FollowSpotify` (`F`) follows what Spotify plays elsewhere, on purpose: every PC shows what Spotify plays, on which device, and the radio stays where it was. Another device playing or merely active never starts this on its own, since only a deliberate act moves the radio; something else playing on the session's own speaker (a Jam) is followed, read from Spotify's own connect-state pushes. Play takes control back and resumes the radio; if Spotify kept playing inside the radio's playlist while the session followed, the radio picks up from where Spotify got to.
- **Play here on another PC**: `PlayHereOnPc` (`P`) lists the PCs playing the session and whether play here is on for each; **enter** turns it on or off for the selected one. Above the PCs, four entries set every PC at once: play here on every PC, on no PC (the session pauses), only on this PC, and on every PC but this one; each is also a command (`PlayHereEverywhere`, `PlayHereNowhere`, `PlayHereOnlyHere`, `PlayHereAllButHere`) and a palette entry. The request goes to the session's relay with the relay key, like every other request, and the session passes it to Snapcast.
- **Pausing**: when no PC is left playing the session aloud (every PC quit or muted), the session pauses, and it pauses again any playback started while nobody listens. Resume from any PC. How a PC leaves matters: quitting (`q`) turns play here off on the way out, so the session pauses at once if no PC is left; a PC whose connection drops instead (restarted, crashed, off the network) still counts as listening for 30 seconds, so a restart doesn't stop the music. Each PC remembers play here across restarts (`play_here_off.json` in its cache folder).
- The PCs share the saved session through `session_file` on shared storage, so the radio and continue mode set on one PC are what the session continues with.
- **One screen**: every PC shows the same screen: the page and what's selected on it, popups and the command palette, whether the [audio visualization](#audio-visualization) is shown, and the ASCII cover's mode. The session keeps it (on port 1706 of the `cast_server` host, saved beside `session_file`), so a PC opens on whatever was last on screen, or on the visualization the first time. One PC at a time is in control; the others show `locked: space to take control` at the top right and ignore every other key and the mouse. Space takes control there. A theme picked on one PC stays on that PC. A PC that can't reach the session is its own screen.
- **What's wrong shows on screen**: while anything is wrong, a red row at the top of every PC's screen says what, on every page. It covers the session (no Spotify player, the radio stopped on its own, its saved place unreadable, a kept playlist not refilling, the Spotify sign-in not renewing, a wait on Spotify's rate limit with the time left) and the PC itself (the session or its audio out of reach, a speaker that won't open, a command that failed or is still waiting for an answer after a second). Several take turns, numbered. Each clears itself once fixed, and a failed command after 15 seconds. `spotify_player get key app` lists them all.
- **Upgrades don't stop the music**: `spotify_player session --slots <folder> --release <version>` runs as one of two slots, which Snapcast's `meta` source joins as one stream. The slot that isn't the session waits; started on a newer release, it connects beside the session and takes the playback over at the next song change, or right away while paused. `nas/spt-session` runs both slots, and publishing a release restarts only the waiting one.

### Audio Visualization

Real-time audio visualization replaces the page and the playback window with ASCII frequency bars (64 log-scale bands from bass (left) to treble (right)), drawn from the [home cast](#home-cast)'s audio as it reaches this PC's speakers. A transport line beneath the bars shows the play state, the elapsed time on a progress line you can click to seek, the duration, then the volume, device and what plays next. It stays up while playback is paused, with the bars flat, until you toggle it off.

Set `enable_audio_visualization` to `true` in your config to enable this feature, or toggle it while the app runs with `V`. Each bar thins from `@` at its base toward `.`, its top follows its neighbours as `^ / \ _ - ~`, and a `'` marks its recent peak. The bars are colored by height in a gradient drawn from the album cover's colors (or a violet → rose → gold one when there's no cover), unless the active theme sets the `visualization` component style (`low`/`mid`/`high` colors); see [config docs](https://github.com/aome510/spotify-player/blob/master/docs/config.md).

![Audio Visualization](https://github.com/user-attachments/assets/8c21c1b0-5276-4a9e-b719-e0c2bd555537)

With the `ascii-cover` feature (on by default), an ASCII rendering of the album cover, colored from the cover, fills up to half of the visualization, left of the bars, with the track, artists and album beneath it; the track title and the progress line take the bars' colors. In `Pulse` mode it brightens and thickens with the bass; in `Still` mode it is a plain picture. Pick the starting mode with `ascii_cover` (`Pulse`, `Still` or `Off`) and cycle it while the app runs with `CycleAsciiCover` (`I`).

### Media Control

Media control is enabled by default. Set `enable_media_control` to `true` in your config to use it. See [config docs](https://github.com/aome510/spotify-player/blob/master/docs/config.md#media-control).

Media control uses [MPRIS DBus](https://wiki.archlinux.org/title/MPRIS) on Linux and OS window events on Windows and macOS.

### Image

To enable image rendering, build with the `image` feature (disabled by default):

```shell
cargo install spotify_player --features image
```

Image rendering is powered by [`ratatui-image`](https://github.com/benjajaja/ratatui-image), which auto-detects the terminal's graphics protocol (Kitty, iTerm2, Sixel) on startup. Terminals without any graphics protocol support fall back to [block characters](https://en.wikipedia.org/wiki/Block_Elements).

**Notes**:

- Protocol detection queries the terminal via stdio. In nested terminals (e.g. Neovim's floating terminal), the query does not reach the outer terminal emulator, so the protocol falls back to block characters.

Image rendering examples:

- iTerm2:

![iTerm2](https://user-images.githubusercontent.com/40011582/172966798-0aadc431-b0c3-4433-adf3-7526684fc2a0.png)

- Kitty:

![kitty](https://user-images.githubusercontent.com/40011582/172967028-8cfb2daa-1642-499a-a5bf-8ed77f2b3fac.png)

- Sixel (`foot` terminal):

![sixel](https://user-images.githubusercontent.com/40011582/219880331-58ac1c30-bbb0-4c99-a6cc-e5b7c9c81455.png)

- Others:

![others](https://user-images.githubusercontent.com/40011582/172967325-d2098037-e19e-440a-a38a-5b076253ecb1.png)

#### Pixelate

For a pixelated look, enable the `pixelate` feature (also enables `image`):

```shell
cargo install spotify_player --features pixelate
```

Adjust the pixelation with the `cover_img_pixels` config option.

| `cover_img_pixels` | `8`                                                                                                                 | `16`                                                                                                                  | `32`                                                                                                                  | `64`                                                                                                                  |
| ------------------ | ------------------------------------------------------------------------------------------------------------------- | --------------------------------------------------------------------------------------------------------------------- | --------------------------------------------------------------------------------------------------------------------- | --------------------------------------------------------------------------------------------------------------------- |
| example            | <img width="100" alt="8x8" src="https://github.com/user-attachments/assets/4137aaea-ce28-4019-8cd5-2d14327e72e4" /> | <img width="100" alt="16x16" src="https://github.com/user-attachments/assets/0ca94748-093a-468c-8fb3-1f5639666eb6" /> | <img width="100" alt="32x32" src="https://github.com/user-attachments/assets/f5d0f2da-0439-47e4-91c9-3a2aa73ac90c" /> | <img width="100" alt="64x64" src="https://github.com/user-attachments/assets/d06ef731-38fa-424d-9672-313f56c193d0" /> |

To temporarily disable pixelation, set `cover_img_pixels` to a high value (e.g., `512`).

### Notify

To enable desktop notifications, build with the `notify` feature (disabled by default):

```shell
cargo install spotify_player --features notify
```

**Note**: Notification support is limited on macOS and Windows compared to Linux.

### Mouse support

Mouse support: You can seek to a position in the playback by left-clicking the progress bar.

### Fuzzy search

To enable [fuzzy search](https://en.wikipedia.org/wiki/Approximate_string_matching), build with the `fzf` feature (disabled by default).

### CLI Commands

`spotify_player` provides several CLI commands for interacting with Spotify:

- `get`: Get Spotify data (playlist/album/artist data, user's data, etc)
- `playback`: Interact with the playback (start a playback, play-pause, next, etc)
- `search`: Search spotify
- `like`: Like currently playing track
- `authenticate`: Authenticate the application
- `playlist`: Playlist editing (new, delete, import, fork, etc)
- `session`: Run the [home cast's](#home-cast) session (`streaming` feature only)

For more details, run `spotify_player -h` or `spotify_player {command} -h`.

**Notes**

- On first use, run `spotify_player authenticate` to authenticate the app.
- CLI commands communicate with a client socket on port `client_port` (default: `8080`). If no instance is running, a new client is started, which may increase latency.

#### Scripting

The command-line interface is script-friendly. Use the `search` subcommand to retrieve Spotify data in JSON format, which can be processed with tools like [jq](https://jqlang.github.io/jq/).

Example: Start playback for the first track from a search query:

```sh
read -p "Search spotify: " query
spotify_player playback start track --id $(spotify_player search "$query" | jq '.tracks.[0].id' | xargs)
```

## Commands

Press `?` or `C-h` to open the shortcut help page (default for `OpenCommandHelp`). Press `esc`, `?` or `backspace` to close it.

**Tips**:

- Use the command palette's "filter" (the `Search` command) to search in the shortcut help page and other pages.
- `RefreshPlayback` manually updates playback status.
- `RefreshLibrary` loads the library from Spotify again. The session keeps playlists, liked songs, albums and artists on disk until then, changing only what spt itself changes.

List of supported commands:

| Command                         | Description                                                                                        | Default shortcuts  |
| ------------------------------- | -------------------------------------------------------------------------------------------------- | ------------------ |
| `NextTrack`                     | next track                                                                                         | `n`                |
| `PreviousTrack`                 | previous track                                                                                     | `p`                |
| `ResumePause`                   | resume/pause based on the current playback                                                         | `space`            |
| `PlayRandom`                    | play a random track in the current context                                                         | `.`                |
| `Repeat`                        | cycle the repeat mode                                                                              | `C-r`              |
| `Shuffle`                       | toggle the shuffle mode                                                                            | `C-s`              |
| `VolumeChange`                  | change playback volume by an offset (default shortcuts use 5%)                                     | `+`, `-`           |
| `Mute`                          | toggle playback volume between 0% and previous level                                               | `_`                |
| `SeekStart`                     | seek start of current track                                                                        | `^`                |
| `SeekForward`                   | seek forward by a duration in seconds (defaults to `seek_duration_secs`, supports vim-style count) | `>`                |
| `SeekBackward`                  | seek backward by a duration in seconds (defaults to `seek_duration_secs`, supports vim-style count)| `<`                |
| `Quit`                          | quit the application                                                                               | `C-c`, `q`         |
| `ClosePopup`                    | close a popup                                                                                      | `esc`              |
| `SelectNextOrScrollDown`        | select the next item in a list/table or scroll down (supports vim-style count: 5j)                 | `j`, `C-n`, `down` |
| `SelectPreviousOrScrollUp`      | select the previous item in a list/table or scroll up (supports vim-style count: 10k)              | `k`, `C-p`, `up`   |
| `PageSelectNextOrScrollDown`    | select the next page item in a list/table or scroll a page down (supports vim-style count: 3C-f)   | `page_down`, `C-f` |
| `PageSelectPreviousOrScrollUp`  | select the previous page item in a list/table or scroll a page up (supports vim-style count: 2C-b) | `page_up`, `C-b`   |
| `SelectFirstOrScrollToTop`      | select the first item in a list/table or scroll to the top                                         | `g g`, `home`      |
| `SelectLastOrScrollToBottom`    | select the last item in a list/table or scroll to the bottom                                       | `G`, `end`         |
| `ChooseSelected`                | choose the selected item                                                                           | `enter`            |
| `RefreshPlayback`               | manually refresh the current playback                                                              | `r`                |
| `RefreshLibrary`                | load the library from Spotify again: playlists, liked songs, albums, artists                       | `R`                |
| `ShowActionsOnSelectedItem`     | open a popup showing actions on a selected item                                                    | `g a`, `C-space`   |
| `ShowActionsOnCurrentTrack`     | open a popup showing actions on the current track                                                  | `a`                |
| `ShowActionsOnCurrentContext`   | open a popup showing actions on the current context                                                | `A`                |
| `AddSelectedItemToQueue`        | add the selected item to queue                                                                     | `Z`, `C-z`         |
| `FocusNextWindow`               | focus the next focusable window (if any)                                                           | `tab`              |
| `FocusPreviousWindow`           | focus the previous focusable window (if any)                                                       | `backtab`          |
| `SwitchTheme`                   | open a popup for switching theme                                                                   | `T`                |
| `OpenCommandPalette`            | open the command palette: search and run what's in the menu                                        | `/`                |
| `Search`                        | open a popup for searching in the current page                                                     |                    |
| `BrowseUserPlaylists`           | open a popup for browsing user's playlists                                                         | `u p`              |
| `BrowseUserFollowedArtists`     | open a popup for browsing user's followed artists                                                  | `u a`              |
| `BrowseUserSavedAlbums`         | open a popup for browsing user's saved albums                                                      | `u A`              |
| `CurrentlyPlayingContextPage`   | go to the currently playing context page                                                           | `g space`          |
| `TopTrackPage`                  | go to the user top track page                                                                      | `g t`              |
| `RecentlyPlayedTrackPage`       | go to the user recently played track page                                                          | `g r`              |
| `LikedTrackPage`                | go to the user liked track page                                                                    | `g y`              |
| `LyricsPage`                    | go to the lyrics page of the current track                                                         | `g L`, `l`         |
| `LibraryPage`                   | go to the user library page                                                                        | `g l`              |
| `SearchPage`                    | go to the search page                                                                              | `g s`              |
| `BrowsePage`                    | go to the browse page                                                                              | `g b`              |
| `Queue`                         | go to the queue page                                                                               | `z`                |
| `OpenCommandHelp`               | go to the command help page                                                                        | `?`, `C-h`         |
| `PreviousPage`                  | go to the previous page                                                                            | `backspace`, `C-q` |
| `OpenLogs`                      | go the the application logs page                                                                   | `g o`              |
| `ToggleMouseCapture`            | toggle mouse capture (off: the terminal can select text again)                                     | `M`                |
| `CycleAsciiCover`               | cycle the ASCII cover beside the audio visualization (pulse, still, off)                           | `I`                |
| `PlayHere`                      | turn play here on or off: play the [home cast](#home-cast) on this PC, or only show it (`cast` feature only) | `H`                |
| `PlayHereOnPc`                  | turn play here on or off for any PC playing the [home cast](#home-cast) (`cast` feature only) | `P`                |
| `PlayHereOutputs`               | choose which of this PC's audio outputs play the [home cast](#home-cast) besides Windows' default (`cast` feature only) | `D`                |
| `PlayHereEverywhere`            | turn play here on for every PC (`cast` feature only) |                    |
| `PlayHereNowhere`               | turn play here off for every PC; the session pauses (`cast` feature only) |                    |
| `PlayHereOnlyHere`              | turn play here on for this PC and off for every other (`cast` feature only) |                    |
| `PlayHereAllButHere`            | turn play here off for this PC and on for every other (`cast` feature only) |                    |
| `FollowSpotify`                 | follow what Spotify plays, wherever it's driven from, until play takes control back (`cast` only)  | `F`                |
| `OpenSpotifyLinkFromClipboard`  | open a Spotify link from clipboard                                                                 | `O`                |
| `SortTrackByTitle`              | sort the track table (if any) by track's title                                                     | `s t`              |
| `SortTrackByArtists`            | sort the track table (if any) by track's artists                                                   | `s a`              |
| `SortTrackByAlbum`              | sort the track table (if any) by track's album                                                     | `s A`              |
| `SortTrackByAddedDate`          | sort the track table (if any) by track's added date                                                | `s D`              |
| `SortTrackByDuration`           | sort the track table (if any) by track's duration                                                  | `s d`              |
| `SortLibraryAlphabetically`     | sort the library alphabetically                                                                    | `s l a`            |
| `SortLibraryByRecent`           | sort the library (playlists and albums) by recently added items                                    | `s l r`            |
| `ReverseOrder`                  | reverse the order of the track table (if any)                                                      | `s r`              |
| `MovePlaylistItemUp`            | move playlist item up one position                                                                 | `C-k`              |
| `MovePlaylistItemDown`          | move playlist item down one position                                                               | `C-j`              |
| `CreatePlaylist`                | create a new playlist                                                                              | `N`                |
| `JumpToCurrentTrackInContext`   | jump to the current track in the context                                                           | `g c`              |
| `JumpToHighlightTrackInContext` | jump to the currently highlighted search result in the context                                     | `C-g`              |

To add or modify shortcuts, see the [keymaps section](https://github.com/aome510/spotify-player/blob/master/docs/config.md#keymaps).

### Actions

Not all actions are available for every Spotify item. To see available actions, use `ShowActionsOnCurrentTrack` or `ShowActionsOnSelectedItem`, then press enter to trigger the action. Some actions may not appear in the popup but can be bound to shortcuts.

List of available actions:

- `GoToArtist`
- `GoToAlbum`
- `GoToRadio`
- `AddToLibrary`
- `AddToPlaylist`
- `AddToQueue`
- `AddToLiked`
- `DeleteFromLiked`
- `DeleteFromLibrary`
- `DeleteFromPlaylist`
- `ShowActionsOnAlbum`
- `ShowActionsOnArtist`
- `ShowActionsOnShow`
- `ToggleLiked`
- `CopyLink`
- `Follow`
- `Unfollow`

Actions can also be bound to shortcuts. To add new shortcuts, see the [actions section](https://github.com/aome510/spotify-player/blob/master/docs/config.md#actions).

### Search Page

When entering the search page, focus is on the search input. Enter text, use `backspace` to delete, and `enter` to search.

To move focus from the search input to other windows (track results, album results, etc.), use `FocusNextWindow` or `FocusPreviousWindow`.

## Configurations

By default, configuration files are located in `$HOME/.config/spotify-player`. Change this with `-c <FOLDER_PATH>` or `--config-folder <FOLDER_PATH>`.

If no configuration file is found, one will be created with default values.

See [configuration documentation](https://github.com/aome510/spotify-player/blob/master/docs/config.md) for details on available options.

## Caches

By default, cache files are stored in `$HOME/.cache/spotify-player` (logs, credentials, audio cache, etc.). Change this with `-C <FOLDER_PATH>` or `--cache-folder <FOLDER_PATH>`.

### Logging

Logs are stored in `$APP_CACHE_FOLDER/spotify-player-*.log`. For debugging or issues, check the backtrace file in `$APP_CACHE_FOLDER/spotify-player-*.backtrace`.

Set the `RUST_LOG` environment variable to control [logging level](https://docs.rs/log/0.4.14/log/enum.Level.html). Default is `spotify_player=INFO`.

## Acknowledgement

`spotify_player` is written in [Rust](https://www.rust-lang.org) and built on top of libraries like [ratatui](https://github.com/ratatui/ratatui), [rspotify](https://github.com/ramsayleung/rspotify), [librespot](https://github.com/librespot-org/librespot), and more. It is inspired by [spotify-tui](https://github.com/Rigellute/spotify-tui) and [ncspot](https://github.com/hrkfdn/ncspot).
