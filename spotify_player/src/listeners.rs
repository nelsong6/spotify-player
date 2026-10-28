//! The session pauses when no PC is left playing it aloud. The Snapcast server that runs it tells
//! it, over the server's control port, which clients are connected and which are muted. A PC that
//! quits turns play here off on its way out; one whose connection drops instead (restarted, crashed,
//! off the network) still counts as listening for a while, so a restart doesn't pause the radio. A PC
//! mutes another through the session's relay, which passes it on over the same port.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicIsize, Ordering};
use std::sync::LazyLock;
use std::time::{Duration, Instant};

use anyhow::Context as _;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::watch;

use crate::client::AppClient;
use crate::screen::Pc;

/// Snapcast's JSON-RPC control port, on the same host as the session
const CONTROL: (&str, u16) = ("127.0.0.1", 1705);
const RETRY_INTERVAL: Duration = Duration::from_secs(2);
/// How long a PC whose connection dropped still counts as listening; a restart is back well within
const DROPPED_GRACE: Duration = Duration::from_secs(30);
const GET_STATUS: &[u8] = b"{\"id\":1,\"jsonrpc\":\"2.0\",\"method\":\"Server.GetStatus\"}\r\n";

/// PCs playing the session aloud; -1 until the server has answered
static LISTENERS: AtomicIsize = AtomicIsize::new(-1);

/// The PCs connected to the server
static PCS: LazyLock<watch::Sender<Vec<Pc>>> = LazyLock::new(|| watch::Sender::new(Vec::new()));

/// The PCs connected to the server, as it reports changes
pub fn pcs() -> watch::Receiver<Vec<Pc>> {
    PCS.subscribe()
}

/// Mute or unmute a PC's output
pub async fn set_muted(pc: &str, muted: bool) -> anyhow::Result<()> {
    let socket = tokio::net::TcpStream::connect(CONTROL)
        .await
        .context("connect to the cast server's control port")?;
    let (reader, mut writer) = socket.into_split();
    let mut request = serde_json::to_string(&serde_json::json!({
        "id": 1,
        "jsonrpc": "2.0",
        "method": "Client.SetVolume",
        "params": {"id": pc, "volume": {"muted": muted, "percent": 100}},
    }))?;
    request.push_str("\r\n");
    writer.write_all(request.as_bytes()).await?;
    let mut lines = BufReader::new(reader).lines();
    let answer = tokio::time::timeout(Duration::from_secs(5), async {
        // notifications come first, from the same change
        while let Some(line) = lines.next_line().await? {
            let message: serde_json::Value = serde_json::from_str(&line)?;
            if message.get("id").is_some() {
                return Ok(message);
            }
        }
        anyhow::bail!("the cast server closed its control port")
    })
    .await
    .context("the cast server didn't answer")??;
    if let Some(err) = answer.get("error") {
        anyhow::bail!("the cast server refused: {err}");
    }
    Ok(())
}

/// Whether the server reported that no PC plays the session aloud
pub fn is_unheard() -> bool {
    LISTENERS.load(Ordering::Relaxed) == 0
}

/// Follow the server's clients for as long as the session runs
pub async fn watch(client: AppClient) {
    loop {
        if let Err(err) = follow(&client).await {
            tracing::warn!("Lost the cast server's client list: {err:#}");
        }
        tokio::time::sleep(RETRY_INTERVAL).await;
    }
}

async fn follow(client: &AppClient) -> anyhow::Result<()> {
    let socket = tokio::net::TcpStream::connect(CONTROL)
        .await
        .context("connect to the cast server's control port")?;
    let (reader, mut writer) = socket.into_split();
    writer.write_all(GET_STATUS).await?;
    let mut lines = BufReader::new(reader).lines();
    let mut heard = Heard::default();
    loop {
        let expiry = heard.next_expiry();
        let line = tokio::select! {
            line = lines.next_line() => line?,
            () = until(expiry) => {
                update(client, heard.count(Instant::now()));
                continue;
            }
        };
        let Some(line) = line else {
            break;
        };
        let message: serde_json::Value = serde_json::from_str(&line)?;
        if let Some(server) = message.pointer("/result/server") {
            let (listening, disconnected) = listeners(server);
            update(client, heard.hear(Instant::now(), listening, &disconnected));
            PCS.send_if_modified(|pcs| {
                let now = list_pcs(server);
                let changed = *pcs != now;
                *pcs = now;
                changed
            });
        } else if message.get("method").is_some() {
            // a client connected, left or was muted: the status says where that leaves things
            writer.write_all(GET_STATUS).await?;
        }
    }
    anyhow::bail!("the cast server closed its control port")
}

fn update(client: &AppClient, listeners: usize) {
    let before = LISTENERS.swap(listeners as isize, Ordering::Relaxed);
    if before != listeners as isize {
        tracing::info!("PCs playing the session aloud: {listeners}");
    }
    if listeners == 0 && before != 0 {
        client.pause_player();
    }
}

async fn until(deadline: Option<Instant>) {
    match deadline {
        Some(deadline) => tokio::time::sleep_until(deadline.into()).await,
        None => std::future::pending().await,
    }
}

/// The PCs that count as playing the session aloud
#[derive(Default)]
struct Heard {
    /// Connected and playing it
    listening: HashSet<String>,
    /// Playing it when their connection dropped, and when it dropped
    dropped: HashMap<String, Instant>,
}

impl Heard {
    /// Take the server's latest list: the PCs playing the session, and the ones not connected that
    /// would be
    fn hear(
        &mut self,
        now: Instant,
        listening: HashSet<String>,
        disconnected: &HashSet<String>,
    ) -> usize {
        // a PC that quit muted itself first, so it isn't among the disconnected that would play
        self.dropped.retain(|pc, _| disconnected.contains(pc));
        for pc in &self.listening {
            if disconnected.contains(pc) && !self.dropped.contains_key(pc) {
                tracing::info!(
                    "The connection of {pc} dropped; it counts as listening for a while"
                );
                self.dropped.insert(pc.clone(), now);
            }
        }
        self.listening = listening;
        self.count(now)
    }

    fn count(&mut self, now: Instant) -> usize {
        self.dropped
            .retain(|_, dropped| now.duration_since(*dropped) < DROPPED_GRACE);
        self.listening.len() + self.dropped.len()
    }

    fn next_expiry(&self) -> Option<Instant> {
        self.dropped
            .values()
            .min()
            .map(|dropped| *dropped + DROPPED_GRACE)
    }
}

/// The unmuted PCs of unmuted groups: those connected, and those not connected
fn listeners(server: &serde_json::Value) -> (HashSet<String>, HashSet<String>) {
    let is_true =
        |v: Option<&serde_json::Value>| v.and_then(serde_json::Value::as_bool) == Some(true);
    let mut listening = HashSet::new();
    let mut disconnected = HashSet::new();
    for client in server["groups"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|group| !is_true(group.get("muted")))
        .flat_map(|group| group["clients"].as_array().into_iter().flatten())
        .filter(|c| !is_true(c.pointer("/config/volume/muted")))
    {
        let Some(id) = client.get("id").and_then(serde_json::Value::as_str) else {
            continue;
        };
        if is_true(client.get("connected")) {
            listening.insert(id.to_string());
        } else {
            disconnected.insert(id.to_string());
        }
    }
    (listening, disconnected)
}

/// The connected PCs, a muted group muting its PCs
fn list_pcs(server: &serde_json::Value) -> Vec<Pc> {
    let is_true =
        |v: Option<&serde_json::Value>| v.and_then(serde_json::Value::as_bool) == Some(true);
    let text = |v: Option<&serde_json::Value>| {
        v.and_then(serde_json::Value::as_str)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
    };
    let mut pcs: Vec<Pc> = server["groups"]
        .as_array()
        .into_iter()
        .flatten()
        .flat_map(|group| {
            let group_muted = is_true(group.get("muted"));
            group["clients"]
                .as_array()
                .into_iter()
                .flatten()
                .filter(|c| is_true(c.get("connected")))
                .filter_map(move |c| {
                    Some(Pc {
                        id: text(c.get("id"))?,
                        name: text(c.pointer("/config/name"))
                            .or_else(|| text(c.pointer("/host/name")))?,
                        muted: group_muted || is_true(c.pointer("/config/volume/muted")),
                    })
                })
        })
        .collect();
    pcs.sort_by(|a, b| a.name.cmp(&b.name));
    pcs
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ids(ids: &[&str]) -> HashSet<String> {
        ids.iter().map(|id| (*id).to_string()).collect()
    }

    #[test]
    fn finds_unmuted_clients_of_unmuted_groups() {
        let client = |id: &str, connected: bool, muted: bool| serde_json::json!({"id": id, "connected": connected, "config": {"volume": {"muted": muted, "percent": 100}}});
        let server = serde_json::json!({"groups": [
            {"muted": false, "clients": [client("a", true, false), client("b", true, true), client("c", false, false)]},
            {"muted": true, "clients": [client("d", true, false)]},
            {"muted": false, "clients": [client("e", true, false)]},
        ]});
        assert_eq!(listeners(&server), (ids(&["a", "e"]), ids(&["c"])));
    }

    #[test]
    fn a_dropped_pc_counts_for_a_while_and_one_that_quit_does_not() {
        let start = Instant::now();
        let mut heard = Heard::default();
        assert_eq!(heard.hear(start, ids(&["a", "b"]), &ids(&[])), 2);
        // a restarts: its connection drops without muting
        assert_eq!(heard.hear(start, ids(&["b"]), &ids(&["a"])), 2);
        // b quits: it mutes itself, then disconnects
        assert_eq!(heard.hear(start, ids(&[]), &ids(&["a"])), 1);
        assert_eq!(heard.next_expiry(), Some(start + DROPPED_GRACE));
        assert_eq!(heard.count(start + DROPPED_GRACE / 2), 1);
        assert_eq!(heard.count(start + DROPPED_GRACE), 0);

        // back within the while, it simply listens again
        let mut heard = Heard::default();
        heard.hear(start, ids(&["a"]), &ids(&[]));
        heard.hear(start, ids(&[]), &ids(&["a"]));
        let back = start + Duration::from_secs(3);
        assert_eq!(heard.hear(back, ids(&["a"]), &ids(&[])), 1);
        assert_eq!(heard.next_expiry(), None);
    }

    #[test]
    fn lists_connected_pcs_by_name() {
        let client = |id: &str, name: &str, connected: bool, muted: bool| serde_json::json!({"id": id, "connected": connected, "host": {"name": id}, "config": {"name": name, "volume": {"muted": muted, "percent": 100}}});
        let server = serde_json::json!({"groups": [
            {"muted": false, "clients": [client("upstairspc", "", true, false), client("gone", "", false, false)]},
            {"muted": true, "clients": [client("nelsonpc", "Desk", true, false)]},
        ]});
        let pc = |id: &str, name: &str, muted: bool| Pc {
            id: id.to_string(),
            name: name.to_string(),
            muted,
        };
        assert_eq!(
            list_pcs(&server),
            vec![
                pc("nelsonpc", "Desk", true),
                pc("upstairspc", "upstairspc", false)
            ]
        );
    }
}
