//! A new release takes over the session between songs, so publishing never stops the music.
//!
//! Two session slots run under Snapcast, whose `meta` source plays whichever of them is playing.
//! A lock on `active.lock` in the slots' folder says which slot is the session; the other waits
//! for the lock. A waiting slot started on a newer release than the session's connects to Spotify
//! first, as a second device, and asks for the handover in `handover.json`. The session moves the
//! playback to it when a song ends or while the playback is paused, then exits; the new release
//! gets the lock, and Snapcast starts the exited slot again as the next one to wait.
//!
//! The slot that isn't the session writes its process id to `waiting.pid`: publishing a release
//! ends that process, and Snapcast starts it again on the new release.

use std::fs::File;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::OnceLock;
use std::time::Duration;

use anyhow::Context as _;
use serde::{Deserialize, Serialize};

use crate::client::AppClient;
use crate::state::SharedState;
use rspotify::prelude::OAuthClient as _;

/// How often the session looks for a handover request while nothing plays
const IDLE_CHECK: Duration = Duration::from_secs(2);
/// How long a failed handover waits before trying again, so a refusal from Spotify isn't
/// answered with more requests
const RETRY_AFTER_FAILURE: Duration = Duration::from_secs(30);

#[derive(Serialize, Deserialize)]
struct Active {
    release: String,
}

#[derive(Serialize, Deserialize)]
struct Request {
    device_id: String,
}

struct Slots {
    dir: PathBuf,
    release: String,
    /// Held for as long as this process is the session
    lock: File,
}

static SLOTS: OnceLock<Slots> = OnceLock::new();
/// Set when this slot started beside an older release's session, to take over from it
static TAKING_OVER: AtomicBool = AtomicBool::new(false);

/// What this slot does on start
pub enum Start {
    /// This slot is the session
    Session,
    /// A session on an older release runs: connect beside it and take the playback over
    TakeOver,
}

/// Find this slot's role. While a session of the same release runs, wait (for as long as it
/// lasts) to become the session.
pub fn claim(dir: &Path, release: &str) -> anyhow::Result<Start> {
    std::fs::create_dir_all(dir)?;
    let lock = File::options()
        .create(true)
        .truncate(false)
        .write(true)
        .open(dir.join("active.lock"))
        .context("open the slots' lock")?;
    let slots = Slots {
        dir: dir.to_path_buf(),
        release: release.to_string(),
        lock,
    };
    let start = if slots.lock.try_lock().is_ok() {
        Start::Session
    } else {
        std::fs::write(dir.join("waiting.pid"), std::process::id().to_string())?;
        if slots.active_release().as_deref() == Some(release) {
            tracing::info!("Waiting to take over from the session, on the same release {release}");
            slots.lock.lock()?;
            Start::Session
        } else {
            Start::TakeOver
        }
    };
    match start {
        Start::Session => slots.record_active()?,
        Start::TakeOver => TAKING_OVER.store(true, Ordering::SeqCst),
    }
    let _ = SLOTS.set(slots);
    Ok(start)
}

impl Slots {
    fn active_release(&self) -> Option<String> {
        let data = std::fs::read(self.dir.join("active.json")).ok()?;
        serde_json::from_slice::<Active>(&data)
            .ok()
            .map(|a| a.release)
    }

    fn record_active(&self) -> anyhow::Result<()> {
        tracing::info!("This slot is the session, on release {}", self.release);
        let _ = std::fs::remove_file(self.dir.join("handover.json"));
        let waiting = self.dir.join("waiting.pid");
        if std::fs::read_to_string(&waiting).ok() == Some(std::process::id().to_string()) {
            let _ = std::fs::remove_file(waiting);
        }
        let active = Active {
            release: self.release.clone(),
        };
        std::fs::write(self.dir.join("active.json"), serde_json::to_vec(&active)?)?;
        Ok(())
    }

    /// The device that asked to take over, other than this one
    fn requested(&self, own_device: &str) -> Option<String> {
        let data = std::fs::read(self.dir.join("handover.json")).ok()?;
        serde_json::from_slice::<Request>(&data)
            .ok()
            .map(|r| r.device_id)
            .filter(|id| id != own_device)
    }
}

/// Called once this process's player is connected as `device_id`. A slot taking over then asks
/// the session for the playback, and becomes the session once it lets go.
pub fn connected(device_id: &str) -> anyhow::Result<()> {
    if !TAKING_OVER.swap(false, Ordering::SeqCst) {
        return Ok(());
    }
    let slots = SLOTS.get().context("no session slots")?;
    tracing::info!(
        "Asking the session (release {:?}) to hand over to release {}",
        slots.active_release(),
        slots.release
    );
    let request = Request {
        device_id: device_id.to_string(),
    };
    std::fs::write(
        slots.dir.join("handover.json"),
        serde_json::to_vec(&request)?,
    )?;
    std::thread::Builder::new()
        .name("handover-wait".to_string())
        .spawn(|| {
            let slots = SLOTS.get().expect("set above");
            match slots.lock.lock() {
                Ok(()) => {
                    if let Err(err) = slots.record_active() {
                        tracing::error!("Failed to record this slot as the session: {err:#}");
                    }
                }
                Err(err) => tracing::error!("Failed to wait for the session's lock: {err:#}"),
            }
        })?;
    Ok(())
}

/// The device waiting to take over, when this process is the session and one has asked
pub fn pending(state: &SharedState) -> Option<String> {
    let own = state.session.lock().own_device.clone()?;
    SLOTS.get()?.requested(&own)
}

/// Move the playback to `device_id`, playing or paused as `play` says, and exit
pub async fn hand_over(client: &AppClient, state: &SharedState, device_id: &str, play: bool) {
    tracing::info!("Handing the session over to device_id={device_id}");
    // the next song would start here otherwise
    client.pause_player();
    let moved = match client.hand_off_track_list(state, device_id, play).await {
        Ok(true) => Ok(()),
        Ok(false) => client
            .transfer_playback(device_id, Some(play))
            .await
            .map_err(anyhow::Error::from),
        Err(err) => Err(err),
    };
    if let Err(err) = moved {
        // keep the session; the next song change tries again
        tracing::error!("Failed to hand the session over: {err:#}");
        return;
    }
    // the paused state moves with the playback, whatever the transfer asks for
    if play {
        if let Err(err) = client.resume_playback(Some(device_id), None).await {
            tracing::error!("Failed to start the playback on the new release: {err:#}");
        }
    }
    tracing::info!("Handed the session over; exiting");
    std::process::exit(0);
}

/// While nothing plays, hand over as soon as it's asked for
pub async fn watch_while_idle(client: AppClient, state: SharedState) {
    loop {
        tokio::time::sleep(IDLE_CHECK).await;
        let playing = state
            .player
            .read()
            .buffered_playback
            .as_ref()
            .is_some_and(|p| p.is_playing);
        if playing {
            continue;
        }
        let Some(device_id) = pending(&state) else {
            continue;
        };
        // Spotify has forgotten the idle session's playback: nothing to move, and the new
        // release starts from the saved session
        if state.player.read().playback.is_none() {
            tracing::info!("Nothing to hand over to device_id={device_id}; exiting");
            std::process::exit(0);
        }
        hand_over(&client, &state, &device_id, false).await;
        tokio::time::sleep(RETRY_AFTER_FAILURE).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn slots(dir: &Path, release: &str) -> Slots {
        let lock = File::options()
            .create(true)
            .truncate(false)
            .write(true)
            .open(dir.join("active.lock"))
            .unwrap();
        Slots {
            dir: dir.to_path_buf(),
            release: release.to_string(),
            lock,
        }
    }

    #[test]
    fn one_slot_holds_the_lock_and_the_other_sees_its_release() {
        let dir = std::env::temp_dir().join(format!("spt-handover-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let session = slots(&dir, "v1");
        assert!(session.lock.try_lock().is_ok());
        session.record_active().unwrap();

        let next = slots(&dir, "v2");
        assert!(next.lock.try_lock().is_err());
        assert_eq!(next.active_release().as_deref(), Some("v1"));

        // the session ignores a request from itself, and sees one from the new release
        std::fs::write(dir.join("handover.json"), br#"{"device_id":"new"}"#).unwrap();
        assert_eq!(session.requested("old").as_deref(), Some("new"));
        assert_eq!(session.requested("new"), None);

        // letting go hands the lock to the waiting slot, which clears the request
        drop(session);
        assert!(next.lock.try_lock().is_ok());
        next.record_active().unwrap();
        assert_eq!(next.active_release().as_deref(), Some("v2"));
        assert_eq!(next.requested("old"), None);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
