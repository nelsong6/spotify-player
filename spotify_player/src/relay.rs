//! The relay: the PCs' only way to Spotify. The session holds the house's one Spotify sign-in; a
//! PC sends it the Web API requests it would have sent Spotify, plus the lyrics, radios and covers
//! that need the session's sign-in or reach. Every request carries a key the session keeps beside
//! the saved session, in a folder only Nelson's NAS account can read, so a PC proves itself
//! through his Windows login. Deleting the key revokes it; the session makes a new one.

use std::path::PathBuf;

/// The session's port for the relay
pub const PORT: u16 = 1707;
/// The header carrying the key
const KEY_HEADER: &str = "x-spt-key";
/// Where the Web API's endpoints start, on Spotify and on the relay
const API_PREFIX: &str = "/v1/";

/// The key's file, beside the saved session
fn key_path() -> PathBuf {
    crate::config::get_config()
        .session_folder()
        .join("relay.key")
}

/// The session's side: serves the relay while this slot is the active one, and keeps the
/// playlists from there, so only one slot ever does either.
#[cfg(feature = "streaming")]
pub mod session {
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

    pub async fn serve(client: AppClient) {
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
        tokio::spawn(crate::keeper::run(client.clone()));
        loop {
            let stream = match listener.accept().await {
                Ok((stream, _)) => stream,
                Err(err) => {
                    tracing::warn!("Failed to accept a relay connection: {err:#}");
                    continue;
                }
            };
            let client = client.clone();
            tokio::spawn(async move {
                let service = hyper::service::service_fn(move |request| {
                    let client = client.clone();
                    async move { Ok::<_, Infallible>(handle(&client, request).await) }
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

    async fn handle(client: &AppClient, request: Request<hyper::body::Incoming>) -> Reply {
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
                    let mut reply =
                        reply(status, response.bytes().await.unwrap_or_default().to_vec());
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
}

/// A PC's side: the session's address and the key, sent with every request
#[cfg(feature = "cast")]
pub mod pc {
    use super::{key_path, API_PREFIX, KEY_HEADER, PORT};
    use anyhow::Context as _;
    use http::Extensions;
    use parking_lot::Mutex;
    use reqwest::{Request, Response, StatusCode};
    use reqwest_middleware::{Middleware, Next};
    use std::path::PathBuf;
    use std::sync::Arc;

    /// Adds the key to every request, reading it again once when the session refuses it
    #[derive(Clone)]
    pub struct KeyMiddleware {
        path: PathBuf,
        key: Arc<Mutex<Option<String>>>,
    }

    impl KeyMiddleware {
        /// With the key the session keeps beside the saved session
        pub fn new() -> Self {
            Self::from_file(key_path())
        }

        fn from_file(path: PathBuf) -> Self {
            Self {
                path,
                key: Arc::default(),
            }
        }

        fn read(&self) -> reqwest_middleware::Result<String> {
            let path = &self.path;
            let key = std::fs::read_to_string(path)
                .with_context(|| format!("read the relay's key from {}", path.display()))
                .map_err(reqwest_middleware::Error::Middleware)?
                .trim()
                .to_string();
            *self.key.lock() = Some(key.clone());
            Ok(key)
        }

        fn with_key(request: &mut Request, key: &str) -> reqwest_middleware::Result<()> {
            let value = key
                .parse()
                .context("the relay's key")
                .map_err(reqwest_middleware::Error::Middleware)?;
            request.headers_mut().insert(KEY_HEADER, value);
            Ok(())
        }
    }

    #[async_trait::async_trait]
    impl Middleware for KeyMiddleware {
        async fn handle(
            &self,
            mut request: Request,
            extensions: &mut Extensions,
            next: Next<'_>,
        ) -> reqwest_middleware::Result<Response> {
            let known = self.key.lock().clone();
            let key = match known {
                Some(key) => key,
                None => self.read()?,
            };
            Self::with_key(&mut request, &key)?;
            let retry = request.try_clone();
            let response = next.clone().run(request, extensions).await?;
            if response.status() != StatusCode::UNAUTHORIZED {
                return Ok(response);
            }
            let Some(mut retry) = retry else {
                return Ok(response);
            };
            let fresh = self.read()?;
            if fresh == key {
                return Ok(response);
            }
            Self::with_key(&mut retry, &fresh)?;
            next.run(retry, extensions).await
        }
    }

    /// The relay's address on the session's host
    pub fn base_url() -> String {
        let server = crate::config::get_config().app_config.cast_server.clone();
        // `cast_server`'s port is Snapcast's
        let host = server
            .rsplit_once(':')
            .map_or(server.as_str(), |(host, _)| host);
        format!("http://{host}:{PORT}")
    }

    /// The Web API's address, on the relay
    pub fn api_base_url() -> String {
        format!("{}{API_PREFIX}", base_url())
    }

    /// The session's own requests: lyrics, radios and covers
    #[derive(Clone)]
    pub struct Relay {
        http: reqwest_middleware::ClientWithMiddleware,
        base: String,
    }

    impl Relay {
        pub fn new() -> Self {
            Self {
                http: reqwest_middleware::ClientBuilder::new(reqwest::Client::new())
                    .with(KeyMiddleware::new())
                    .build(),
                base: base_url(),
            }
        }

        async fn get(&self, path: &str) -> anyhow::Result<Vec<u8>> {
            let response = self.http.get(format!("{}{path}", self.base)).send().await?;
            let status = response.status();
            let body = response.bytes().await?;
            anyhow::ensure!(
                status.is_success(),
                "the relay answered {status}: {}",
                String::from_utf8_lossy(&body)
            );
            Ok(body.to_vec())
        }

        pub async fn lyrics(&self, id: &str) -> anyhow::Result<Option<Vec<(i64, String)>>> {
            Ok(serde_json::from_slice(
                &self.get(&format!("/spt/lyrics/{id}")).await?,
            )?)
        }

        pub async fn radio(&self, seed_uri: &str) -> anyhow::Result<Vec<String>> {
            Ok(serde_json::from_slice(
                &self.get(&format!("/spt/radio/{seed_uri}")).await?,
            )?)
        }

        pub async fn image(&self, url: &str) -> anyhow::Result<Vec<u8>> {
            let query: String = reqwest::Url::parse_with_params("http://relay/", [("url", url)])?
                .query()
                .unwrap_or_default()
                .to_string();
            self.get(&format!("/spt/image?{query}")).await
        }
    }

    #[cfg(test)]
    mod tests {
        use super::KeyMiddleware;
        use wiremock::{
            matchers::{header, method},
            Mock, MockServer, ResponseTemplate,
        };

        #[tokio::test]
        async fn reads_the_key_again_once_the_session_refuses_it() {
            let server = MockServer::start().await;
            Mock::given(method("GET"))
                .and(header("x-spt-key", "new"))
                .respond_with(ResponseTemplate::new(200))
                .mount(&server)
                .await;
            Mock::given(method("GET"))
                .respond_with(ResponseTemplate::new(401))
                .mount(&server)
                .await;
            let dir = std::env::temp_dir().join(format!("spt-relay-pc-{}", std::process::id()));
            std::fs::create_dir_all(&dir).unwrap();
            let path = dir.join("relay.key");
            std::fs::write(&path, "old\n").unwrap();
            let client = reqwest_middleware::ClientBuilder::new(reqwest::Client::new())
                .with(KeyMiddleware::from_file(path.clone()))
                .build();

            let refused = client.get(server.uri()).send().await.unwrap();
            assert_eq!(refused.status(), 401);

            // the session made a new key
            std::fs::write(&path, "new").unwrap();
            let sent = client.get(server.uri()).send().await.unwrap();
            assert_eq!(sent.status(), 200);
            std::fs::remove_dir_all(&dir).unwrap();
        }
    }
}
