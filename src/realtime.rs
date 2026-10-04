use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use std::sync::mpsc::Sender;
use tokio::sync::{mpsc, watch};
use tokio::time::{Duration, sleep};
use tokio_tungstenite::tungstenite::Message as WsMessage;
use tokio_tungstenite::tungstenite::http::Request;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream, connect_async};

use crate::api::{Auth, origin, ws_url};
use crate::backend::{Repainter, Update};
use crate::model::{Message, Scope};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Sub {
    pub scope: Scope,
    pub id: i64,
}

pub enum RtCommand {
    Text(String),
    ReceiveImage {
        message_id: i64,
        image_url: String,
        content_type: String,
    },
    ToggleReaction {
        message_id: i64,
        emoji: String,
    },
}

type Socket = WebSocketStream<MaybeTlsStream<tokio::net::TcpStream>>;

async fn connect(base: &str, auth: &Auth) -> anyhow::Result<Socket> {
    let url = ws_url(base)?;
    let origin = origin(base)?;
    let parsed = url::Url::parse(&url)?;
    let host = parsed.host_str().unwrap_or_default().to_string();
    let authority = match parsed.port() {
        Some(port) => format!("{host}:{port}"),
        None => host,
    };
    let request = Request::builder()
        .uri(&url)
        .header("Host", authority)
        .header("Origin", origin)
        .header("Cookie", format!("pw_session={}", auth.token))
        .body(())?;
    let (stream, _) = connect_async(request).await?;
    Ok(stream)
}

fn sub_key(sub: Sub) -> String {
    format!("{}:{}", sub.scope.as_str(), sub.id)
}

fn subscribe_text(sub: Sub) -> String {
    json!({ "type": "subscribe", "key": sub_key(sub) }).to_string()
}

fn event_sub(value: &Value) -> Option<Sub> {
    let scope = match value.get("scope").and_then(Value::as_str) {
        Some("channel") => Scope::Channel,
        Some("direct") => Scope::Direct,
        _ => return None,
    };
    let id = value.get("scope_id").and_then(Value::as_i64)?;
    Some(Sub { scope, id })
}

fn parse_message(value: &Value) -> Option<Message> {
    value
        .get("message")
        .cloned()
        .and_then(|raw| serde_json::from_value(raw).ok())
}

fn handle_text(text: &str, updates: &Sender<Update>, repaint: &Repainter) {
    let Ok(value) = serde_json::from_str::<Value>(text) else {
        return;
    };
    match value.get("type").and_then(Value::as_str).unwrap_or("") {
        "message_created" | "message_updated" => {
            if let (Some(sub), Some(message)) = (event_sub(&value), parse_message(&value)) {
                let _ = updates.send(Update::MessageUpsert { sub, message });
            }
        }
        "message_deleted" => {
            if let Some(sub) = event_sub(&value) {
                let id = value
                    .get("message_id")
                    .or_else(|| value.get("id"))
                    .and_then(Value::as_i64);
                if let Some(message_id) = id {
                    let _ = updates.send(Update::MessageDeleted { sub, message_id });
                }
            }
        }
        "typing" => {
            if let Some(sub) = event_sub(&value) {
                let name = value
                    .get("display_name")
                    .and_then(Value::as_str)
                    .filter(|s| !s.is_empty())
                    .or_else(|| value.get("username").and_then(Value::as_str))
                    .unwrap_or("Someone")
                    .to_string();
                let active = value.get("active").and_then(Value::as_bool).unwrap_or(true);
                let user_id = value.get("user_id").and_then(Value::as_i64).unwrap_or(0);
                let _ = updates.send(Update::Typing {
                    sub,
                    user_id,
                    name,
                    active,
                });
            }
        }
        "reaction_added" => {
            if let Some(sub) = event_sub(&value) {
                if let Some(message) = parse_message(&value) {
                    let _ = updates.send(Update::ReactionAdded { sub, message });
                }
            }
        }
        "reaction_removed" => {
            if let Some(sub) = event_sub(&value) {
                if let Some(message) = parse_message(&value) {
                    let _ = updates.send(Update::ReactionRemoved { sub, message });
                }
            }
        }
        "error" => {
            let message = value
                .get("error")
                .and_then(Value::as_str)
                .unwrap_or("realtime error")
                .to_string();
            let _ = updates.send(Update::Error(message));
        }
        _ => {}
    }
    repaint.request();
}

pub async fn run(
    base: String,
    auth: Auth,
    mut inbox: mpsc::UnboundedReceiver<RtCommand>,
    mut subs: watch::Receiver<Option<Sub>>,
    updates: Sender<Update>,
    repaint: Repainter,
) {
    let mut backoff = 500u64;
    loop {
        match connect(&base, &auth).await {
            Ok(socket) => {
                backoff = 500;
                let _ = updates.send(Update::Realtime(true));
                repaint.request();
                let (mut write, mut read) = socket.split();
                let initial = *subs.borrow();
                if let Some(sub) = initial {
                    let _ = write.send(WsMessage::Text(subscribe_text(sub).into())).await;
                }
                loop {
                    tokio::select! {
                        incoming = inbox.recv() => {
                            match incoming {
                                Some(RtCommand::Text(text)) => {
                                    if write.send(WsMessage::Text(text.into())).await.is_err() {
                                        break;
                                    }
                                }
                                Some(RtCommand::ReceiveImage { message_id, image_url, content_type }) => {
                                    let frame = json!({
                                        "type": "receive_image",
                                        "message_id": message_id,
                                        "url": image_url,
                                        "content_type": content_type,
                                    }).to_string();
                                    if write.send(WsMessage::Text(frame.into())).await.is_err() {
                                        break;
                                    }
                                }
                                Some(RtCommand::ToggleReaction { message_id, emoji }) => {
                                    let frame = json!({
                                        "type": "toggle_reaction",
                                        "message_id": message_id,
                                        "emoji": emoji,
                                    }).to_string();
                                    if write.send(WsMessage::Text(frame.into())).await.is_err() {
                                        break;
                                    }
                                }
                                None => return,
                            }
                        }
                        changed = subs.changed() => {
                            if changed.is_err() {
                                return;
                            }
                            let current = {
                                let value = subs.borrow();
                                *value
                            };
                            if let Some(sub) = current {
                                let _ = write.send(WsMessage::Text(subscribe_text(sub).into())).await;
                            }
                        }
                        frame = read.next() => {
                            match frame {
                                Some(Ok(WsMessage::Text(text))) => handle_text(text.as_str(), &updates, &repaint),
                                Some(Ok(WsMessage::Ping(payload))) => {
                                    let _ = write.send(WsMessage::Pong(payload)).await;
                                }
                                Some(Ok(WsMessage::Close(_))) | None => break,
                                Some(Err(_)) => break,
                                _ => {}
                            }
                        }
                    }
                }
            }
            Err(error) => {
                let _ = updates.send(Update::Status(format!("realtime offline: {error}")));
                repaint.request();
            }
        }
        let _ = updates.send(Update::Realtime(false));
        repaint.request();
        sleep(Duration::from_millis(backoff)).await;
        backoff = (backoff * 2).min(15_000);
    }
}
