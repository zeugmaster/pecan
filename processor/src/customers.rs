//! Keycloak customer administration. The service credential stays server-side.
use std::{sync::Arc, time::Duration};

use anyhow::{bail, ensure, Context, Result};
use reqwest::{Client, Method, RequestBuilder, StatusCode, Url};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::mint_auth::{secure_url, Policy};

pub const PAGE_SIZE: usize = 25;

#[derive(Clone)]
pub struct Customers {
    client: Client,
    issuer: String,
    admin: Url,
    client_id: String,
    secret: Arc<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum CustomerError {
    #[error("{0}")]
    Invalid(String),
    #[error("A customer with that username or email already exists.")]
    Duplicate,
    #[error("Customer no longer exists. Refresh the list.")]
    NotFound,
}

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Customer {
    pub id: String,
    pub username: String,
    #[serde(default)]
    pub email: Option<String>,
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub required_actions: Vec<String>,
    #[serde(default, skip_serializing)]
    service_account_client_id: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NewCustomer {
    pub username: String,
    #[serde(default)]
    pub email: String,
    pub password: String,
    pub password_confirm: String,
}

#[derive(Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub enum CustomerAction {
    SetEnabled {
        enabled: bool,
    },
    ResetPassword {
        password: String,
        password_confirm: String,
    },
    SignOut,
}

impl Customers {
    pub fn from_env() -> Result<Option<Self>> {
        let read = |key| std::env::var(key).unwrap_or_default();
        let issuer = read("CDK_BRANCH_PROCESSOR_KEYCLOAK_ISSUER");
        let client_id = read("CDK_BRANCH_PROCESSOR_KEYCLOAK_CLIENT_ID");
        let secret = read("CDK_BRANCH_PROCESSOR_KEYCLOAK_CLIENT_SECRET");
        if issuer.is_empty() && client_id.is_empty() && secret.is_empty() {
            return Ok(None);
        }
        Self::new(&issuer, &client_id, secret).map(Some)
    }

    fn new(issuer: &str, client_id: &str, secret: String) -> Result<Self> {
        secure_url(issuer)?;
        let mut admin = Url::parse(issuer)?;
        let issuer = issuer.trim_end_matches('/').to_owned();
        let (prefix, realm) = admin
            .path()
            .trim_end_matches('/')
            .rsplit_once("/realms/")
            .context("Keycloak issuer must end in /realms/<customer-realm>")?;
        ensure!(
            !realm.is_empty()
                && !realm.contains('/')
                && realm != "master"
                && !realm.contains('%')
                && admin.query().is_none(),
            "Use a dedicated Keycloak customer realm, not master"
        );
        let path = format!("{prefix}/admin/realms/{realm}/users");
        admin.set_path(&path);
        ensure!(
            !client_id.trim().is_empty()
                && client_id.len() <= 200
                && !client_id.chars().any(char::is_control)
                && !secret.trim().is_empty(),
            "Set KEYCLOAK_ISSUER, KEYCLOAK_CLIENT_ID and KEYCLOAK_CLIENT_SECRET together"
        );
        Ok(Self {
            client: Client::builder()
                .timeout(Duration::from_secs(15))
                .redirect(reqwest::redirect::Policy::none())
                .build()?,
            issuer,
            admin,
            client_id: client_id.to_owned(),
            secret: Arc::new(secret),
        })
    }

    pub fn issuer(&self) -> &str {
        &self.issuer
    }

    pub fn wallet_client(&self, policy: &Policy) -> Result<String> {
        let clear = policy
            .clear
            .as_ref()
            .context("Enable wallet authentication at the mint before managing customers.")?;
        ensure!(clear.openid_discovery == format!("{}/.well-known/openid-configuration", self.issuer),
            "The mint's active identity provider does not match the configured Keycloak realm. Customer management is blocked.");
        ensure!(
            clear.client_id != self.client_id,
            "The wallet client must be separate from Pecan's management client."
        );
        validate_text("Wallet client ID", &clear.client_id, 200, false)?;
        ensure!(
            ![".", ".."].contains(&clear.client_id.as_str())
                && !clear.client_id.contains(['/', '\\', '%']),
            "Invalid wallet client ID"
        );
        Ok(clear.client_id.clone())
    }

    async fn token(&self) -> Result<String> {
        let body = response(
            self.client
                .post(format!("{}/protocol/openid-connect/token", self.issuer))
                .form(&[
                    ("grant_type", "client_credentials"),
                    ("client_id", &self.client_id),
                    ("client_secret", self.secret.as_str()),
                ]),
            false,
        )
        .await?;
        body.get("access_token")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
            .context("Keycloak did not return a management token.")
    }

    fn url(&self, id: &str, suffix: &[&str]) -> Result<Url> {
        validate_text("Customer ID", id, 256, false)?;
        ensure!(
            id != "." && id != ".." && !id.contains(['/', '\\', '%']),
            "Invalid customer ID"
        );
        let mut url = self.admin.clone();
        url.path_segments_mut()
            .unwrap()
            .push(id)
            .extend(suffix.iter().copied());
        Ok(url)
    }

    pub async fn list(&self, search: &str, offset: u32) -> Result<(Vec<Customer>, bool)> {
        validate_text("Search", search, 200, true)?;
        let token = self.token().await?;
        let body = response(
            self.client
                .get(self.admin.clone())
                .bearer_auth(&token)
                .query(&[
                    // A leading wildcard keeps Keycloak's id: lookup syntax out of customer search.
                    ("search", format!("*{search}*")),
                    ("first", offset.to_string()),
                    ("max", (PAGE_SIZE + 1).to_string()),
                ]),
            false,
        )
        .await?;
        let mut users: Vec<Customer> =
            serde_json::from_value(body).context("Invalid Keycloak customer list.")?;
        let more = users.len() > PAGE_SIZE;
        users.truncate(PAGE_SIZE);
        users.retain(|user| user.service_account_client_id.is_none());
        Ok((users, more))
    }

    pub async fn create(&self, form: NewCustomer) -> Result<()> {
        validate_text("Username", form.username.trim(), 255, false)?;
        validate_text("Email", form.email.trim(), 254, true)?;
        validate_password(&form.password, &form.password_confirm)?;
        let mut user = json!({"username": form.username.trim(), "enabled": true,
            "credentials": [{"type": "password", "value": form.password, "temporary": true}]});
        if !form.email.trim().is_empty() {
            user["email"] = json!(form.email.trim());
        }
        response(
            self.client
                .post(self.admin.clone())
                .bearer_auth(self.token().await?)
                .json(&user),
            false,
        )
        .await?;
        Ok(())
    }

    pub async fn act(&self, id: &str, action: CustomerAction, wallet_client: &str) -> Result<()> {
        let url = self.url(id, &[])?;
        if let CustomerAction::ResetPassword {
            password,
            password_confirm,
        } = &action
        {
            validate_password(password, password_confirm)?;
        }
        let token = self.token().await?;
        let customer: Customer = serde_json::from_value(
            response(self.client.get(url.clone()).bearer_auth(&token), false).await?,
        )?;
        ensure!(
            customer.service_account_client_id.is_none(),
            "Service accounts cannot be managed as customers."
        );
        // User GETs omit the service-account marker; quoted search excludes service users.
        let matches: Vec<Customer> = serde_json::from_value(
            response(
                self.client
                    .get(self.admin.clone())
                    .bearer_auth(&token)
                    .query(&[("search", format!("\"{}\"", customer.username))]),
                false,
            )
            .await?,
        )?;
        ensure!(
            matches.iter().any(|user| user.id == id),
            "Service accounts cannot be managed as customers."
        );
        match action {
            CustomerAction::SetEnabled { enabled } => {
                response(
                    self.client
                        .put(url)
                        .bearer_auth(&token)
                        .json(&json!({"enabled": enabled})),
                    false,
                )
                .await?;
                if !enabled {
                    self.sign_out(id, &token, wallet_client).await.context("Account disabled, but session sign-out failed. Refresh and use Sign out again.")?;
                }
            }
            CustomerAction::ResetPassword { password, .. } => {
                response(
                    self.client
                        .put(self.url(id, &["reset-password"])?)
                        .bearer_auth(&token)
                        .json(&json!({"type": "password", "value": password, "temporary": true})),
                    false,
                )
                .await?;
                self.sign_out(id, &token, wallet_client).await.context(
                    "Password reset, but session sign-out failed. Refresh and use Sign out again.",
                )?;
            }
            CustomerAction::SignOut => self.sign_out(id, &token, wallet_client).await?,
        }
        Ok(())
    }

    async fn sign_out(&self, id: &str, token: &str, wallet_client: &str) -> Result<()> {
        response(
            self.client
                .post(self.url(id, &["logout"])?)
                .bearer_auth(token),
            false,
        )
        .await?;
        // Keycloak's logout endpoint alone does not revoke offline refresh tokens.
        response(
            self.client
                .request(Method::DELETE, self.url(id, &["consents", wallet_client])?)
                .bearer_auth(token),
            true,
        )
        .await?;
        Ok(())
    }
}

fn validate_text(label: &str, value: &str, max: usize, empty: bool) -> Result<()> {
    if (!empty && value.trim().is_empty())
        || value.len() > max
        || value.chars().any(char::is_control)
    {
        return Err(CustomerError::Invalid(format!("{label} is required, must fit within {max} bytes and cannot contain control characters.")).into());
    }
    Ok(())
}

fn validate_password(password: &str, confirm: &str) -> Result<()> {
    if password != confirm || password.chars().count() < 8 || password.len() > 1024 {
        return Err(CustomerError::Invalid("Passwords must match and contain 8–1024 characters. Keycloak's password policy also applies.".into()).into());
    }
    Ok(())
}

async fn response(request: RequestBuilder, missing_ok: bool) -> Result<Value> {
    let mut res = request.send().await.map_err(|_| {
        anyhow::anyhow!(
            "Keycloak request failed. Refresh before retrying; a change may have completed."
        )
    })?;
    match res.status() {
        StatusCode::NOT_FOUND if missing_ok => return Ok(Value::Null),
        StatusCode::NOT_FOUND => return Err(CustomerError::NotFound.into()),
        StatusCode::CONFLICT => return Err(CustomerError::Duplicate.into()),
        StatusCode::BAD_REQUEST => return Err(CustomerError::Invalid("Keycloak rejected the change. Check the username, email and realm password policy.".into()).into()),
        StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN => bail!("Keycloak denied customer management. Check the service credential and its realm permissions."),
        status if !status.is_success() => bail!("Keycloak returned HTTP {}. Refresh before retrying.", status.as_u16()),
        _ => {}
    }
    // Never forward provider error bodies, credentials, or unbounded responses.
    let mut bytes = Vec::new();
    while let Some(chunk) = res
        .chunk()
        .await
        .context("Could not read Keycloak response.")?
    {
        ensure!(
            bytes.len() + chunk.len() <= 1_048_576,
            "Keycloak response is too large."
        );
        bytes.extend_from_slice(&chunk);
    }
    if bytes.is_empty() {
        Ok(Value::Null)
    } else {
        serde_json::from_slice(&bytes).context("Invalid Keycloak response.")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn provider_redirects_and_errors_cannot_expose_credentials() {
        use axum::{routing::get, Router};
        use std::sync::atomic::{AtomicUsize, Ordering};
        let hits = Arc::new(AtomicUsize::new(0));
        let target_hits = hits.clone();
        let server = Router::new()
            .route(
                "/redirect",
                get(|| async { (StatusCode::TEMPORARY_REDIRECT, [("location", "/target")]) }),
            )
            .route(
                "/target",
                get(move || {
                    let hits = target_hits.clone();
                    async move {
                        hits.fetch_add(1, Ordering::SeqCst);
                        "{}"
                    }
                }),
            )
            .route(
                "/error",
                get(|| async { (StatusCode::INTERNAL_SERVER_ERROR, "secret-provider-token") }),
            )
            .route("/large", get(|| async { "x".repeat(1_048_577) }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let task = tokio::spawn(async move { axum::serve(listener, server).await.unwrap() });
        let customers = Customers::new(
            &format!("{base}/realms/mint"),
            "management",
            "secret".into(),
        )
        .unwrap();
        for path in ["redirect", "error", "large"] {
            let error = response(
                customers
                    .client
                    .get(format!("{base}/{path}"))
                    .bearer_auth("secret-provider-token"),
                false,
            )
            .await
            .unwrap_err();
            assert!(!format!("{error:#}").contains("secret-provider-token"));
        }
        assert_eq!(hits.load(Ordering::SeqCst), 0);
        task.abort();
    }

    #[test]
    fn management_is_bound_to_the_customer_realm() {
        for url in [
            "http://id.example/realms/mint",
            "https://id.example/realms/master",
            "https://id.example/realms/mint?token=x",
            "https://id.example/realms/a/b",
        ] {
            assert!(Customers::new(url, "pecan-admin", "secret".into()).is_err());
        }
        let c = Customers::new(
            "https://id.example/auth/realms/mint",
            "pecan-admin",
            "secret".into(),
        )
        .unwrap();
        assert_eq!(
            c.admin.as_str(),
            "https://id.example/auth/admin/realms/mint/users"
        );
        let policy = |issuer, client| {
            Policy::from_info(&json!({"nuts":{"21":{
                "openid_discovery": issuer, "client_id": client, "protected_endpoints":[]
            }}}))
            .unwrap()
        };
        assert!(c
            .wallet_client(&policy("https://other.example/discovery", "cashu"))
            .is_err());
        let discovery = "https://id.example/auth/realms/mint/.well-known/openid-configuration";
        assert!(c.wallet_client(&policy(discovery, "pecan-admin")).is_err());
        assert_eq!(
            c.wallet_client(&policy(discovery, "cashu")).unwrap(),
            "cashu"
        );
        for id in ["..", "a/b", "%2e%2e", "a\\b"] {
            assert!(c.url(id, &[]).is_err());
        }
    }

    #[test]
    fn customer_projection_and_input_do_not_expose_or_accept_provider_secrets() {
        let user: Customer = serde_json::from_value(json!({"id":"1", "username":"alice", "credentials":[{"value":"secret"}], "serviceAccountClientId":"admin"})).unwrap();
        let value = serde_json::to_value(user).unwrap();
        assert!(value.get("credentials").is_none());
        assert!(value.get("serviceAccountClientId").is_none());
        assert!(validate_password("short", "short").is_err());
        assert!(validate_password("test-password", "different").is_err());
        assert!(validate_password("test-password", "test-password").is_ok());
        assert!(serde_json::from_value::<CustomerAction>(
            json!({"action":"set_enabled", "enabled":true, "realmRoles":["admin"]})
        )
        .is_err());
    }
}
