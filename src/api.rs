use anyhow::{Result, anyhow, bail};
use reqwest::header::{HeaderMap, SET_COOKIE};
use reqwest::{Client, RequestBuilder};
use serde_json::{Value, json};
use std::sync::Mutex;

use crate::model::{Message, Profile, Scope, ServerDetail, Session, SyncPayload};

#[derive(Clone, Debug)]
pub struct Auth {
    pub token: String,
    pub csrf: String,
}

pub struct Api {
    http: Client,
    base: Mutex<String>,
    auth: Mutex<Option<Auth>>,
}

pub fn normalize_base(input: &str) -> String {
    let trimmed = input.trim().trim_end_matches('/');
    if trimmed.is_empty() {
        return String::new();
    }
    if trimmed.starts_with("http://") || trimmed.starts_with("https://") {
        trimmed.to_string()
    } else {
        format!("https://{trimmed}")
    }
}

pub fn ws_url(base: &str) -> Result<String> {
    let url = url::Url::parse(base).map_err(|e| anyhow!("invalid server url: {e}"))?;
    let scheme = match url.scheme() {
        "https" | "wss" => "wss",
        "http" | "ws" => "ws",
        other => bail!("unsupported scheme: {other}"),
    };
    let host = url.host_str().ok_or_else(|| anyhow!("missing host"))?;
    let port = match url.port() {
        Some(p) => format!(":{p}"),
        None => String::new(),
    };
    Ok(format!("{scheme}://{host}{port}/ws"))
}

pub fn origin(base: &str) -> Result<String> {
    let url = url::Url::parse(base).map_err(|e| anyhow!("invalid server url: {e}"))?;
    let scheme = url.scheme();
    let host = url.host_str().ok_or_else(|| anyhow!("missing host"))?;
    let port = match url.port() {
        Some(p) => format!(":{p}"),
        None => String::new(),
    };
    Ok(format!("{scheme}://{host}{port}"))
}

fn unwrap_envelope(value: Value) -> Result<Value> {
    let ok = value.get("ok").and_then(Value::as_bool).unwrap_or(false);
    if !ok {
        let error = value
            .get("error")
            .and_then(Value::as_str)
            .unwrap_or("request_failed");
        bail!("{error}");
    }
    Ok(value.get("data").cloned().unwrap_or(Value::Null))
}

fn find_session_cookie(headers: &HeaderMap) -> Option<String> {
    for value in headers.get_all(SET_COOKIE) {
        let Ok(raw) = value.to_str() else { continue };
        for part in raw.split(';') {
            if let Some(token) = part.trim().strip_prefix("pw_session=") {
                return Some(token.to_string());
            }
        }
    }
    None
}

impl Api {
    pub fn new() -> Result<Self> {
        let http = Client::builder()
            .user_agent("plainwire-desktop/0.1")
            .build()?;
        Ok(Self {
            http,
            base: Mutex::new(String::new()),
            auth: Mutex::new(None),
        })
    }

    pub fn base(&self) -> String {
        self.base.lock().unwrap().clone()
    }

    pub fn auth(&self) -> Option<Auth> {
        self.auth.lock().unwrap().clone()
    }

    pub fn clear(&self) {
        *self.auth.lock().unwrap() = None;
    }

    fn url(&self, path: &str) -> String {
        format!("{}{}", self.base(), path)
    }

    fn authed(&self, request: RequestBuilder) -> Result<RequestBuilder> {
        let auth = self
            .auth
            .lock()
            .unwrap()
            .clone()
            .ok_or_else(|| anyhow!("not authenticated"))?;
        Ok(request
            .header("Cookie", format!("pw_session={}", auth.token))
            .header("X-CSRF-Token", auth.csrf))
    }

    async fn decode(response: reqwest::Response) -> Result<Value> {
        let status = response.status();
        let text = response.text().await?;
        let value: Value = serde_json::from_str(&text)
            .map_err(|e| anyhow!("invalid json from server ({status}): {e}"))?;
        unwrap_envelope(value)
    }

    pub async fn login(&self, base: &str, username: &str, password: &str) -> Result<Session> {
        let normalized = normalize_base(base);
        if normalized.is_empty() {
            bail!("server url is required");
        }
        *self.base.lock().unwrap() = normalized;
        let body = json!({ "username": username, "password": password });
        let response = self
            .http
            .post(self.url("/api/login"))
            .json(&body)
            .send()
            .await?;
        let token = find_session_cookie(response.headers());
        let data = Self::decode(response).await?;
        let session: Session = serde_json::from_value(data)?;
        let token = token.ok_or_else(|| anyhow!("server did not issue a session cookie"))?;
        *self.auth.lock().unwrap() = Some(Auth {
            token,
            csrf: session.csrf.clone(),
        });
        Ok(session)
    }

    pub async fn sync(&self) -> Result<SyncPayload> {
        let response = self
            .authed(self.http.get(self.url("/api/sync")))?
            .send()
            .await?;
        let data = Self::decode(response).await?;
        Ok(serde_json::from_value(data)?)
    }

    pub async fn server_detail(&self, server_id: i64) -> Result<ServerDetail> {
        let response = self
            .authed(self.http.get(self.url(&format!("/api/server/{server_id}"))))?
            .send()
            .await?;
        let data = Self::decode(response).await?;
        Ok(serde_json::from_value(data)?)
    }

    pub async fn messages(
        &self,
        scope: Scope,
        scope_id: i64,
        _before: Option<i64>,
    ) -> Result<Vec<Message>> {
        let response = self.authed(self.http.get(self.url("/api/messages")))?
            .query(&[
                ("scope", scope.as_str().to_string()),
                ("scope_id", scope_id.to_string()),
            ])
            .send()
            .await
            .map_err(|e| anyhow!("failed to send request: {e}"))?;
        let data = Self::decode(response).await?;
        Ok(serde_json::from_value(data)?)
    }

    pub async fn send(&self, scope: Scope, scope_id: i64, body: &str) -> Result<Message> {
        let path = match scope {
            Scope::Channel => format!("/api/channels/{scope_id}/messages"),
            Scope::Direct => format!("/api/conversation/{scope_id}/messages"),
        };
        let response = self
            .authed(self.http.post(self.url(&path)))?
            .json(&json!({ "body": body }))
            .send()
            .await?;
        let data = Self::decode(response).await?;
        Ok(serde_json::from_value(data)?)
    }

    pub async fn list_profiles(&self) -> Result<Vec<Profile>> {
        let response = self
            .authed(self.http.get(self.url("/api/profiles")))?
            .send()
            .await?;
        let data = Self::decode(response).await?;
        Ok(serde_json::from_value(data)?)
    }

    pub async fn profile_by_user_id(&self, user_id: i64) -> Result<Profile> {
        let response = self
            .authed(self.http.get(self.url(&format!("/api/profile/{user_id}"))))?
            .send()
            .await?;
        let data = Self::decode(response).await?;
        Ok(serde_json::from_value(data)?)
    }

    pub async fn profile_by_username(&self, username: &str) -> Result<Profile> {
        let response = self
            .authed(self.http.get(self.url(&format!("/api/profile/username/{username}"))))?
            .send()
            .await?;
        let data = Self::decode(response).await?;
        Ok(serde_json::from_value(data)?)
    }

    pub async fn upload_image(&self, file_path: &str, content_type: &str) -> Result<Message> {
        let file = std::fs::read(file_path)
            .map_err(|e| anyhow!("failed to read file: {e}"))?;
        if file.len() > 25_000_000 {
            bail!("image exceeds 25 MB limit");
        }
        let form = reqwest::multipart::Form::new()
            .part("file", reqwest::multipart::Part::bytes(file).mime_str(content_type)?);
        let response = self
            .authed(self.http.post(self.url("/api/uploads/image")))?
            .multipart(form)
            .send()
            .await?;
        let data = Self::decode(response).await?;
        Ok(serde_json::from_value(data)?)
    }
}
