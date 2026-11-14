# Testing and updating the running app

spt on a PC is a client of the home cast's session on the home cluster: it shows the
radio, controls it and plays its audio here. AGENTS.md has the architecture and the
CI checks; this file is how to try a change without touching Nelson's music, how to
show it to him, and how his open window gets a new release.

## What a test must never touch

His music is live in the session, and every PC's window is part of it. A second spt
started with the shared config (`\\NelsonNAS\spt-config`) is not a harmless copy:

- Snapcast knows a PC by its hostname (`cast/mod.rs`, the hello), so a second instance
  on this PC is the same Snapcast client as his window. It plays the session aloud a
  second time, and quitting it (`q`) sends play here off for this PC, which silences
  his window and pauses the session if no other PC plays.
- It joins the shared screen (port 1706), where space in a test takes control of
  every PC's screen, and keys sent to it change what every room shows.
- It writes the saved session (`session_file`) and reads `relay.key` beside it.

So a test instance always runs on its own config and cache folders, with
`cast_server` pointing at this PC, where nothing listens on 1704, 1706 or 1707
(check with `Get-NetTCPConnection -State Listen -LocalPort 1704,1706,1707`). It can't
reach the session, Spotify or the NAS: its pages are empty and its problem row says
it can't reach the session. A check that needs live playback or the library can't be
run in isolation; never point a test at the real session to get one. Never send keys
to his window (space there pauses or takes control), and never start, stop or skip
playback from a test.

A test instance opens this PC's default output (`Using audio device` in its log) and
plays nothing into it.

## Automated checks

CI runs formatting, tests, both clippy runs and the builds on every PR (AGENTS.md,
Verifying changes); a PR's Windows job also leaves a signed build as the
`spotify_player-windows` artifact. nelsonpc is the gaming PC: don't build here when
CI can, and cap any local build at four jobs (`cargo build -j 4`).

## Isolated check

The at menu's entry is `system:spotify tool:spt -c tool-config:spt` (at's README,
`system:`). fzt-automate resolves `tool:spt` to
`C:\Program Files\at-home\spt\current\spotify_player.exe` (from
`~\.home-sync\tools.json`) and `tool-config:spt` to `\\NelsonNAS\spt-config`, then
runs `tmux new-window -d -M spotify -t system: -o remain-on-exit=relaunch -W <client>
<exe> -c <config>` (fzt-automate `appArgs`). Since `tool-config:spt` always names the
shared config, running the real menu action anywhere starts a second real PC (above).
The check below runs that same tmux command on a throwaway server with a test config.
Confirm the entry itself by reading it: `~\.fzt-automate\menu-cache.yaml` has the
line above, and `~\.home-sync\tools.json` lists `spt`.

Build the test folder from the shared config, changing only what points at shared
state:

```powershell
$sptQa = Join-Path $env:TEMP ('spt-qa-' + [guid]::NewGuid().ToString('N').Substring(0, 8))
$sptQaConfig = Join-Path $sptQa 'config'
$sptQaCache = Join-Path $sptQa 'cache'
New-Item -ItemType Directory -Path $sptQaConfig, $sptQaCache, "$sptQa\sets" | Out-Null
Copy-Item '\\NelsonNAS\spt-config\menu.yaml' $sptQaConfig
$sptQaToml = Get-Content '\\NelsonNAS\spt-config\app.toml' | ForEach-Object {
    if ($_ -like 'session_file*') { "session_file = '$sptQa\last_session.json'" }
    elseif ($_ -like 'client_port*') { 'client_port = 18080' }
    else { $_ }
}
@('cast_server = "127.0.0.1"') + $sptQaToml | Set-Content (Join-Path $sptQaConfig 'app.toml') -Encoding utf8
```

Start a throwaway server (from pwsh; from Git Bash `unset SHELL` first) with a
`system` session, give it a session-set folder so `reopen-window` works there, and open
spt the way the menu does. The program is the installed release run where it is
installed, or a test build (Previewing, below); never copy a released binary.

```powershell
tmux-throwaway start spt-qa -- -s system -x 160 -y 45
tmux -L spt-qa set -g '@session-set-dir' ("$sptQa\sets" -replace '\\', '/')
$sptQaExe = '"C:\Program Files\at-home\spt\current\spotify_player.exe"'
tmux -L spt-qa new-window -d -M spotify -t system: -o remain-on-exit=relaunch $sptQaExe -c $sptQaConfig -C $sptQaCache
tmux -L spt-qa list-windows -t system -F '#{window_index} #{window_name} #{@spotify-window} #{remain-on-exit}'
tmux -L spt-qa capture-pane -p -t system:spotify
```

The quotes inside `$sptQaExe` are what fzt-automate passes too. Allow a few seconds
for the first frame. Then confirm the process is the test one, not his:

```powershell
Get-CimInstance Win32_Process -Filter "Name='spotify_player.exe'" |
    Select-Object ProcessId, ParentProcessId, CommandLine
```

His window's process has `-c \\NelsonNAS\spt-config`; the test's has the `spt-qa-*`
folders. Capture the pane before every key you send and send one key at a time:
`Enter` in the palette runs whatever is selected (`edit menu` opens an editor on his
desktop). Check what the change touches, and:

- The window is `spotify`, marked `@spotify-window` 1, with `remain-on-exit`
  `relaunch`. Running the `new-window -M` line again leaves one window and one
  process.
- The red problem row says the session can't be reached and the page says it is
  loading: the disconnected state is visible, not a blank screen.
- `/` opens the palette with the key's commands; typing searches it. Escape first
  leaves the search for the list, then clears it, then closes the palette (fzt's
  design). `:` holds `info`; select it and Enter shows Repo, Link, Version, Built,
  Installed and Checkout, and any key closes it.
- The log is `$sptQaCache\spotify-player-*.log`, each line starting with an RFC 3339
  UTC time, and it names the refused connections. Nothing new appears in
  `%USERPROFILE%\.cache\spotify-player`.
- **Quitting.** `q` ends spt and the window stays, dead, with `spotify stopped (exit
  0). Press Enter to relaunch.`; Enter starts it again.
- **Restore** (checklist item 6). Close the window and reopen it:

  ```powershell
  tmux -L spt-qa kill-window -t system:spotify
  tmux -L spt-qa reopen-window -d
  tmux -L spt-qa list-panes -s -t system -F '#{pane_id} #{pane_current_command} #{pane_dead} #{pane_start_command}'
  tmux -L spt-qa capture-pane -p -t system:spotify
  ```

  It comes back at its place as `spotify`, still marked `@spotify-window` 1 with
  `remain-on-exit relaunch`, running the same command with the test folders, and draws
  its screen. A tmux restore reruns the same saved command.

Selection is checked in at-terminal itself (Shift+drag across text and paste): a tmux
capture doesn't exercise the terminal's selection.

Clean up: `tmux-throwaway end spt-qa`, confirm no `spotify_player.exe` with `spt-qa-`
in its command line is left and his window's process is the same as before, then
remove `$sptQa` after checking it is the `spt-qa-*` folder under `$env:TEMP`.

## Previewing a change for Nelson

A change to what he sees is shown to him before it ships. Run the PR's build in the
isolated check above in place of the installed one: the PR's Windows job uploads it,
signed, with `fzt.dll`, as the `spotify_player-windows` artifact. Find the one whose
run built the PR's head commit:

```powershell
$cred = "protocol=https`nhost=forgejo.romaine.life`n`n" | git credential fill
$forgejo = @{ Authorization = 'token ' + (($cred | Where-Object { $_ -like 'password=*' }) -replace '^password=', '') }
$api = 'https://forgejo.romaine.life/api/v1/repos/nelsong6/spotify-player'
$head = git rev-parse HEAD
$artifact = @(Invoke-RestMethod -Headers $forgejo "$api/actions/artifacts?name=spotify_player-windows") |
    Where-Object { (Invoke-RestMethod -Headers $forgejo "$api/actions/runs/$($_.run_id)").commit_sha -eq $head } |
    Select-Object -First 1
Invoke-WebRequest -Headers $forgejo $artifact.archive_download_url -OutFile "$sptQa\build.zip"
Expand-Archive "$sptQa\build.zip" "$sptQa\build"
Get-AuthenticodeSignature "$sptQa\build\spotify_player.exe", "$sptQa\build\fzt.dll" | Select-Object Status, SignerCertificate
$sptQaExe = '"' + "$sptQa\build\spotify_player.exe" + '"'
```

Both are `Valid`, `CN=at-terminal`. Never preview the build of a commit already
released: it is the released binary, and a copy of that is never made. A local build
(`cargo build -j 4 --release`) is the fallback; it is unsigned and needs `fzt.dll`
pinned by `fzt.lock` beside it, as CI's Fetch fzt step does.

Put it in front of him with `tmux-throwaway show spt-qa -t system`, the bottom half of
your own pane. Before telling him what it shows, read what he will see: list every pane
of your window on `at` and capture each, the shown pane is alive (`#{pane_dead}` is
`0`), and `tmux -L spt-qa capture-pane -p -t system:spotify` shows the case you're
describing. A preview can only show what an isolated instance can: say so when the
change is about live playback or the library. The turn his answer settles it, end the
server and read his window again.

## Updating his window after a release

A merge to master releases, and the release job starts home-sync on every PC.
home-sync (at-home-tools `internal/homesync/running_windows.go`) installs the release,
then looks in every live `at` server for panes whose program is `spotify_player`. When
spt changed in that run, or the pane's process isn't the installed release, it runs

```
tmux respawn-pane -k -t <pane> "C:\Program Files\at-home\spt\current\spotify_player.exe" -c \\NelsonNAS\spt-config
```

with `%USERPROFILE%\tools\spt\config` in place of the share when the share can't be
read; with neither, or `at` not answering, it leaves the pane on its old release and
reports it as pending. Nothing for you to run: wait for the release's delivery, then
check it read-only.

What the restart keeps and loses:

- **The music doesn't stop.** It plays in the session, which the restart doesn't
  touch: the song, its place, the queue, the volume and the other rooms carry on.
  tmux closes the pane's console and ends the process, so spt doesn't run its quit
  path and doesn't send play here off. The session counts a PC whose connection
  dropped as listening for 30 seconds (`listeners.rs`), so it doesn't pause even when
  this was the only PC playing.
- **This PC's audio** stops until the new process reconnects, a few seconds (4 on
  2026-11-12). It comes back as the same Snapcast client with this PC's play here
  choice (`play_here_off.json`) and chosen outputs (`outputs.json`), both in its cache
  folder.
- **The screen** comes back from the session: page, selection, popups, palette and
  visualization, as the controlling PC last sent it.
- **Control of the screen** stays where it was. The shared screen knows a PC by its
  hostname (`screen/pc.rs`), so a window that was in control comes back in control,
  and one that was locked comes back locked.
- What lives only in the old process is gone: an open info panel and anything typed
  but not run.

Check it with read-only commands; send no keys to his pane:

```powershell
(Get-Item 'C:\Program Files\at-home\spt\current').Target
& 'C:\Program Files\at-home\spt\current\spotify_player.exe' --version
log-search "restarted spt pane" --pc $env:COMPUTERNAME.ToLower() --since (Get-Date -Format yyyy-MM-dd)
tmux -L at list-panes -a -F '#{pane_id}|#{pane_current_command}|#{pane_pid}|#{pane_dead}' | Select-String '\|spotify_player\|'
Get-CimInstance Win32_Process -Filter "Name='spotify_player.exe'" | Select-Object ProcessId, CreationDate, CommandLine
```

The `current` junction names the new tag, the pane's `pane_pid` is the
`spotify_player.exe` whose `CreationDate` follows home-sync's `Restarted ... spt` line,
and the pane is alive. `ExecutablePath` reads `current` before and after a release, so
it doesn't tell the version. Then `tmux capture-pane -p -t <pane>` shows the change.
If home-sync reported the pane pending, fix what it named and run `home-sync sync`,
which reconciles it the same way; never respawn his pane by hand.

A change to the shared config (`app.toml`, `menu.yaml`) needs no restart for the
palette, which reads `menu.yaml` each time it opens; a change to `app.toml` takes
effect at the next start, so it waits for the next release's restart or his own.
