//! Wallet-facing mint authentication. Secrets never enter an API response.
use std::{
    collections::BTreeMap,
    path::PathBuf,
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use anyhow::{bail, ensure, Context, Result};
use bitcoin_hashes::{sha256, Hash};
use cdk_common::bitcoin::base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use cdk_common::{
    amount::SplitTarget,
    dhke,
    nuts::{nut21, nut22, BlindAuthToken, KeysResponse, MintResponse, PreMintSecrets},
};
use reqwest::{header::HeaderValue, Client, RequestBuilder};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::{io::AsyncWriteExt, sync::Mutex};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    #[default]
    None,
    Clear,
    Blind,
}

pub const OPERATIONS: [(&str, &str, &str); 10] = [
    ("get_mint_quote", "POST", "/v1/mint/quote/"),
    ("check_mint_quote", "GET", "/v1/mint/quote/"),
    ("mint", "POST", "/v1/mint/"),
    ("get_melt_quote", "POST", "/v1/melt/quote/"),
    ("check_melt_quote", "GET", "/v1/melt/quote/"),
    ("melt", "POST", "/v1/melt/"),
    ("swap", "POST", "/v1/swap"),
    ("restore", "POST", "/v1/restore"),
    ("check_proof_state", "POST", "/v1/checkstate"),
    ("websocket_auth", "GET", "/v1/ws"),
];

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AuthDraft {
    pub auth_enabled: bool,
    pub openid_discovery: String,
    pub openid_client_id: String,
    pub mint_max_bat: u64,
    pub operations: BTreeMap<String, Mode>,
}

impl AuthDraft {
    pub fn validate(&self) -> Result<()> {
        ensure!(
            self.operations.len() == OPERATIONS.len()
                && OPERATIONS
                    .iter()
                    .all(|(name, _, _)| self.operations.contains_key(*name)),
            "provide all ten auth operations"
        );
        ensure!(
            self.mint_max_bat <= 10_000 && (!self.auth_enabled || self.mint_max_bat > 0),
            "blind-token batch limit must be between 1 and 10000"
        );
        ensure!(
            self.openid_client_id.len() <= 200
                && !self.openid_client_id.chars().any(char::is_control),
            "invalid OpenID client ID"
        );
        if self.auth_enabled {
            secure_url(&self.openid_discovery)?;
            ensure!(
                !self.openid_client_id.trim().is_empty(),
                "OpenID client ID is required"
            );
        } else if !self.openid_discovery.is_empty() {
            secure_url(&self.openid_discovery)?;
        }
        Ok(())
    }

    pub fn snippet(&self) -> String {
        // JSON strings are valid TOML basic strings for these validated values.
        let mut out = format!("[auth]\nauth_enabled = {}\nopenid_discovery = {}\nopenid_client_id = {}\nmint_max_bat = {}\n", self.auth_enabled, json!(self.openid_discovery), json!(self.openid_client_id), self.mint_max_bat);
        for (name, _, _) in OPERATIONS {
            out.push_str(&format!("{name} = {}\n", json!(self.operations[name])));
        }
        out
    }
}

#[derive(Clone, Default, Serialize)]
pub struct Policy {
    pub clear: Option<nut21::Settings>,
    pub blind: Option<nut22::Settings>,
}

impl Policy {
    pub fn from_info(info: &Value) -> Result<Self> {
        let clear = info
            .pointer("/nuts/21")
            .filter(|v| !v.is_null())
            .map(|v| serde_json::from_value(v.clone()))
            .transpose()?;
        let blind = info
            .pointer("/nuts/22")
            .filter(|v| !v.is_null())
            .map(|v| serde_json::from_value(v.clone()))
            .transpose()?;
        Ok(Self { clear, blind })
    }

    pub fn mode(&self, method: &str, path: &str) -> Result<Mode> {
        let endpoint =
            nut21::ProtectedEndpoint::new(serde_json::from_value(json!(method))?, path.parse()?);
        let score = |items: &[nut21::ProtectedEndpoint]| {
            items
                .iter()
                .filter_map(|p| p.match_specificity(&endpoint))
                .max()
        };
        let clear = self
            .clear
            .as_ref()
            .and_then(|s| score(&s.protected_endpoints));
        let blind = self
            .blind
            .as_ref()
            .and_then(|s| score(&s.protected_endpoints));
        match (clear, blind) {
            (Some(a), Some(b)) if a == b => {
                bail!("mint advertises conflicting authentication policies")
            }
            (Some(a), Some(b)) if a > b => Ok(Mode::Clear),
            (_, Some(_)) => Ok(Mode::Blind),
            (Some(_), _) => Ok(Mode::Clear),
            _ => Ok(Mode::None),
        }
    }

    pub fn operations(&self, payment_method: &str) -> Result<BTreeMap<String, Mode>> {
        OPERATIONS
            .iter()
            .map(|(name, method, path)| {
                let path = if path.ends_with('/') {
                    format!("{path}{payment_method}")
                } else {
                    path.to_string()
                };
                Ok((name.to_string(), self.mode(method, &path)?))
            })
            .collect()
    }

    fn binding(&self, mint: &str) -> Result<Binding> {
        let clear = self
            .clear
            .as_ref()
            .context("mint does not advertise clear authentication (NUT-21)")?;
        secure_url(mint)?;
        secure_url(&clear.openid_discovery)?;
        ensure!(
            !clear.client_id.is_empty() && clear.client_id.len() <= 200,
            "invalid mint OpenID client ID"
        );
        Ok(Binding {
            mint: mint.trim_end_matches('/').to_string(),
            discovery: clear.openid_discovery.clone(),
            client_id: clear.client_id.clone(),
        })
    }
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
struct Binding {
    mint: String,
    discovery: String,
    client_id: String,
}

#[derive(Clone, Serialize, Deserialize)]
struct Credentials {
    binding: Binding,
    token_endpoint: String,
    access_token: String,
    refresh_token: Option<String>,
    expires_at: u64,
    #[serde(default)]
    bats: Vec<String>,
}

#[derive(Default)]
struct AuthState {
    credentials: Option<Credentials>,
    pending: Option<Pending>,
}
struct Pending {
    session: String,
    binding: Binding,
    device_code: String,
    code_verifier: String,
    token_endpoint: String,
    view: Login,
    next_poll: u64,
}

#[derive(Clone, Serialize)]
pub struct Login {
    pub id: String,
    pub user_code: String,
    pub verification_uri: String,
    pub expires_at: u64,
    pub interval: u64,
}

#[derive(Serialize)]
pub struct Connection {
    pub stored: bool,
    pub connected: bool,
    pub expires_at: Option<u64>,
    pub blind_tokens: usize,
    pub can_refresh: bool,
}

#[derive(Clone)]
pub struct MintAuth {
    state: Arc<Mutex<AuthState>>,
    path: Arc<PathBuf>,
    http: Client,
}

impl MintAuth {
    pub async fn load(path: PathBuf) -> Result<Self> {
        let credentials = match tokio::fs::read(&path).await {
            Ok(bytes) => serde_json::from_slice(&bytes).context("read mint credentials")?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
            Err(e) => return Err(e.into()),
        };
        Ok(Self {
            state: Arc::new(Mutex::new(AuthState {
                credentials,
                pending: None,
            })),
            path: Arc::new(path),
            http: Client::builder()
                .timeout(Duration::from_secs(10))
                .redirect(reqwest::redirect::Policy::none())
                .build()?,
        })
    }

    async fn persist(&self, credentials: &Option<Credentials>) -> Result<()> {
        let tmp = self
            .path
            .with_extension(format!("{}.tmp", uuid::Uuid::new_v4()));
        let mut opts = tokio::fs::OpenOptions::new();
        opts.write(true).create_new(true);
        #[cfg(unix)]
        opts.mode(0o600);
        let result: Result<()> = async {
            let mut file = opts.open(&tmp).await?;
            file.write_all(&serde_json::to_vec(credentials)?).await?;
            file.sync_all().await?;
            tokio::fs::rename(&tmp, self.path.as_ref()).await?;
            #[cfg(unix)]
            tokio::fs::File::open(
                self.path
                    .parent()
                    .context("credentials directory missing")?,
            )
            .await?
            .sync_all()
            .await?;
            Ok(())
        }
        .await;
        if result.is_err() {
            let _ = tokio::fs::remove_file(tmp).await;
        }
        result.context("persist mint credentials")
    }

    pub async fn connection(&self, mint: &str, policy: &Policy) -> Connection {
        let state = self.state.lock().await;
        let credentials = policy
            .binding(mint)
            .ok()
            .and_then(|b| state.credentials.as_ref().filter(|c| c.binding == b));
        Connection {
            stored: state.credentials.is_some() || state.pending.is_some(),
            connected: credentials.is_some_and(|c| {
                c.expires_at > now() || c.refresh_token.is_some() || !c.bats.is_empty()
            }),
            expires_at: credentials.map(|c| c.expires_at),
            blind_tokens: credentials.map_or(0, |c| c.bats.len()),
            can_refresh: credentials.is_some_and(|c| c.refresh_token.is_some()),
        }
    }

    pub async fn disconnect(&self) -> Result<()> {
        let mut state = self.state.lock().await;
        self.persist(&None).await?;
        state.credentials = None;
        state.pending = None;
        Ok(())
    }

    pub async fn start(&self, mint: &str, policy: &Policy, session: String) -> Result<Login> {
        let binding = policy.binding(mint)?;
        let mut state = self.state.lock().await;
        let doc = successful(self.http.get(&binding.discovery)).await?;
        let token_endpoint = string(&doc, "token_endpoint")?;
        let device_endpoint = string(&doc, "device_authorization_endpoint")
            .context("provider must support device authorization for processor sign-in")?;
        secure_url(&token_endpoint)?;
        secure_url(&device_endpoint)?;
        // Keycloak enforces the public client's PKCE policy on device login too.
        let code_verifier = cdk_common::secret::Secret::generate().to_string();
        let code_challenge = pkce_challenge(&code_verifier);
        let device = successful(self.http.post(device_endpoint).form(&[
            ("client_id", binding.client_id.as_str()),
            ("scope", "openid offline_access"),
            ("code_challenge_method", "S256"),
            ("code_challenge", code_challenge.as_str()),
        ]))
        .await?;
        let verification_uri = string(&device, "verification_uri")?;
        secure_url(&verification_uri)?;
        let lifetime = device["expires_in"]
            .as_u64()
            .filter(|v| (1..=3600).contains(v))
            .context("invalid device login expiry")?;
        let view = Login {
            id: uuid::Uuid::new_v4().to_string(),
            user_code: string(&device, "user_code")?,
            verification_uri,
            expires_at: now() + lifetime,
            interval: device["interval"].as_u64().unwrap_or(5).clamp(1, 60),
        };
        state.pending = Some(Pending {
            session,
            binding,
            device_code: string(&device, "device_code")?,
            code_verifier,
            token_endpoint,
            next_poll: now() + view.interval,
            view: view.clone(),
        });
        Ok(view)
    }

    pub async fn poll(
        &self,
        mint: &str,
        policy: &Policy,
        session: &str,
        id: &str,
    ) -> Result<Value> {
        let binding = policy.binding(mint)?;
        let mut state = self.state.lock().await;
        let pending = state
            .pending
            .as_mut()
            .context("sign-in ended; start again")?;
        ensure!(
            pending.session == session && pending.view.id == id && pending.binding == binding,
            "sign-in does not belong to this session and mint"
        );
        if now() >= pending.view.expires_at {
            state.pending = None;
            bail!("sign-in expired; start again");
        }
        if now() < pending.next_poll {
            return Ok(json!({"status":"pending", "interval":pending.view.interval}));
        }
        pending.next_poll = now() + pending.view.interval;
        let (status, body) = response(self.http.post(&pending.token_endpoint).form(&[
            ("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
            ("device_code", &pending.device_code),
            ("client_id", &binding.client_id),
            ("code_verifier", &pending.code_verifier),
        ]))
        .await?;
        if status.is_success() {
            let credentials =
                credentials_from(body, binding, pending.token_endpoint.clone(), None)?;
            self.persist(&Some(credentials.clone())).await?;
            state.credentials = Some(credentials);
            state.pending = None;
            return Ok(json!({"status":"connected"}));
        }
        match body["error"].as_str() {
            Some("authorization_pending") => {
                Ok(json!({"status":"pending", "interval":pending.view.interval}))
            }
            Some("slow_down") => {
                pending.view.interval += 5;
                pending.next_poll = now() + pending.view.interval;
                Ok(json!({"status":"pending", "interval":pending.view.interval}))
            }
            _ => {
                state.pending = None;
                bail!("provider denied or expired the sign-in; start again")
            }
        }
    }

    async fn clear_token(&self, credentials: &mut Credentials) -> Result<String> {
        if credentials.expires_at <= now() + 30 {
            let refresh = credentials
                .refresh_token
                .as_ref()
                .context("processor sign-in expired; reconnect in Access")?;
            let body = successful(self.http.post(&credentials.token_endpoint).form(&[
                ("grant_type", "refresh_token"),
                ("refresh_token", refresh),
                ("client_id", &credentials.binding.client_id),
            ]))
            .await
            .context("processor session could not be refreshed; reconnect in Access")?;
            let mut updated = credentials_from(
                body,
                credentials.binding.clone(),
                credentials.token_endpoint.clone(),
                Some(credentials),
            )?;
            updated.bats = credentials.bats.clone();
            // Retain a rotated refresh token in memory even if the disk write fails.
            *credentials = updated;
            self.persist(&Some(credentials.clone())).await?;
        }
        Ok(credentials.access_token.clone())
    }

    pub async fn header(
        &self,
        mint: &str,
        policy: &Policy,
        method: &str,
        path: &str,
    ) -> Result<Option<(&'static str, HeaderValue)>> {
        let mode = policy.mode(method, path)?;
        if mode == Mode::None {
            return Ok(None);
        }
        let binding = policy.binding(mint)?;
        let mut state = self.state.lock().await;
        let credentials = state
            .credentials
            .as_mut()
            .filter(|c| c.binding == binding)
            .context("mint requires authentication; connect the processor in Access")?;
        let (name, token) = if mode == Mode::Clear {
            ("Clear-auth", self.clear_token(credentials).await?)
        } else {
            if credentials.bats.is_empty() {
                let count = policy
                    .blind
                    .as_ref()
                    .context("missing blind auth settings")?
                    .bat_max_mint
                    .min(32);
                ensure!(count > 0, "mint's blind-token limit is zero");
                let cat = self.clear_token(credentials).await?;
                let keys: KeysResponse = serde_json::from_value(
                    successful(
                        self.http
                            .get(format!("{}/v1/auth/blind/keys", binding.mint)),
                    )
                    .await?,
                )?;
                let keyset = keys
                    .keysets
                    .iter()
                    .find(|k| k.unit.to_string() == "auth")
                    .context("mint has no active auth keys")?;
                let key = keyset
                    .keys
                    .amount_key(1.into())
                    .context("missing denomination-one auth key")?;
                let secrets = PreMintSecrets::random(
                    keyset.id,
                    count.into(),
                    &SplitTarget::Value(1.into()),
                    &(0, vec![1]).into(),
                )?;
                let signatures: MintResponse = serde_json::from_value(
                    successful(
                        self.http
                            .post(format!("{}/v1/auth/blind/mint", binding.mint))
                            .header("Clear-auth", sensitive(&cat)?)
                            .json(&json!({"outputs":secrets.blinded_messages()})),
                    )
                    .await?,
                )?;
                ensure!(
                    signatures.signatures.len() == secrets.secrets.len(),
                    "mint returned an incorrect blind signature count"
                );
                for (sig, secret) in signatures.signatures.iter().zip(&secrets.secrets) {
                    ensure!(
                        sig.amount == 1.into() && sig.keyset_id == keyset.id,
                        "mint returned an invalid auth signature"
                    );
                    sig.verify_dleq(key, secret.blinded_message.blinded_secret)
                        .context("invalid auth DLEQ proof")?;
                }
                credentials.bats = dhke::construct_proofs(
                    signatures.signatures,
                    secrets.rs(),
                    secrets.secrets(),
                    &keyset.keys,
                )?
                .into_iter()
                .map(|p| {
                    Ok(BlindAuthToken::new(p.try_into()?)
                        .without_dleq()
                        .to_string())
                })
                .collect::<Result<_>>()?;
            }
            // Burn before dispatch, including on HTTP errors, timeouts and crashes.
            let token = credentials.bats.pop().context("empty auth token pool")?;
            self.persist(&Some(credentials.clone())).await?;
            ("Blind-auth", token)
        };
        Ok(Some((name, sensitive(&token)?)))
    }
}

fn sensitive(token: &str) -> Result<HeaderValue> {
    let mut header = HeaderValue::from_str(token).context("invalid authentication header")?;
    header.set_sensitive(true);
    Ok(header)
}

fn credentials_from(
    body: Value,
    binding: Binding,
    token_endpoint: String,
    previous: Option<&Credentials>,
) -> Result<Credentials> {
    let access_token = string(&body, "access_token")?;
    sensitive(&access_token)?;
    let lifetime = body["expires_in"]
        .as_u64()
        .filter(|n| (1..=31_536_000).contains(n))
        .context("provider must return a valid token expiry")?;
    let refresh_token = body["refresh_token"]
        .as_str()
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .or_else(|| previous.and_then(|c| c.refresh_token.clone()));
    Ok(Credentials {
        binding,
        token_endpoint,
        access_token,
        refresh_token,
        expires_at: now() + lifetime,
        bats: vec![],
    })
}

fn string(value: &Value, name: &str) -> Result<String> {
    value[name]
        .as_str()
        .filter(|s| !s.is_empty() && s.len() <= 16_384)
        .map(str::to_owned)
        .with_context(|| format!("provider response missing {name}"))
}

pub fn secure_url(value: &str) -> Result<()> {
    ensure!(
        value.len() <= 2048 && !value.chars().any(char::is_control),
        "invalid authentication URL"
    );
    let url = reqwest::Url::parse(value).context("invalid authentication URL")?;
    let local = url
        .host_str()
        .is_some_and(|h| h == "localhost" || h == "127.0.0.1" || h == "[::1]");
    ensure!(
        url.scheme() == "https" || (url.scheme() == "http" && local),
        "authentication requires HTTPS (HTTP is allowed only on loopback)"
    );
    ensure!(
        url.host_str().is_some()
            && url.username().is_empty()
            && url.password().is_none()
            && url.fragment().is_none(),
        "authentication URL must not contain credentials or a fragment"
    );
    Ok(())
}

async fn response(request: RequestBuilder) -> Result<(reqwest::StatusCode, Value)> {
    let mut response = request
        .send()
        .await
        .context("authentication service is unreachable")?;
    let status = response.status();
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await? {
        ensure!(
            bytes.len() + chunk.len() <= 1_048_576,
            "authentication response too large"
        );
        bytes.extend_from_slice(&chunk);
    }
    Ok((
        status,
        serde_json::from_slice(&bytes).context("invalid authentication response")?,
    ))
}

async fn successful(request: RequestBuilder) -> Result<Value> {
    let (status, body) = response(request).await?;
    // Provider errors may contain tokens; only report status to the console.
    ensure!(
        status.is_success(),
        "authentication service returned HTTP {}",
        status.as_u16()
    );
    Ok(body)
}

fn pkce_challenge(verifier: &str) -> String {
    URL_SAFE_NO_PAD.encode(sha256::Hash::hash(verifier.as_bytes()).to_byte_array())
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn device_pkce_matches_rfc7636_vector() {
        assert_eq!(
            pkce_challenge("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk"),
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
        );
    }

    #[test]
    fn policy_matches_custom_quotes_and_keeps_clear_off_blind_routes() {
        let policy = Policy::from_info(&json!({"nuts": {
            "21":{"openid_discovery":"https://id.example/discovery", "client_id":"cashu", "protected_endpoints":[{"method":"POST","path":"/v1/auth/blind/mint"}]},
            "22":{"bat_max_mint":32, "protected_endpoints":[{"method":"GET","path":"/v1/mint/quote/branch"}, {"method":"POST","path":"/v1/melt/*"}]}
        }})).unwrap();
        assert_eq!(
            policy.mode("GET", "/v1/mint/quote/branch").unwrap(),
            Mode::Blind
        );
        assert_eq!(
            policy.mode("POST", "/v1/mint/quote/branch").unwrap(),
            Mode::None
        );
        assert_eq!(policy.mode("POST", "/v1/melt/branch").unwrap(), Mode::Blind);
        assert_eq!(
            policy.mode("POST", "/v1/auth/blind/mint").unwrap(),
            Mode::Clear
        );
        assert!(policy.binding("http://mint.example").is_err());
        assert!(policy.binding("https://mint.example").is_ok());
    }

    #[test]
    fn rejects_unsafe_urls_and_incomplete_drafts() {
        for url in [
            "javascript:alert(1)",
            "http://evil.example",
            "https://u:p@example.com",
            "https://example.com/#secret",
        ] {
            assert!(secure_url(url).is_err());
        }
        assert!(secure_url("http://127.0.0.1:18090/discovery").is_ok());
        let mut draft = AuthDraft {
            auth_enabled: true,
            openid_discovery: "https://id.example/discovery".into(),
            openid_client_id: "cashu".into(),
            mint_max_bat: 32,
            operations: BTreeMap::new(),
        };
        assert!(draft.validate().is_err());
        draft.operations = OPERATIONS
            .iter()
            .map(|(name, _, _)| (name.to_string(), Mode::Blind))
            .collect();
        draft.validate().unwrap();
        assert_eq!(draft.snippet().matches("= \"blind\"").count(), 10);
        draft.mint_max_bat = 0;
        assert!(draft.validate().is_err());
        draft.auth_enabled = false;
        assert!(draft.validate().is_ok());
    }

    #[tokio::test]
    async fn tokens_are_burned_durably_and_bound_to_one_mint() {
        let path = std::env::temp_dir().join(format!("pecan-auth-{}.json", uuid::Uuid::new_v4()));
        let auth = MintAuth::load(path.clone()).await.unwrap();
        let policy = Policy { clear: Some(nut21::Settings::new("https://id.example/discovery".into(), "cashu".into(), vec![])), blind: Some(serde_json::from_value(json!({"bat_max_mint":1,"protected_endpoints":[{"method":"POST","path":"/v1/swap"}]})).unwrap()) };
        let binding = policy.binding("https://mint.example").unwrap();
        auth.state.lock().await.credentials = Some(Credentials {
            binding,
            token_endpoint: "https://id.example/token".into(),
            access_token: "private-cat".into(),
            refresh_token: None,
            expires_at: now() + 300,
            bats: vec!["authAsecond".into(), "authAfirst".into()],
        });
        assert!(auth
            .header("https://other.example", &policy, "POST", "/v1/swap")
            .await
            .is_err());
        let (_, token) = auth
            .header("https://mint.example", &policy, "POST", "/v1/swap")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(token, "authAfirst");
        assert!(token.is_sensitive());
        let reopened = MintAuth::load(path.clone()).await.unwrap();
        assert_eq!(
            reopened
                .state
                .lock()
                .await
                .credentials
                .as_ref()
                .unwrap()
                .bats,
            vec!["authAsecond"]
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
        reopened.disconnect().await.unwrap();
        assert!(MintAuth::load(path.clone())
            .await
            .unwrap()
            .state
            .lock()
            .await
            .credentials
            .is_none());
        tokio::fs::remove_file(path).await.unwrap();
    }
}
