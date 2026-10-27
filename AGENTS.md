# AGENTS.md

This file guides coding agents when working in this repository.

## Project Overview

`spotify-player` is a terminal Spotify client (requires Spotify Premium) written in Rust. It supports playback control, Spotify Connect, synced lyrics, desktop notifications, OS media controls, album art rendering, and a full CLI interface. In this fork every PC plays one shared session on the NAS (the home cast; see Fork notes).

## Architecture

### Key modules in `spotify_player/src/`

| Module                        | Responsibility                                                              |
| ----------------------------- | --------------------------------------------------------------------------- |
| `main.rs`                     | Entry point; wires threads/tasks; CLI arg parsing; logging init             |
| `state/`                      | Shared app state (`Arc<State>`): UI, player data, library caches, queue     |
| `state/model.rs`              | Core domain types: `Track`, `Album`, `Artist`, `Playlist`, `Playback`, etc. |
| `state/player.rs`             | `PlayerState`: current playback, devices, queue, progress estimation        |
| `state/data.rs`               | `AppData`: user library, TTL memory caches, file-cache persistence          |
| `state/ui/`                   | `UIState`: page history stack, popup state, key buffer, count prefix        |
| `client/mod.rs`               | `AppClient`: Spotify API calls, session management                          |
| `client/request.rs`           | `ClientRequest` / `PlayerRequest` enums (async message types)               |
| `client/handlers.rs`          | Tokio task: receives `ClientRequest`, dispatches API calls                  |
| `config/mod.rs`               | `Configs` loaded once into a `OnceLock`; read via `config::get_config()`    |
| `config/keymap.rs`            | Default keybindings and key sequence lookup                                 |
| `config/theme.rs`             | Theme definitions and style resolution                                      |
| `command.rs`                  | `Command` enum (all TUI commands), `Action` / `CommandOrAction`             |
| `key.rs`                      | `Key` / `KeySequence` types; vim-style key parsing (`C-x`, `M-x`)           |
| `event/mod.rs`                | crossterm input loop; routes events to page or popup handler                |
| `event/{page,popup}.rs`       | Key event dispatch per page / popup overlay                                 |
| `event/palette.rs`            | Command palette: opens fzt over `menu.yaml`, runs the picked entry          |
| `ui/mod.rs`                   | ratatui render loop; main layout dispatch                                   |
| `ui/{page,playback,popup}.rs` | Render functions for pages, playback bar, popups                            |
| `ui/palette.rs`               | Draws the command palette over the page from fzt's rows                     |
| `fzt.rs`                      | Loads fzt (`fzt.dll`, fzt-terminal's `cmd/fztlib`) and wraps its JSON calls |
| `keeper.rs`                   | The session's job of trimming played songs and refilling radio playlists    |
| `relay/`                      | The session's relay: a PC's only way to Spotify, behind a key on the NAS    |
| `follow.rs`                   | Drive and follow: the session follows Spotify when something else drives it |
| `play_report.rs`              | Reports the session's plays to Spotify (feature-gated)                      |
| `ui/streaming.rs`             | FFT audio visualizer: `Analyzer`, `VisBands`, visualization page            |
| `ui/ascii_cover.rs`           | ASCII album cover beside the bars, pulsing with the bass (feature-gated)    |
| `streaming.rs`                | The session's librespot player, its audio on stdout (feature-gated)         |
| `listeners.rs`                | Pauses the session when no PC plays it aloud (Snapcast's client list)       |
| `home_auth.rs`                | auth.romaine.life sign-in on a PC; the session's check before muting a PC   |
| `screen/`                     | One screen on every PC: the session keeps it, one PC at a time controls it  |
| `cast/`                       | A PC's side of the home cast: Snapcast client, clock sync, synced output    |
| `cli/`                        | Unix socket server and client for inter-process CLI commands                |
| `auth.rs`                     | OAuth scopes and librespot credential/session building                      |
| `media_control.rs`            | OS media key integration via `souvlaki` (feature-gated)                     |

### Concurrency model

Multiple OS threads communicate via `flume` channels and `Arc<State>`:

- **Event thread** — blocking `crossterm::event::read()` loop
- **UI thread** — poll-based ratatui render loop
- **Tokio runtime** — async tasks: client handler, socket listener, streaming
- **Player-event watcher** — polls librespot playback state, sends `ClientRequest`s
- **Media-control thread** — OS media key events (feature-gated)
- **Cast threads** (a PC) — the Snapcast connection, its time sync, the sound card's callback,
  and the visualization's analyzer
- **Screen threads** (a PC) — the shared screen's connection to the session, and its sender

Event/UI threads never call async functions directly. They send a `ClientRequest` over a `flume` channel; the async handler updates `Arc<State>`.

### Feature flags

| Feature                                                                                                                                 | Effect                                                   |
| --------------------------------------------------------------------------------------------------------------------------------------- | -------------------------------------------------------- |
| `streaming`                                                                                                                             | The session: librespot player, `spotify_player session`  |
| `cast`                                                                                                                                  | A PC: plays the session from Snapcast, visualization     |
| `tui`                                                                                                                                   | The terminal UI: pages, keys, palette (part of `cast`)   |
| `media-control`                                                                                                                         | OS media key integration                                 |
| `image`                                                                                                                                 | Album art rendering                                      |
| `ascii-cover`                                                                                                                           | ASCII album cover in the visualization (default)         |
| `sixel`                                                                                                                                 | Sixel terminal image protocol (extends `image`)          |
| `pixelate`                                                                                                                              | Pixelated image rendering fallback (extends `image`)     |
| `notify`                                                                                                                                | Desktop notifications                                    |
| `fzf`                                                                                                                                   | Fuzzy search                                             |

Default: `cast` + `media-control` + `ascii-cover` (the PCs' build); the NAS's Linux build is `rustls-tls` + `streaming`, without `tui`, so the session's program holds no screen code. Code that's the session's or a PC's alone lives in its own file (`relay/session.rs` and `relay/pc.rs`, `screen/session.rs` and `screen/pc.rs`, `client/session_side.rs` and `client/pc_side.rs`): `publish.ps1` moves the session only when a file compiled into it changed. Gate feature-specific code with `#[cfg(feature = "...")]`.

## Verifying changes

CI treats all warnings as errors. Before committing, run what CI runs:

```sh
cargo fmt --all                      # CI checks: cargo fmt --all -- --check
cargo test --no-default-features --features streaming,cast,media-control,image,notify,fzf
cargo clippy --no-default-features --features streaming,cast,media-control,image,notify,fzf -- -D warnings
cargo clippy --no-default-features -- -D warnings   # core paths, no features
```

When fixing no-feature clippy warnings, you may need `#[allow(dead_code)]` / `#[allow(unused_variables)]` on items only used in feature-gated paths. Lint the session's build on its own too: `cargo clippy --no-default-features --features rustls-tls,streaming -- -D warnings`.

## Conventions

### Error handling

- Return `anyhow::Result<T>` from fallible functions.
- Add context with `.context("...")` / `.with_context(|| ...)`; early-exit with `anyhow::bail!("...")`.
- Format error chains with `{err:#}` (alternate Display), never `{err}`.
- At async task boundaries, log and continue — never let one failure crash a long-running task:
  ```rust
  if let Err(err) = do_something().await {
      tracing::error!("Failed to ...: {err:#}");
  }
  ```

### Logging

Use `tracing` exclusively — never `println!`, `eprintln!`, or the `log` crate.

```rust
tracing::info!("...");
tracing::error!("...: {err:#}");
tracing::debug!("{value:?}");
```

### Comments and doc comments

- Reserve comments for non-obvious intent: invariants, ordering constraints, workarounds, edge cases.
- Do not narrate the implementation. A doc comment states a type/function's purpose and contract (what callers need to know); leave the mechanics — which branch does what, field-by-field behaviour, control flow — to the code itself. Implementation rationale belongs in a focused inline comment at the relevant line, not in the doc comment.
- Keep comments concise and clear, avoid long paragraphs and try to keep comments up to date with code changes.

## Keeping docs up to date

`README.md` and `docs/config.md` are the primary user-facing references. Update them on any user-visible change:

- **New feature / config option** — describe it in `README.md` (`Features`, `Configuration`, …), document the field in `docs/config.md`, and update `examples/app.toml` if applicable.
- **Changed / removed behaviour** — update affected tables, command descriptions, and usage examples in both files.
- **New feature flag** — add it to the feature-flags table in `README.md`.
- **New CLI subcommand** — document it under the CLI section of `README.md`.

Keep `.github/copilot-instructions.md` and this `AGENTS.md` in sync when project structure, architecture, or conventions change significantly.

### Adding a new `Command`

1. Add the variant to `Command` in `command.rs` and update `Command::desc()`.
2. Add a default keybinding in `config/keymap.rs`.
3. Update the command table in `README.md`.
4. Add it to the command palette's default menu, `examples/menu.yaml` (and the shared
   `\NelsonNAS\tools\spt\config\menu.yaml`, which overrides it on the home PCs).

## Writing PR descriptions

Base the description on the actual branch diff (`git diff master...<branch>`), not assumptions. Keep it clear and concise:

- **Summary** — 2-4 sentences: what problem the change solves and the approach. State the _why_ (the prior behaviour / bug) before the _what_.
- **Changes** — a bullet per logical change, each tagged with the affected module/file. Lead with the user-facing or architectural change, not mechanical edits.
- Prefer plain prose over filler; omit empty sections. Add a short **Notes** section only for non-obvious trade-offs or follow-ups.

Output the description as raw markdown in a fenced code block so it can be copied and pasted directly into the PR.
## Fork notes (nelsong6/spotify-player)

Spotify behaviors this fork depends on, each verified live (2026-10-23 to 10-25):

- **Spotify's own apps drop a bare track list.** `PUT /me/player/play` with `uris` on an
  official app returns 204 and plays nothing; `/me/player` then shows
  `currently_playing_type: unknown` and no item. Only the integrated player (librespot)
  plays one. A real playlist plays everywhere, which is why kept playlists exist.
- **librespot doesn't report plays** (librespot#913), so the integrated player's plays never
  reach the listening history. `play_report.rs` sends librespot-java's three events to
  `hm://event-service/v1/events` with method `POST`; `SEND`, librespot's public method,
  gets 404.
- **librespot keeps its old copy of a context it's told to play again**, so refilling a
  playlist and replaying its URI plays stale tracks. Adding songs ahead of the playing one
  and removing songs behind it are both picked up, by librespot and the official apps.
- **Playlists created through the API are public** whatever `public` says, on create and on
  update.
- **One broken playlist empties `/me/playlists`**: a page containing a playlist Spotify
  answers 502 for comes back with no items, so the library shows no playlists.
- **Recently played** keeps the last 50 plays, each with its context; skipped songs count.
- **ncspot's client ID accepts only `/ncspot_login`** as its redirect path (any loopback
  port); `/login` gets "redirect_uri: no matching configuration". Upstream still sends
  `/login` for the fallback client, which only shows on a first sign-in, since refreshing a
  cached token never checks the redirect URI.

Working on the fork:

- `publish.ps1` publishes CI's build of the pushed commit to `\NelsonNAS\tools\spt`:
  the Windows exe plus `fzt.dll` (pinned in `fzt.lock`, from fzt-terminal releases) and the
  Linux binary `nas/` runs for the session, then, when the session's program changed (a file
  in CI's `session-sources.txt`, from Cargo's dependency file, or the dependencies), restarts
  only the session's waiting slot, which takes over between songs
  (`handover.rs`, OpenProject #78), and restarts every spt running in a tmux pane on the
  PCs, in its pane (`restart-spt.ps1`, over ssh for the other PCs; one asleep is skipped and
  updates when spt next opens). It skips a tab still on a tmux older than win32.7-7, whose
  `respawn-pane -k` crashed the whole tab (2026-10-26, it took a Claude session with it). CI's `windows-build` and `linux-build`
  jobs run on PRs and on master, alongside the checks rather than after them;
  `publish.ps1` takes them only from a run that passed in full. For a squash merge whose
  tree matches its PR's last commit it takes the PR run's build instead of waiting for the
  run on master. With `Swatinem/rust-cache`
  warm, push to publishable binary measured 3m11s (19 min uncached and chained; 12.5 min on
  a cold cache).
- On Windows the cast's output follows the system's default output through Windows'
  default-device notice (`IMMNotificationClient`, in `cast/output.rs`). cpal 0.16 only
  opens a fixed device, never a stream that follows the default. Polling the default was
  rejected by Nelson; so was a hand-written WASAPI sink.
- The home cast (decided 2026-10-25, OpenProject #71): spt on a PC has one mode. It never
  plays on its own; it attaches to the one session on the NAS (`nas/spt-session`), plays its
  audio in sync through Snapcast, and controls it through the session's relay. Only a PC with spt open
  is an output, and the session pauses when none plays it aloud. Kept playlists are a
  separate feature, and phones play only those.
- One screen (decided 2026-10-25, OpenProject #79): every PC shows the same screen, kept by
  the session (`screen/`), since Nelson is the only user and wants no drift between rooms.
  One PC at a time controls it, by his choice: the controller changes its own screen at once
  and sends it, and a locked PC takes control with space. Rejected: every PC sending commands
  to the session and drawing what comes back (consistent too, but each change waits on the
  NAS). The lock is plain text; a 🔒 emoji left a stray border character beside it.
- One Spotify sign-in (decided 2026-10-26, OpenProject #82): only the session talks to
  Spotify. Three instances polling `/me/player` every 5 s kept the account rate-limited all
  day. A PC sends its Web API requests, lyrics, radios and covers to the session's relay
  (`relay/`, port 1707), which sends them on as its own; the session asks for its playback
  only on its player's events and pushes it to the PCs over the screen connection. The relay
  takes only requests carrying `relay.key`, which the session keeps beside the saved session
  on the NAS, where only Nelson's account and the NAS admins can read it: a PC proves itself
  through his Windows login and never signs in. The keeper runs in the slot that holds the
  relay's port, so only one slot keeps playlists.
- Our radio (decided 2026-10-26, OpenProject #86): spt is a radio built on Spotify, not a
  Spotify client. The PCs show only the radio: the session's own player while it plays, the
  saved session otherwise (`screen/` pushes it marked `left_off` when Spotify holds nothing
  for the session's player, as when Spotify drops the idle speaker). Spotify playing elsewhere,
  a phone say, changes nothing on screen. Play resumes the radio's song on the session's
  speaker, never on Spotify's active device. A PC's play asks the session to resume the radio
  (the relay's `/spt/resume`), so its resume logic applies wherever play is pressed.
- Drive and follow (decided 2026-10-26, OpenProject #86): spt drives the session by default.
  When something else drives Spotify (a phone, a Jam), the session follows (`follow.rs`): it
  reads Spotify's connect-state pushes (`hm://connect-state/v1/cluster`, the dealer stream
  librespot's own spirc reads), so following costs no Web API calls, and the PCs show what
  Spotify plays while the radio stays put. Play takes control back. While following, Spotify
  playing inside the radio's playlist moves the radio's saved place (the catch-up). While
  following, a second player in the session (not a Connect device, so it never takes over;
  verified live 2026-10-26 against a playing phone) plays along with the followed device, so the
  PCs visualize it; it's silent on a PC until play here is switched on there. The keeper
  takes the radio's position from the session and never removes the radio's song: it had
  trimmed a half-played one by Spotify's listening history, leaving the saved place on nothing.
- The PCs' sync is snapclient's algorithm (`cast/stream.rs`), timed from cpal's callback
  timestamps (when the buffer reaches the speaker). WASAPI shared mode takes only the
  device's rate, so the 44.1 kHz stream is resampled to it on arrival (`cast/resample.rs`).
- To see the UI without touching the desktop, run a test instance (its own `-c`/`-C`
  folders, its own `client_port`, `cast_server` pointing nowhere unless needed) in a
  detached tmux session on its own server
  (`tmux -L <name> new-session -d -x <cols> -y <rows> <command>`), read it
  with `capture-pane -p` (`-e` keeps the colors) and send keys with `send-keys`. Launch it
  through a `.cmd` file holding the full command line: quoted Windows paths passed straight
  to `new-session` were dropped, and the instance silently used the real cache. A test with
  an empty `-C` folder runs every first-time sign-in.
- No Actions run on a PR, not even a queued one: GitHub may have disabled workflows on
  this fork (its Actions page says so and has the button to re-enable them).
