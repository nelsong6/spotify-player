//! auth.romaine.life, for muting another PC: a PC signs in there and keeps its token fresh, and
//! the session accepts only a token auth issued to spt for an admin.

/// The audience auth signs access tokens for. Asked for no audience, it issues an opaque token
/// the session couldn't check.
const RESOURCE: &str = "https://auth.romaine.life/api/auth";
const CLIENT_ID: &str = "spt";

/// A PC's side: an access token for the session, signing in through the browser when there's no
/// refresh token or it has expired
#[cfg(feature = "cast")]
pub mod pc {
    use super::{CLIENT_ID, RESOURCE};
    use crate::{auth, config};
    use anyhow::Context as _;
    use parking_lot::Mutex;
    use serde::{Deserialize, Serialize};
    use std::net::TcpListener;

    const AUTHORIZE_URL: &str = "https://auth.romaine.life/api/auth/oauth2/authorize";
    const TOKEN_URL: &str = "https://auth.romaine.life/api/auth/oauth2/token";
    const TOKEN_FILE: &str = "home_auth.json";
    /// An access token this close to expiring is renewed first
    const MARGIN: chrono::TimeDelta = chrono::TimeDelta::minutes(1);

    #[derive(Serialize, Deserialize)]
    struct Tokens {
        access_token: String,
        refresh_token: String,
        expires_at: chrono::DateTime<chrono::Utc>,
    }

    #[derive(Deserialize)]
    struct TokenResponse {
        access_token: String,
        refresh_token: String,
        expires_in: i64,
    }

    /// Auth rotates the refresh token on each use and revokes every token of a sign-in when a used
    /// one comes back, so two renewals must never race.
    static RENEWING: Mutex<()> = Mutex::new(());

    /// A current access token. Blocks, for as long as a browser sign-in takes when one is needed.
    pub fn access_token() -> anyhow::Result<String> {
        let _renewing = RENEWING.lock();
        let path = config::get_config().cache_folder.join(TOKEN_FILE);
        let saved = std::fs::read_to_string(&path)
            .ok()
            .and_then(|saved| serde_json::from_str::<Tokens>(&saved).ok());
        if let Some(saved) = &saved {
            if saved.expires_at - MARGIN > chrono::Utc::now() {
                return Ok(saved.access_token.clone());
            }
        }
        let renewed = saved.and_then(|saved| {
            request(&[
                ("grant_type", "refresh_token"),
                ("refresh_token", &saved.refresh_token),
                ("client_id", CLIENT_ID),
                ("resource", RESOURCE),
            ])
            .inspect_err(|err| {
                tracing::warn!("Failed to renew the auth.romaine.life token: {err:#}");
            })
            .ok()
        });
        let tokens = match renewed {
            Some(tokens) => tokens,
            None => sign_in()?,
        };
        std::fs::write(&path, serde_json::to_string(&tokens)?)
            .with_context(|| format!("save {}", path.display()))?;
        Ok(tokens.access_token)
    }

    fn sign_in() -> anyhow::Result<Tokens> {
        // auth accepts the loopback callback on any port
        let listener = TcpListener::bind("127.0.0.1:0").context("bind the sign-in callback")?;
        let redirect_uri = format!("http://{}/callback", listener.local_addr()?);
        let pkce = auth::Pkce::new_random();
        let url = reqwest::Url::parse_with_params(
            AUTHORIZE_URL,
            &[
                ("response_type", "code"),
                ("client_id", CLIENT_ID),
                ("redirect_uri", redirect_uri.as_str()),
                ("scope", "openid offline_access"),
                ("code_challenge_method", "S256"),
                ("code_challenge", pkce.challenge.as_str()),
                ("state", auth::random_url_safe(16).as_str()),
            ],
        )?;
        tracing::info!("Signing in to auth.romaine.life: {url}");
        open::that_in_background(url.as_str());
        let code = auth::wait_for_auth_code(&listener)?;
        request(&[
            ("grant_type", "authorization_code"),
            ("code", &code),
            ("redirect_uri", &redirect_uri),
            ("client_id", CLIENT_ID),
            ("code_verifier", &pkce.verifier),
            ("resource", RESOURCE),
        ])
    }

    fn request(form: &[(&str, &str)]) -> anyhow::Result<Tokens> {
        let response: TokenResponse = reqwest::blocking::Client::new()
            .post(TOKEN_URL)
            .form(form)
            .send()
            .context("send the token request")?
            .error_for_status()
            .context("the token request failed")?
            .json()
            .context("parse the token response")?;
        Ok(Tokens {
            access_token: response.access_token,
            refresh_token: response.refresh_token,
            expires_at: chrono::Utc::now() + chrono::TimeDelta::seconds(response.expires_in),
        })
    }
}

/// The session's side: whether a PC's token lets it mute PCs
#[cfg(feature = "streaming")]
pub mod session {
    use super::{CLIENT_ID, RESOURCE};
    use anyhow::Context as _;
    use jsonwebtoken::jwk::JwkSet;
    use serde::Deserialize;

    const ISSUER: &str = "https://auth.romaine.life";
    const JWKS_URL: &str = "https://auth.romaine.life/api/auth/jwks";

    #[derive(Deserialize)]
    struct Claims {
        azp: String,
        role: String,
    }

    /// Auth's signing keys, fetched again when a token names one not among them
    static KEYS: tokio::sync::Mutex<Option<JwkSet>> = tokio::sync::Mutex::const_new(None);

    /// Accept a token signed by auth, for this audience, issued to spt, for an admin
    pub async fn check(token: &str) -> anyhow::Result<()> {
        let kid = jsonwebtoken::decode_header(token)?
            .kid
            .context("the token names no key")?;
        let mut keys = KEYS.lock().await;
        if keys.as_ref().is_none_or(|keys| keys.find(&kid).is_none()) {
            *keys = Some(
                reqwest::get(JWKS_URL)
                    .await?
                    .error_for_status()?
                    .json()
                    .await
                    .context("fetch auth's signing keys")?,
            );
        }
        let key = keys
            .as_ref()
            .and_then(|keys| keys.find(&kid))
            .context("auth has no such signing key")?;
        check_with(token, &jsonwebtoken::DecodingKey::from_jwk(key)?)
    }

    fn check_with(token: &str, key: &jsonwebtoken::DecodingKey) -> anyhow::Result<()> {
        let mut validation = jsonwebtoken::Validation::new(jsonwebtoken::Algorithm::RS256);
        validation.set_issuer(&[ISSUER]);
        validation.set_audience(&[RESOURCE]);
        let claims = jsonwebtoken::decode::<Claims>(token, key, &validation)?.claims;
        anyhow::ensure!(
            claims.azp == CLIENT_ID,
            "the token was issued to {}",
            claims.azp
        );
        anyhow::ensure!(claims.role == "admin", "the token's user is not an admin");
        Ok(())
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use rsa::pkcs1::{EncodeRsaPrivateKey as _, EncodeRsaPublicKey as _};

        #[test]
        fn accepts_only_auths_admin_tokens_for_spt() {
            let private = rsa::RsaPrivateKey::new(&mut rsa::rand_core::OsRng, 2048).unwrap();
            let signing =
                jsonwebtoken::EncodingKey::from_rsa_der(private.to_pkcs1_der().unwrap().as_bytes());
            let key = jsonwebtoken::DecodingKey::from_rsa_der(
                private.to_public_key().to_pkcs1_der().unwrap().as_bytes(),
            );
            let exp = chrono::Utc::now().timestamp() + 600;
            let token = |claims: serde_json::Value| {
                let mut all = serde_json::json!({
                    "iss": ISSUER,
                    "aud": [RESOURCE, format!("{RESOURCE}/oauth2/userinfo")],
                    "azp": CLIENT_ID,
                    "role": "admin",
                    "exp": exp,
                });
                all.as_object_mut()
                    .unwrap()
                    .extend(claims.as_object().unwrap().clone());
                jsonwebtoken::encode(
                    &jsonwebtoken::Header::new(jsonwebtoken::Algorithm::RS256),
                    &all,
                    &signing,
                )
                .unwrap()
            };

            assert!(check_with(&token(serde_json::json!({})), &key).is_ok());
            for refused in [
                serde_json::json!({"role": "user"}),
                serde_json::json!({"azp": "grafana"}),
                serde_json::json!({"aud": "https://auth.romaine.life"}),
                serde_json::json!({"iss": "https://elsewhere.example"}),
                serde_json::json!({"exp": exp - 1200}),
            ] {
                assert!(
                    check_with(&token(refused.clone()), &key).is_err(),
                    "accepted {refused}"
                );
            }
        }
    }
}
