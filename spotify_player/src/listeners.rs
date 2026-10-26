//! The session pauses when no PC is left playing it aloud. The Snapcast server that runs it tells
//! it, over the server's control port, which clients are connected and which are muted.

use std::sync::atomic::{AtomicIsize, Ordering};
use std::time::Duration;

use anyhow::Context as _;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

use crate::client::AppClient;

/// Snapcast's JSON-RPC control port, on the same host as the session
const CONTROL: (&str, u16) = ("127.0.0.1", 1705);
const RETRY_INTERVAL: Duration = Duration::from_secs(2);
const GET_STATUS: &[u8] = b"{\"id\":1,\"jsonrpc\":\"2.0\",\"method\":\"Server.GetStatus\"}\r\n";

/// PCs playing the session aloud; -1 until the server has answered
static LISTENERS: AtomicIsize = AtomicIsize::new(-1);

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
    while let Some(line) = lines.next_line().await? {
        let message: serde_json::Value = serde_json::from_str(&line)?;
        if let Some(server) = message.pointer("/result/server") {
            update(client, count_listeners(server));
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

fn count_listeners(server: &serde_json::Value) -> usize {
    let is_true =
        |v: Option<&serde_json::Value>| v.and_then(serde_json::Value::as_bool) == Some(true);
    server["groups"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|group| !is_true(group.get("muted")))
        .flat_map(|group| group["clients"].as_array().into_iter().flatten())
        .filter(|c| is_true(c.get("connected")) && !is_true(c.pointer("/config/volume/muted")))
        .count()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counts_connected_unmuted_clients_of_unmuted_groups() {
        let client = |connected: bool, muted: bool| serde_json::json!({"connected": connected, "config": {"volume": {"muted": muted, "percent": 100}}});
        let server = serde_json::json!({"groups": [
            {"muted": false, "clients": [client(true, false), client(true, true), client(false, false)]},
            {"muted": true, "clients": [client(true, false)]},
            {"muted": false, "clients": [client(true, false)]},
        ]});
        assert_eq!(count_listeners(&server), 2);
        assert_eq!(count_listeners(&serde_json::json!({"groups": []})), 0);
    }
}
