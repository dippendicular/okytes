//! GitHub and Google sign-in: authorization code flow with PKCE and a one-time
//! state. Only provider-verified email addresses are accepted, because accounts
//! are linked by email.
use anyhow::{bail, Context, Result};
use base64::Engine as _;
use serde::Deserialize;
use sha2::{Digest, Sha256};

#[derive(Clone)]
pub struct Provider {
    pub client_id: String,
    pub client_secret: String,
}

pub struct Identity {
    pub subject: String,
    pub email: String,
}

pub fn challenge(verifier: &str) -> String {
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()))
}

pub fn authorize_url(
    name: &str,
    provider: &Provider,
    redirect: &str,
    state: &str,
    verifier: &str,
) -> String {
    let (base, scope) = match name {
        "github" => (
            "https://github.com/login/oauth/authorize",
            "read:user user:email",
        ),
        _ => (
            "https://accounts.google.com/o/oauth2/v2/auth",
            "openid email",
        ),
    };
    let mut url = url::Url::parse(base).expect("static URL");
    url.query_pairs_mut()
        .append_pair("client_id", &provider.client_id)
        .append_pair("redirect_uri", redirect)
        .append_pair("response_type", "code")
        .append_pair("scope", scope)
        .append_pair("state", state)
        .append_pair("code_challenge", &challenge(verifier))
        .append_pair("code_challenge_method", "S256");
    url.into()
}

#[derive(Deserialize)]
struct Token {
    access_token: Option<String>,
    error: Option<String>,
}

fn client() -> Result<reqwest::Client> {
    Ok(reqwest::Client::builder()
        .user_agent("okytes-cloud")
        .timeout(std::time::Duration::from_secs(15))
        .build()?)
}

async fn exchange(
    http: &reqwest::Client,
    endpoint: &str,
    provider: &Provider,
    code: &str,
    redirect: &str,
    verifier: &str,
) -> Result<String> {
    let body = url::form_urlencoded::Serializer::new(String::new())
        .append_pair("client_id", &provider.client_id)
        .append_pair("client_secret", &provider.client_secret)
        .append_pair("code", code)
        .append_pair("redirect_uri", redirect)
        .append_pair("code_verifier", verifier)
        .append_pair("grant_type", "authorization_code")
        .finish();
    let token: Token = http
        .post(endpoint)
        .header("accept", "application/json")
        .header("content-type", "application/x-www-form-urlencoded")
        .body(body)
        .send()
        .await?
        .json()
        .await
        .context("token response")?;
    match (token.access_token, token.error) {
        (Some(token), _) => Ok(token),
        (_, Some(error)) => bail!("provider refused the sign-in: {error}"),
        _ => bail!("provider returned no access token"),
    }
}

pub async fn identity(
    name: &str,
    provider: &Provider,
    code: &str,
    redirect: &str,
    verifier: &str,
) -> Result<Identity> {
    let http = client()?;
    if name == "github" {
        let token = exchange(
            &http,
            "https://github.com/login/oauth/access_token",
            provider,
            code,
            redirect,
            verifier,
        )
        .await?;
        #[derive(Deserialize)]
        struct GitHubUser {
            id: u64,
        }
        #[derive(Deserialize)]
        struct Email {
            email: String,
            primary: bool,
            verified: bool,
        }
        let user: GitHubUser = http
            .get("https://api.github.com/user")
            .bearer_auth(&token)
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        let emails: Vec<Email> = http
            .get("https://api.github.com/user/emails")
            .bearer_auth(&token)
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        let email = emails
            .into_iter()
            .find(|e| e.primary && e.verified)
            .context("your GitHub account has no verified primary email")?;
        Ok(Identity {
            subject: user.id.to_string(),
            email: email.email.to_lowercase(),
        })
    } else {
        let token = exchange(
            &http,
            "https://oauth2.googleapis.com/token",
            provider,
            code,
            redirect,
            verifier,
        )
        .await?;
        #[derive(Deserialize)]
        struct Info {
            sub: String,
            email: Option<String>,
            email_verified: Option<bool>,
        }
        let info: Info = http
            .get("https://openidconnect.googleapis.com/v1/userinfo")
            .bearer_auth(&token)
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        match (info.email, info.email_verified) {
            (Some(email), Some(true)) => Ok(Identity {
                subject: info.sub,
                email: email.to_lowercase(),
            }),
            _ => bail!("your Google account email is not verified"),
        }
    }
}
