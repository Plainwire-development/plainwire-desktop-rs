use anyhow::{Result, anyhow, bail};
use reqwest::header::{HeaderMap, SET_COOKIE};
use reqwest::{Client, RequestBuilder};
use serde_json::{Value, json};
use std::sync::Mutex;

use crate::model::{
    Message, ProfileResponse, RtcConfig, Scope, ServerDetail, Session, SyncPayload, User,
};

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

    pub fn set_base(&self, base: &str) {
        *self.base.lock().unwrap() = base.trim_end_matches('/').to_string();
    }

    pub fn set_auth(&self, token: String, csrf: String) {
        *self.auth.lock().unwrap() = Some(Auth { token, csrf });
    }

    pub async fn session(&self) -> Result<Session> {
        let response = self
            .authed(self.http.get(self.url("/api/me")))?
            .send()
            .await?;
        let data = Self::decode(response).await?;
        Ok(serde_json::from_value(data)?)
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

    pub async fn logout(&self) -> Result<()> {
        if self.auth().is_some() {
            let _ = self
                .authed(self.http.post(self.url("/api/logout")))?
                .send()
                .await;
        }
        Ok(())
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
        before: Option<i64>,
    ) -> Result<Vec<Message>> {
        let mut query = vec![
            ("scope", scope.as_str().to_string()),
            ("scope_id", scope_id.to_string()),
        ];
        if let Some(before) = before {
            query.push(("before", before.to_string()));
        }
        let response = self
            .authed(self.http.get(self.url("/api/messages")))?
            .query(&query)
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

    pub async fn mark_conversation_read(&self, conversation_id: i64) -> Result<()> {
        let _ = self
            .authed(self
                .http
                .post(self.url(&format!("/api/conversation/{conversation_id}/read"))))?
            .send()
            .await;
        Ok(())
    }

    pub async fn search_users(&self, query: &str) -> Result<Vec<User>> {
        let trimmed = query.trim();
        if trimmed.len() < 2 {
            return Ok(Vec::new());
        }
        let response = self
            .authed(self.http.get(self.url("/api/users")))?
            .query(&[("q", trimmed)])
            .send()
            .await?;
        let data = Self::decode(response).await?;
        Ok(serde_json::from_value(data)?)
    }

    pub async fn profile_by_user_id(&self, user_id: i64) -> Result<ProfileResponse> {
        let response = self
            .authed(self.http.get(self.url(&format!("/api/profile/{user_id}"))))?
            .send()
            .await?;
        let data = Self::decode(response).await?;
        Ok(serde_json::from_value(data)?)
    }

    pub async fn profile_by_username(&self, username: &str) -> Result<ProfileResponse> {
        let response = self
            .authed(self.http.get(self.url("/api/profile-by-username")))?
            .query(&[("username", username)])
            .send()
            .await?;
        let data = Self::decode(response).await?;
        Ok(serde_json::from_value(data)?)
    }

    pub async fn rtc_config(&self) -> Result<RtcConfig> {
        let response = self
            .authed(self.http.get(self.url("/api/rtc-config")))?
            .send()
            .await?;
        let data = Self::decode(response).await?;
        Ok(serde_json::from_value(data)?)
    }

    pub async fn fetch_image(&self, url: &str) -> Result<Vec<u8>> {
        let target = absolute_media_url(&self.base(), url);
        let response = self.authed(self.http.get(target))?.send().await?;
        let status = response.status();
        if !status.is_success() {
            bail!("image request failed: {status}");
        }
        let body = response.bytes().await?;
        if body.is_empty() {
            bail!("image response was empty");
        }
        Ok(body.to_vec())
    }

    pub async fn upload_file(
        &self,
        name: &str,
        content_type: &str,
        bytes: Vec<u8>,
    ) -> Result<Uploaded> {
        if bytes.is_empty() {
            bail!("refusing to upload an empty file");
        }
        let request = self
            .http
            .post(self.url("/api/uploads"))
            .header("X-File-Name", percent_encode_component(name))
            .header("Content-Type", content_type)
            .body(bytes);
        let data = Self::decode(self.authed(request)?.send().await?).await?;
        Ok(Uploaded {
            name: string_field(&data, "name"),
            content_type: string_field(&data, "content_type"),
            url: string_field(&data, "url"),
        })
    }
}

#[derive(Clone, Debug)]
pub struct Uploaded {
    pub name: String,
    pub content_type: String,
    pub url: String,
}

fn string_field(value: &Value, key: &str) -> String {
    value
        .get(key)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

pub fn percent_encode_component(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    for byte in input.as_bytes() {
        let byte = *byte;
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'!' | b'~' | b'*' | b'\'' | b'(' | b')') {
            out.push(byte as char);
        } else {
            out.push('%');
            out.push_str(&format!("{byte:02X}"));
        }
    }
    out
}

pub fn guess_content_type(path: &std::path::Path) -> &'static str {
    let extension = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    match extension.as_str() {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "avif" => "image/avif",
        "bmp" => "image/bmp",
        "txt" | "log" => "text/plain",
        "md" => "text/markdown",
        "json" => "application/json",
        "pdf" => "application/pdf",
        _ => "application/octet-stream",
    }
}

pub fn safe_attachment_name(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .map(|c| if matches!(c, '\\' | ']' | '[' | '(' | ')' | '\r' | '\n') { '_' } else { c })
        .collect();
    let trimmed = cleaned.trim();
    if trimmed.is_empty() {
        "file".to_string()
    } else {
        trimmed.to_string()
    }
}

pub fn attachment_markup(name: &str, content_type: &str, url: &str) -> String {
    let safe = safe_attachment_name(name);
    if content_type.starts_with("image/") {
        format!("![{safe}]({url})")
    } else {
        format!("[{safe}]({url})")
    }
}

pub fn absolute_media_url(base: &str, url: &str) -> String {
    if url.starts_with("http://") || url.starts_with("https://") || url.starts_with("data:") {
        return url.to_string();
    }
    let trimmed = base.trim_end_matches('/');
    if trimmed.is_empty() {
        return url.to_string();
    }
    if url.starts_with('/') {
        format!("{trimmed}{url}")
    } else {
        format!("{trimmed}/{url}")
    }
}

pub fn decode_image(bytes: &[u8]) -> Result<image::RgbaImage> {
    image::load_from_memory(bytes)
        .map(|img| img.to_rgba8())
        .map_err(|e| anyhow!("could not decode image: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_me_endpoint_returns_a_session_and_not_a_flat_user() {
        let body = r#"{"user":{"id":42,"username":"ada","display_name":"Ada",
                       "avatar_url":"/api/media/ada.png","status":"online"},
                       "csrf":"csrf-abc","server_time":7}"#;
        let value: Value = serde_json::from_str(body).unwrap();

        let session: Session = serde_json::from_value(value.clone()).unwrap();
        assert_eq!(session.csrf, "csrf-abc");
        assert_eq!(session.user.id, 42);
        assert_eq!(session.user.display_name, "Ada");
        assert_eq!(session.user.avatar_url, "/api/media/ada.png");

        let flattened: User = serde_json::from_value(value).unwrap();
        assert_eq!(
            flattened.id, 0,
            "reading /api/me as a flat user silently yields an empty profile"
        );
        assert!(flattened.avatar_url.is_empty());
    }

    #[test]
    fn server_relative_media_resolves_against_the_instance() {

        assert_eq!(
            absolute_media_url("https://plainwi.re", "/api/media/abc"),
            "https://plainwi.re/api/media/abc"
        );
        assert_eq!(
            absolute_media_url("https://plainwi.re/", "/api/files/x.png"),
            "https://plainwi.re/api/files/x.png"
        );
        assert_eq!(
            absolute_media_url("https://plainwi.re", "media/abc"),
            "https://plainwi.re/media/abc"
        );
    }

    #[test]
    fn absolute_urls_are_left_alone() {
        for url in [
            "https://cdn.example/a.png",
            "http://cdn.example/a.png",
            "data:image/png;base64,AAAA",
        ] {
            assert_eq!(absolute_media_url("https://plainwi.re", url), url);
        }
    }

    #[test]
    fn base_is_normalized_for_the_form() {
        assert_eq!(normalize_base("plainwi.re"), "https://plainwi.re");
        assert_eq!(normalize_base("  https://plainwi.re/  "), "https://plainwi.re");
        assert_eq!(normalize_base(""), "");
    }

    #[test]
    fn websocket_url_follows_the_scheme() {
        assert_eq!(
            ws_url("https://plainwi.re").unwrap(),
            "wss://plainwi.re/ws"
        );
        assert_eq!(ws_url("http://localhost:4000").unwrap(), "ws://localhost:4000/ws");
    }

    #[test]
    fn images_decode_for_the_texture_upload() {
        let mut png = std::io::Cursor::new(Vec::new());
        image::DynamicImage::new_rgba8(3, 2)
            .write_to(&mut png, image::ImageFormat::Png)
            .expect("encodes");
        let decoded = decode_image(&png.into_inner()).expect("decodes");
        assert_eq!((decoded.width(), decoded.height()), (3, 2));
    }

    #[test]
    fn fetch_image_sends_the_session_cookie() {

        use std::io::{Read, Write};

        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let png = {
            let mut buffer = std::io::Cursor::new(Vec::new());
            image::DynamicImage::new_rgba8(2, 2)
                .write_to(&mut buffer, image::ImageFormat::Png)
                .unwrap();
            buffer.into_inner()
        };

        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0u8; 2048];
            let _ = stream.read(&mut request);
            let request = String::from_utf8_lossy(&request).to_lowercase();
            if request.contains("cookie: pw_session=session-token") {
                stream
                    .write_all(
                        format!(
                            "HTTP/1.1 200 OK\r\nContent-Type: image/png\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                            png.len()
                        )
                        .as_bytes(),
                    )
                    .unwrap();
                stream.write_all(&png).unwrap();
                true
            } else {
                stream
                    .write_all(b"HTTP/1.1 401 Unauthorized\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
                    .unwrap();
                false
            }
        });

        let api = Api::new().unwrap();
        api.set_base(&format!("http://127.0.0.1:{port}"));
        api.set_auth("session-token".to_string(), "csrf".to_string());
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let bytes = runtime
            .block_on(api.fetch_image("/api/media/abc"))
            .expect("avatar downloads with the session cookie");
        assert!(server.join().unwrap(), "request carried the cookie");
        assert!(!bytes.is_empty());
        assert!(decode_image(&bytes).is_ok());
    }

    #[test]
    fn fetch_image_without_authentication_is_an_error() {
        let api = Api::new().unwrap();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        assert!(
            runtime.block_on(api.fetch_image("/api/media/abc")).is_err(),
            "media requests must not be attempted without a session"
        );
    }

    #[test]
    fn file_name_header_is_percent_encoded() {
        assert_eq!(percent_encode_component("image.png"), "image.png");
        assert_eq!(percent_encode_component("a b.png"), "a%20b.png");
        assert_eq!(
            percent_encode_component("héllo wörld.png"),
            "h%C3%A9llo%20w%C3%B6rld.png"
        );
        assert_eq!(
            percent_encode_component("100% (final).PNG"),
            "100%25%20(final).PNG"
        );
    }

    #[test]
    fn content_type_is_guessed_from_the_extension() {
        assert_eq!(guess_content_type(std::path::Path::new("a.PNG")), "image/png");
        assert_eq!(guess_content_type(std::path::Path::new("a.jpeg")), "image/jpeg");
        assert_eq!(guess_content_type(std::path::Path::new("a.txt")), "text/plain");
        assert_eq!(
            guess_content_type(std::path::Path::new("archive.tar.gz")),
            "application/octet-stream"
        );
        assert_eq!(
            guess_content_type(std::path::Path::new("noextension")),
            "application/octet-stream"
        );
        for guessed in [
            guess_content_type(std::path::Path::new("a.png")),
            guess_content_type(std::path::Path::new("a.txt")),
            guess_content_type(std::path::Path::new("a.pdf")),
        ] {
            let (major, minor) = guessed.split_once('/').expect("media type has two parts");
            assert!(!major.is_empty() && !minor.is_empty());
            for part in [major, minor] {
                assert!(
                    part.bytes().all(|b| {
                        b.is_ascii_lowercase() || b.is_ascii_digit() || b".+-".contains(&b)
                    }),
                    "the server rejects media types outside [a-z0-9!#$&^_.+-]: {guessed}"
                );
            }
        }
    }

    #[test]
    fn markup_matches_the_web_client() {
        assert_eq!(
            attachment_markup("image.png", "image/png", "/api/files/abc"),
            "![image.png](/api/files/abc)"
        );
        assert_eq!(
            attachment_markup("notes.txt", "text/plain", "/api/files/abc"),
            "[notes.txt](/api/files/abc)"
        );
        assert_eq!(
            attachment_markup("a(b)[c].png", "image/png", "/api/files/abc"),
            "![a_b__c_.png](/api/files/abc)",
            "the web client replaces each of \\ ] [ ( ) with its own underscore"
        );
        assert_eq!(
            attachment_markup("no-type", "", "/api/files/abc"),
            "[no-type](/api/files/abc)"
        );
    }

    #[test]
    fn uploading_without_a_session_is_an_error() {
        let api = Api::new().unwrap();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        assert!(runtime.block_on(api.upload_file("a.png", "image/png", vec![1, 2, 3])).is_err());
        assert!(
            runtime.block_on(api.upload_file("a.png", "image/png", Vec::new())).is_err(),
            "an empty upload must be rejected before it reaches the server"
        );
    }

    #[test]
    fn upload_sends_the_raw_body_and_the_expected_headers() {
        use std::io::{Read, Write};

        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let body: &[u8] = b"\x89PNG\r\n\x1a\n";
        let body_len = body.len();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut raw: Vec<u8> = Vec::new();
            let mut buf = [0u8; 512];
            let mut seen = 0usize;
            loop {
                let read = stream.read(&mut buf).unwrap();
                if read == 0 {
                    break;
                }
                raw.extend_from_slice(&buf[..read]);
                seen += read;
                let text = String::from_utf8_lossy(&raw);
                if let Some(pos) = text.find("\r\n\r\n") {
                    let head_len = pos + 4;
                    if raw.len() >= head_len + body_len {
                        break;
                    }
                }
                if seen > 65_536 {
                    break;
                }
            }
            let json = "{\"ok\":true,\"data\":{\"id\":\"abc\",\"name\":\"my image.png\",\"content_type\":\"image/png\",\"size\":8,\"url\":\"/api/files/abc\"}}";
            stream
                .write_all(
                    format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                        json.len(),
                        json
                    )
                    .as_bytes(),
                )
                .unwrap();
            stream.flush().unwrap();
            raw
        });

        let api = Api::new().unwrap();
        api.set_base(&format!("http://{addr}"));
        api.set_auth("tok".into(), "csrf-token".into());
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let uploaded = runtime
            .block_on(api.upload_file("my image.png", "image/png", body.to_vec()))
            .expect("upload succeeds");
        assert_eq!(uploaded.url, "/api/files/abc");
        assert_eq!(uploaded.content_type, "image/png");
        assert_eq!(uploaded.name, "my image.png");

        let raw = server.join().unwrap();
        let split = raw
            .windows(4)
            .position(|w| w == b"\r\n\r\n")
            .expect("request has a header terminator")
            + 4;
        let text = String::from_utf8_lossy(&raw[..split]).to_lowercase();
        assert!(
            text.starts_with("post /api/uploads"),
            "wrong route: {}",
            &text[..40.min(text.len())]
        );
        assert!(text.contains("x-file-name: my%20image.png"), "headers: {text}");
        assert!(text.contains("content-type: image/png"), "headers: {text}");
        assert!(text.contains("x-csrf-token: csrf-token"), "headers: {text}");
        assert!(text.contains("cookie: pw_session=tok"), "headers: {text}");
        assert!(
            text.contains(&format!("content-length: {body_len}")),
            "the server requires an exact content-length: {text}"
        );
        assert!(
            raw[split..] == *body,
            "the file must be the raw request body, not multipart: {:?}",
            String::from_utf8_lossy(&raw[split..])
        );
    }
}
