# spt: design

spt is a home radio. Spotify is its backend: the catalog, the audio and the playlists' storage.
It is not a Spotify client, and keeping in step with Spotify's own apps is not a goal.

Nelson is the only user. The house is the NAS and the PCs (nelsonpc, upstairspc, treadmill).

## The parts

- **The session** (on the NAS) is the radio. It is the only player and the only thing signed in
  to Spotify. It holds the radio's place, keeps the kept playlists, and stays within its own
  request budget.
- **A PC** is a speaker and a screen. It plays the session's audio in sync with the other PCs,
  and controls the session. It never plays on its own and never talks to Spotify.
- **The screen** is one screen, shown on every PC. One PC at a time controls it.

## What plays

- The radio: a kept playlist (spt radio, Bike), always from where it left off.
- A one-off radio from a song.
- Anything from the library or a search: an album, a playlist, liked songs, a song.

## Controls

- Play, pause, skip, volume: the session's, so the same in every room.
- Mute this PC (`H`): its speakers go quiet, the screen stays, the other rooms play on.
- Mute another PC (`P`), the same way.
- Mute everywhere (the palette's "mute"): the session's volume to 0 and back.

## Spotify as a backend

- One Spotify app, Nelson's own. No fallback client.
- Every request goes through the session's budget. The library is kept on disk and may be stale
  until a refresh (`R`).
- A playlist edited on a phone shows in spt after a refresh.

## Not supported

- Following what Spotify plays elsewhere, and Jams (follow mode; on hold, OpenProject #89, #87).
- A live library.
- Phones as speakers. A phone plays only the kept playlists, through Spotify's own app.
- More than one user.
