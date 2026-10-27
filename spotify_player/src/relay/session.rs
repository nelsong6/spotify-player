use super::{key_path, API_PREFIX, KEY_HEADER, PORT};
use crate::client::AppClient;
use anyhow::Context as _;
use base64::Engine as _;
use http_body_util::{BodyExt as _, Full};
use hyper::{body::Bytes, Method, Request, Response, StatusCode};
use rspotify::{
    http::{HttpError, Query},
    model::TrackId,
    prelude::*,
    ClientError,
};
use std::convert::Infallible;
use std::path::Path;

type Reply = Response<Full<Bytes>>;

pub async fn serve(client: AppClient, state: crate::state::SharedState) {
    let mut logged = false;
    let listener = loop {
        match tokio::net::TcpListener::bind(("0.0.0.0", PORT)).await {
            Ok(listener) => break listener,
            Err(err) if !logged => {
                tracing::info!("Waiting for port {PORT} for the relay: {err:#}");
                logged = true;
            }
            Err(_) => {}
        }
        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
    };
    if let Err(err) = key(&key_path()) {
        tracing::error!("Failed to make the relay's key: {err:#}");
    }
    tokio::spawn(crate::keeper::run(client.clone(), state.clone()));
    loop {
        let stream = match listener.accept().await {
            Ok((stream, _)) => stream,
            Err(err) => {
                tracing::warn!("Failed to accept a relay connection: {err:#}");
                continue;
            }
        };
        let client = client.clone();
        let state = state.clone();
        tokio::spawn(async move {
            let service = hyper::service::service_fn(move |request| {
                let client = client.clone();
                let state = state.clone();
                async move { Ok::<_, Infallible>(handle(&client, &state, request).await) }
            });
            if let Err(err) = hyper::server::conn::http1::Builder::new()
                .serve_connection(hyper_util::rt::TokioIo::new(stream), service)
                .await
            {
                tracing::debug!("A relay connection ended: {err:#}");
            }
        });
    }
}

/// The key, made if there is none
fn key(path: &Path) -> anyhow::Result<String> {
    match std::fs::read_to_string(path) {
        Ok(key) if !key.trim().is_empty() => Ok(key.trim().to_string()),
        Err(err) if err.kind() != std::io::ErrorKind::NotFound => {
            Err(err).with_context(|| format!("read {}", path.display()))
        }
        _ => {
            let mut bytes = [0u8; 32];
            rand::fill(&mut bytes);
            let key = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes);
            std::fs::write(path, &key).with_context(|| format!("write {}", path.display()))?;
            tracing::info!("Made a new relay key in {}", path.display());
            Ok(key)
        }
    }
}

fn reply(status: StatusCode, body: impl Into<Bytes>) -> Reply {
    let mut response = Response::new(Full::new(body.into()));
    *response.status_mut() = status;
    response
}

async fn handle(
    client: &AppClient,
    state: &crate::state::SharedState,
    request: Request<hyper::body::Incoming>,
) -> Reply {
    let sent = request
        .headers()
        .get(KEY_HEADER)
        .and_then(|key| key.to_str().ok());
    match key(&key_path()) {
        Ok(key) if sent == Some(key.as_str()) => {}
        Ok(_) => return reply(StatusCode::UNAUTHORIZED, "wrong relay key"),
        Err(err) => {
            tracing::error!("Failed to read the relay's key: {err:#}");
            return reply(StatusCode::INTERNAL_SERVER_ERROR, "no relay key");
        }
    }
    let method = request.method().clone();
    let path_and_query = request
        .uri()
        .path_and_query()
        .map_or("/", |p| p.as_str())
        .to_string();
    let body = match request.into_body().collect().await {
        Ok(body) => body.to_bytes(),
        Err(err) => return reply(StatusCode::BAD_REQUEST, format!("{err:#}")),
    };
    let result = if let Some(endpoint) = path_and_query.strip_prefix(API_PREFIX) {
        return forward(client, &method, endpoint, &body).await;
    } else if let Some(id) = path_and_query.strip_prefix("/spt/lyrics/") {
        lyrics(client, id).await
    } else if let Some(seed) = path_and_query.strip_prefix("/spt/radio/") {
        radio(client, seed).await
    } else if let Some(query) = path_and_query.strip_prefix("/spt/image?") {
        image(client, query).await
    } else if path_and_query == "/spt/refresh" && method == Method::POST {
        crate::client::refresh_library();
        Ok(Vec::new())
    } else if path_and_query == "/spt/resume" && method == Method::POST {
        crate::client::until_accepted("resume the radio", || client.resume_saved_session(state))
            .await
            .map(|()| Vec::new())
    } else {
        return reply(StatusCode::NOT_FOUND, "");
    };
    match result {
        Ok(body) => reply(StatusCode::OK, body),
        Err(err) => {
            tracing::warn!("Relay: {path_and_query} failed: {err:#}");
            reply(StatusCode::BAD_GATEWAY, format!("{err:#}"))
        }
    }
}

/// Send a PC's Web API request to Spotify as the session's own
async fn forward(client: &AppClient, method: &Method, endpoint: &str, body: &[u8]) -> Reply {
    let payload = if body.is_empty() {
        serde_json::Value::Null
    } else {
        match serde_json::from_slice(body) {
            Ok(payload) => payload,
            Err(err) => return reply(StatusCode::BAD_REQUEST, format!("{err:#}")),
        }
    };
    let result = match *method {
        Method::GET => client.api_get(endpoint, &Query::new()).await,
        Method::POST => client.api_post(endpoint, &payload).await,
        Method::PUT => client.api_put(endpoint, &payload).await,
        Method::DELETE => client.api_delete(endpoint, &payload).await,
        _ => return reply(StatusCode::METHOD_NOT_ALLOWED, ""),
    };
    match result {
        Ok(text) if text.is_empty() => reply(StatusCode::NO_CONTENT, ""),
        Ok(text) => reply(StatusCode::OK, text),
        Err(ClientError::Http(err)) => match *err {
            // Spotify's own refusal, passed on whole so the PC waits as Spotify asks
            HttpError::StatusCode(response) => {
                let status = StatusCode::from_u16(response.status().as_u16())
                    .unwrap_or(StatusCode::BAD_GATEWAY);
                let retry_after = response
                    .headers()
                    .get(reqwest::header::RETRY_AFTER)
                    .cloned();
                let mut reply = reply(status, response.bytes().await.unwrap_or_default().to_vec());
                if let Some(retry_after) = retry_after {
                    if let Ok(value) =
                        hyper::header::HeaderValue::from_bytes(retry_after.as_bytes())
                    {
                        reply
                            .headers_mut()
                            .insert(hyper::header::RETRY_AFTER, value);
                    }
                }
                reply
            }
            HttpError::Client(err) => reply(StatusCode::BAD_GATEWAY, format!("{err:#}")),
        },
        Err(err) => reply(StatusCode::BAD_GATEWAY, format!("{err:#}")),
    }
}

/// A track's lyrics as `[milliseconds, line]` pairs, or `null` without any
async fn lyrics(client: &AppClient, id: &str) -> anyhow::Result<Vec<u8>> {
    let lyrics = client.lyrics(TrackId::from_id(id)?.into_static()).await?;
    let lines = lyrics.map(|lyrics| {
        lyrics
            .lines
            .into_iter()
            .map(|(at, line)| (at.num_milliseconds(), line))
            .collect::<Vec<_>>()
    });
    Ok(serde_json::to_vec(&lines)?)
}

/// The ids of a radio's tracks, from a seed's URI
async fn radio(client: &AppClient, seed: &str) -> anyhow::Result<Vec<u8>> {
    let ids = client.radio_track_ids(seed).await?;
    let ids: Vec<String> = ids.iter().map(|id| id.id().to_string()).collect();
    Ok(serde_json::to_vec(&ids)?)
}

/// A cover, from Spotify's image servers only
async fn image(client: &AppClient, query: &str) -> anyhow::Result<Vec<u8>> {
    let url = reqwest::Url::parse(&format!("http://relay/?{query}"))?
        .query_pairs()
        .find(|(name, _)| name == "url")
        .map(|(_, url)| url.into_owned())
        .context("no url")?;
    let parsed = reqwest::Url::parse(&url)?;
    let host = parsed.host_str().unwrap_or_default();
    anyhow::ensure!(
        parsed.scheme() == "https"
            && (host.ends_with(".scdn.co") || host.ends_with(".spotifycdn.com")),
        "not a Spotify image: {url}"
    );
    client.fetch_image(&url).await
}

#[cfg(test)]
mod tests {
    #[test]
    fn makes_a_key_once_and_a_new_one_after_it_is_deleted() {
        let dir = std::env::temp_dir().join(format!("spt-relay-key-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("relay.key");
        let _ = std::fs::remove_file(&path);

        let first = super::key(&path).unwrap();
        assert_eq!(first.len(), 43);
        assert_eq!(super::key(&path).unwrap(), first);
        std::fs::remove_file(&path).unwrap();
        assert_ne!(super::key(&path).unwrap(), first);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
