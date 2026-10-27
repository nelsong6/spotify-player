# spt: design

spt is a home radio. Spotify is its backend: the catalog, the audio and the playlists' storage.
It is not a Spotify client, and keeping in step with Spotify's own apps is not a goal.

Nelson is the only user. The house is the NAS and the PCs (nelsonpc, upstairspc, treadmill).

## The parts

- **The session** (on the NAS) is the radio and its one output device. The music plays there
  and keeps playing there; only a deliberate act moves it. It is the only player and the only
  thing signed in to Spotify. It holds the radio's place, keeps the kept playlists, and stays
  within its own request budget.
- **A PC** is a client of the session: it shows the radio, controls it, and plays it *here*
  (aloud, in sync with the other PCs) when play here is on. It never plays on its own and never
  talks to Spotify.
- **The screen** is one screen, shown on every PC. One PC at a time controls it.

## Play here

Every spt client chooses whether it plays the session's audio here. Play here off, the client
still shows the radio and controls it, and the music goes on in the other rooms. The session's
audio is the same everywhere; only whether a room hears it differs.

## Features

Every feature spt has, what it does today, and whether it is part of the radio. **In** and
**Out** are Nelson's decisions (2026-10-27: every existing feature not questioned stays as it is).

### Playing

| Feature | Key | What it does today | Radio |
| --- | --- | --- | --- |
| Play / pause | `space` | Plays or pauses the session. Play resumes the radio's saved song at its saved place, on the session. | In |
| Next, previous | `n`, `p` | Skips within what plays; the saved place follows. | In |
| Seek | `<`, `>`, `^` | Back or forward 5 s; back to the song's start. | In |
| Volume | `+`, `-` | The session's volume, the same in every room. | In |
| Mute everywhere | `_` | The session's volume to 0 and back. | In |
| Shuffle, repeat | `C-s`, `C-r` | Spotify's shuffle and repeat on what plays. | In |
| Play a random song | `.` | A random song of what plays. | In |
| Continue mode | `C` | What plays when a list runs out: a radio of the playlist, a radio of the last song, or stop. | In |
| Queue | `z`, `Z` | Shows what plays next; adds the selected song to it. | In |

### What plays

| Feature | Key | What it does today | Radio |
| --- | --- | --- | --- |
| Kept playlists | (palette) | spt radio and Bike, always from where they left off. The session removes played songs and refills spt radio from radios of ambient electronic. | In |
| One-off radio | `g R`, palette | A radio from one song, or from a random song of a playlist. | In |
| The library | `g l`, `u p`, `u a`, `u A`, `g y` | Browse and play your playlists, artists, albums and liked songs. | In |
| Search | `g s` | Search Spotify and play a result. | In |
| Top tracks, recently played | `g t`, `g r` | Your Spotify top tracks and listening history. | In |
| Browse | `g b` | Spotify's categories and their playlists. | In |
| Open a Spotify link | `O` | Opens a Spotify link from the clipboard. | In |
| Refresh the library | `R` | Loads the library from Spotify again; until then it may be stale. | In |

### Changing Spotify's library

| Feature | Key | What it does today | Radio |
| --- | --- | --- | --- |
| Like, add to playlist, follow | `a`, `A` (actions) | Likes a song, adds it to a playlist, saves an album, follows an artist. | In |
| Edit a playlist | `C-k`, `C-j`, actions | Reorders or removes a playlist's songs. | In |
| Create a playlist | `N` | Makes a new playlist. | In |

### Where the audio goes

| Feature | Key | What it does today | Radio |
| --- | --- | --- | --- |
| Play here | `H` | Turns play here on or off for this client. | In |
| Play here on another PC | `P` | Turns play here on or off for any other client, with the relay key every PC holds. | In |
| This PC's output | none | Plays on Windows' default output and follows a change of it; Windows chooses it, not spt. | In |
| Another PC's output | none | Nothing; each PC's Windows chooses its own. | Out for now; may change |
| Spotify devices | `D` | Lists Spotify's devices and moves playback to one, which stops the radio on Home. The radio has no sense of sending its playback elsewhere: every PC is just a client of the session. | Out |
| Pause with nobody listening | (automatic) | The session pauses when no PC plays it aloud, so the radio doesn't advance unless someone is listening. | In |

### The screen

| Feature | Key | What it does today | Radio |
| --- | --- | --- | --- |
| One screen, one controller | `space` on a locked PC | Every PC shows the same screen; space takes control. | In |
| Visualization | `V` | Audio bars of what plays. | In |
| ASCII cover | `I` | The album cover beside the bars: pulse, still, off. | In |
| Lyrics | `l` | Synced lyrics of the song. | In |
| Command palette | `/` | Search and run the menu (`menu.yaml`). | In |
| Themes | `T` | Switches the theme on this PC only. | In |
| Sorting | `s ...` | Sorts track lists and the library. | In |
| Help, logs | `?`, `g o` | Every key; the app's log. | In |
| Mouse capture | `M` | Off lets the terminal select text. | In |

### Behind the scenes

| Feature | What it does today | Radio |
| --- | --- | --- |
| One Spotify app, one sign-in | Only the session talks to Spotify, with Nelson's own app; no fallback client. | In |
| Request budget | Every request goes under 10 per 30 s and 200 per hour. | In |
| Upgrades don't stop the music | A new release takes over between songs. | In |
| Plays reported | The session's plays show in Spotify's listening history. | In |
| Command line | `spt playback ...`, `spt start ...`, `spt playlist ...` and others, run against the open spt. | In |
| OS media keys | Off in the shared config. | In |
| Following Spotify | `F`, or automatically when something else plays: the PCs show what Spotify plays elsewhere. | On hold (#89, #87) |

## Not supported

- A live library.
- Phones as speakers. A phone plays only the kept playlists, through Spotify's own app.
- More than one user.
