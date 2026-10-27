use super::{key_path, API_PREFIX, KEY_HEADER, PORT};
use anyhow::Context as _;
use http::Extensions;
use parking_lot::Mutex;
use reqwest::{Method, Request, Response, StatusCode};
use reqwest_middleware::{Middleware, Next};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

/// How long a request may take: the session holds one while Spotify has asked it to wait,
/// which outlasts rspotify's own 10 s
const TIMEOUT: Duration = Duration::from_mins(3);
/// How long a PC keeps trying to reach the session, and how often
const RECONNECT: Duration = Duration::from_secs(30);
const RECONNECT_INTERVAL: Duration = Duration::from_secs(1);

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
        *request.timeout_mut() = Some(TIMEOUT);
        Ok(())
    }
}

/// Send a request, again while the session is out of reach, as when a new release takes it
/// over. A request the session dropped mid-way goes again only if it changes nothing, since
/// the session may have passed it on already.
async fn send(
    mut request: Request,
    extensions: &mut Extensions,
    next: &Next<'_>,
) -> reqwest_middleware::Result<Response> {
    let deadline = tokio::time::Instant::now() + RECONNECT;
    loop {
        let again = request.try_clone();
        let error = match next.clone().run(request, extensions).await {
            Err(reqwest_middleware::Error::Reqwest(error)) => error,
            result => return result,
        };
        let unsent = error.is_connect();
        let dropped =
            error.is_request() && again.as_ref().is_some_and(|r| r.method() == Method::GET);
        match again {
            Some(again) if (unsent || dropped) && tokio::time::Instant::now() < deadline => {
                tracing::info!("The session is out of reach; trying again: {error:#}");
                tokio::time::sleep(RECONNECT_INTERVAL).await;
                request = again;
            }
            _ => return Err(error.into()),
        }
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
        let response = send(request, extensions, &next).await?;
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
        send(retry, extensions, &next).await
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

    /// Have the session resume the radio where it left off
    pub async fn resume(&self) -> anyhow::Result<()> {
        let response = self
            .http
            .post(format!("{}/spt/resume", self.base))
            .send()
            .await?;
        let status = response.status();
        anyhow::ensure!(
            status.is_success(),
            "the relay answered {status}: {}",
            response.text().await.unwrap_or_default()
        );
        Ok(())
    }

    /// Have the session mute or unmute a PC, by its cast id
    pub async fn mute(&self, pc: &str, muted: bool) -> anyhow::Result<()> {
        let response = self
            .http
            .post(format!("{}/spt/mute", self.base))
            .json(&serde_json::json!({ "pc": pc, "muted": muted }))
            .send()
            .await?;
        let status = response.status();
        anyhow::ensure!(
            status.is_success(),
            "the relay answered {status}: {}",
            response.text().await.unwrap_or_default()
        );
        Ok(())
    }

    /// Have the session load the library from Spotify again
    pub async fn refresh(&self) -> anyhow::Result<()> {
        let response = self
            .http
            .post(format!("{}/spt/refresh", self.base))
            .send()
            .await?;
        let status = response.status();
        anyhow::ensure!(status.is_success(), "the relay answered {status}");
        Ok(())
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

    #[tokio::test]
    async fn waits_for_a_new_release_to_take_the_session_over() {
        // a port the session isn't listening on yet
        let addr = std::net::TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap();
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(1500)).await;
            let listener = tokio::net::TcpListener::bind(addr).await.unwrap();
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut buffer = [0u8; 4096];
            let _ = tokio::io::AsyncReadExt::read(&mut stream, &mut buffer).await;
            tokio::io::AsyncWriteExt::write_all(
                &mut stream,
                b"HTTP/1.1 200 OK\r\ncontent-length: 0\r\nconnection: close\r\n\r\n",
            )
            .await
            .unwrap();
        });
        let dir = std::env::temp_dir().join(format!("spt-relay-again-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("relay.key");
        std::fs::write(&path, "key").unwrap();
        let client = reqwest_middleware::ClientBuilder::new(reqwest::Client::new())
            .with(KeyMiddleware::from_file(path))
            .build();

        let response = client.get(format!("http://{addr}/")).send().await.unwrap();
        assert_eq!(response.status(), 200);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[tokio::test]
    async fn outwaits_the_clients_own_timeout() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(200).set_delay(std::time::Duration::from_secs(2)))
            .mount(&server)
            .await;
        let dir = std::env::temp_dir().join(format!("spt-relay-wait-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("relay.key");
        std::fs::write(&path, "key").unwrap();
        let short = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(1))
            .build()
            .unwrap();
        let client = reqwest_middleware::ClientBuilder::new(short)
            .with(KeyMiddleware::from_file(path))
            .build();

        let response = client.get(server.uri()).send().await.unwrap();
        assert_eq!(response.status(), 200);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
