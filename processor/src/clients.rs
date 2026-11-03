//! HTTP client for the attached mint's public (wallet-facing) API.
//!
//! This is the processor's only outbound channel to the mint: `/v1/info` and
//! `/v1/keysets` for the attachment checklist and read-only console cards,
//! per-quote lookups to cross-check a teller match against the mint's own
//! records, and quote creation for the end-to-end self-test. There is no
//! management RPC and no database access — the mint is configured and
//! operated by its own operator.

use std::sync::Arc;
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Probing the same URL wallets use keeps the checklist honest, but it must
/// never hang the console snapshot — keep the timeout short.
const HTTP_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Clone)]
pub struct MintHttpClient {
    base: Arc<String>,
    http: reqwest::Client,
    auth: Option<crate::mint_auth::MintAuth>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[allow(dead_code)]
pub struct KeysetEntry {
    pub id: String,
    pub unit: String,
    pub active: bool,
    pub input_fee_ppk: u64,
    /// Unix seconds. Present on keysets rotated with a `final_expiry`; the
    /// mint stops honoring the keyset once the value is in the past.
    #[serde(default)]
    pub final_expiry: Option<u64>,
}

#[derive(Debug, Clone, Deserialize)]
struct KeysetsResponse {
    keysets: Vec<KeysetEntry>,
}

/// A mint HTTP response that came back non-2xx: kept separate from transport
/// errors so the self-test can tell "mint said no" from "mint unreachable".
#[derive(Debug, Clone)]
pub struct MintHttpError {
    pub status: u16,
    pub body: String,
}

impl std::fmt::Display for MintHttpError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "HTTP {}: {}", self.status, self.body)
    }
}

/// Response to creating a custom-method mint quote (the self-test's deposit
/// leg). Extra fields the mint includes are ignored.
#[derive(Debug, Clone, Deserialize)]
pub struct MintQuoteProbe {
    /// The mint-generated quote id.
    pub quote: String,
    /// The processor-issued payment request (the ticket id) echoed back.
    #[serde(default)]
    pub request: String,
    #[serde(default)]
    pub expiry: Option<u64>,
    #[serde(default)]
    pub pubkey: Option<String>,
}

/// Response to creating a custom-method melt quote (the self-test's payout leg).
#[derive(Debug, Clone, Deserialize)]
pub struct MeltQuoteProbe {
    pub quote: String,
    #[serde(default)]
    pub expiry: Option<u64>,
}

impl MintHttpClient {
    pub fn new(base: impl Into<String>) -> Self {
        let http = reqwest::Client::builder()
            .timeout(HTTP_TIMEOUT)
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .expect("static mint HTTP client configuration");
        Self {
            base: Arc::new(base.into()),
            http,
            auth: None,
        }
    }

    pub fn with_auth(mut self, auth: crate::mint_auth::MintAuth) -> Self {
        self.auth = Some(auth);
        self
    }

    async fn authorized(
        &self,
        request: reqwest::RequestBuilder,
        method: &str,
        route: &str,
    ) -> Result<reqwest::RequestBuilder> {
        let Some(auth) = &self.auth else {
            return Ok(request);
        };
        let policy = crate::mint_auth::Policy::from_info(&self.get_info().await?)?;
        Ok(
            match auth.header(self.base(), &policy, method, route).await? {
                Some((name, value)) => request.header(name, value),
                None => request,
            },
        )
    }

    fn base(&self) -> &str {
        self.base.trim_end_matches('/')
    }

    pub async fn list_keysets(&self) -> Result<Vec<KeysetEntry>> {
        let base = self.base();
        let r = self
            .http
            .get(format!("{base}/v1/keysets"))
            .send()
            .await
            .with_context(|| format!("GET {base}/v1/keysets"))?;
        if !r.status().is_success() {
            return Err(anyhow!("list_keysets: HTTP {}", r.status()));
        }
        let parsed: KeysetsResponse = r.json().await?;
        Ok(parsed.keysets)
    }

    pub async fn get_info(&self) -> Result<Value> {
        let base = self.base();
        let r = self
            .http
            .get(format!("{base}/v1/info"))
            .send()
            .await
            .with_context(|| format!("GET {base}/v1/info"))?;
        if !r.status().is_success() {
            return Err(anyhow!("get_info: HTTP {}", r.status()));
        }
        Ok(r.json().await?)
    }

    /// Fetch one mint quote from the mint's public API. `Ok(None)` means the
    /// mint does not know the quote (404) — for the teller that distinction
    /// (unknown vs. unreachable mint) decides between "reject" and "retry".
    pub async fn get_mint_quote(
        &self,
        method: &str,
        quote_id: &str,
    ) -> Result<Option<MintQuoteSnapshot>> {
        let base = self.base();
        let url = format!("{base}/v1/mint/quote/{method}/{quote_id}");
        let r = self
            .authorized(
                self.http.get(&url),
                "GET",
                &format!("/v1/mint/quote/{method}"),
            )
            .await?
            .send()
            .await
            .with_context(|| format!("GET {url}"))?;
        if r.status() == reqwest::StatusCode::NOT_FOUND {
            return Ok(None);
        }
        if !r.status().is_success() {
            return Err(anyhow!("get_mint_quote: HTTP {}", r.status()));
        }
        Ok(Some(r.json().await?))
    }

    /// Melt-quote sibling of [`Self::get_mint_quote`].
    pub async fn get_melt_quote(
        &self,
        method: &str,
        quote_id: &str,
    ) -> Result<Option<MeltQuoteSnapshot>> {
        let base = self.base();
        let url = format!("{base}/v1/melt/quote/{method}/{quote_id}");
        let r = self
            .authorized(
                self.http.get(&url),
                "GET",
                &format!("/v1/melt/quote/{method}"),
            )
            .await?
            .send()
            .await
            .with_context(|| format!("GET {url}"))?;
        if r.status() == reqwest::StatusCode::NOT_FOUND {
            return Ok(None);
        }
        if !r.status().is_success() {
            return Err(anyhow!("get_melt_quote: HTTP {}", r.status()));
        }
        Ok(Some(r.json().await?))
    }

    /// Create a custom-method mint quote, acting as a wallet — the self-test's
    /// deposit leg. `Ok(Err(_))` is a mint-side rejection (status + body);
    /// `Err(_)` is transport (unreachable, timeout, TLS).
    pub async fn create_probe_mint_quote(
        &self,
        method: &str,
        unit: &str,
        amount: u64,
        pubkey: &str,
        description: &str,
    ) -> Result<std::result::Result<MintQuoteProbe, MintHttpError>> {
        let base = self.base();
        let url = format!("{base}/v1/mint/quote/{method}");
        let body = serde_json::json!({
            "amount": amount,
            "unit": unit,
            "description": description,
            "pubkey": pubkey,
        });
        let r = self
            .authorized(
                self.http.post(&url).json(&body),
                "POST",
                &format!("/v1/mint/quote/{method}"),
            )
            .await?
            .send()
            .await
            .with_context(|| format!("POST {url}"))?;
        if !r.status().is_success() {
            let status = r.status().as_u16();
            let body = truncate_body(r.text().await.unwrap_or_default());
            return Ok(Err(MintHttpError { status, body }));
        }
        Ok(Ok(r.json().await.context("parse mint quote response")?))
    }

    /// Create a custom-method melt quote — the self-test's payout leg.
    pub async fn create_probe_melt_quote(
        &self,
        method: &str,
        unit: &str,
        amount: u64,
        request: &str,
    ) -> Result<std::result::Result<MeltQuoteProbe, MintHttpError>> {
        let base = self.base();
        let url = format!("{base}/v1/melt/quote/{method}");
        let body = serde_json::json!({
            "method": method,
            "request": request,
            "unit": unit,
            "amount": amount,
        });
        let r = self
            .authorized(
                self.http.post(&url).json(&body),
                "POST",
                &format!("/v1/melt/quote/{method}"),
            )
            .await?
            .send()
            .await
            .with_context(|| format!("POST {url}"))?;
        if !r.status().is_success() {
            let status = r.status().as_u16();
            let body = truncate_body(r.text().await.unwrap_or_default());
            return Ok(Err(MintHttpError { status, body }));
        }
        Ok(Ok(r.json().await.context("parse melt quote response")?))
    }
}

fn truncate_body(body: String) -> String {
    const MAX: usize = 300;
    if body.chars().count() <= MAX {
        body
    } else {
        let truncated: String = body.chars().take(MAX).collect();
        format!("{truncated}…")
    }
}

/// The wallet-visible state of a custom-method mint quote. Custom quotes have
/// no `state` field — "open" means nothing paid or issued yet.
#[derive(Debug, Clone, Deserialize)]
pub struct MintQuoteSnapshot {
    /// The processor-issued payment request (the ticket id).
    pub request: String,
    #[serde(default)]
    pub amount: Option<u64>,
    #[serde(default)]
    pub unit: Option<String>,
    #[serde(default)]
    pub amount_paid: u64,
    #[serde(default)]
    pub amount_issued: u64,
    /// NUT-20 lock. Absent means the quote is not locked to a wallet key.
    #[serde(default)]
    pub pubkey: Option<String>,
}

/// The wallet-visible state of a custom-method melt quote.
#[derive(Debug, Clone, Deserialize)]
pub struct MeltQuoteSnapshot {
    pub amount: u64,
    /// "UNPAID" | "PENDING" | "PAID" (serialized NUT-05 state).
    #[serde(default)]
    pub state: String,
    #[serde(default)]
    pub unit: Option<String>,
}
