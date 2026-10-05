use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use std::sync::mpsc::Sender;
use tokio::sync::{mpsc, watch};
use tokio::time::{Duration, sleep};
use tokio_tungstenite::tungstenite::Message as WsMessage;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::Uri;
use tokio_tungstenite::tungstenite::http::header::HeaderValue;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream, connect_async};

use crate::api::{Auth, origin, ws_url};
use crate::backend::{Repainter, Update};
use crate::model::{
    CallInvite, Message, Notification, Participant, PresenceSnapshot, RoomId, Scope,
};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Sub {
    pub scope: Scope,
    pub id: i64,
}

pub enum RtCommand {
    Text(String),

    JoinRoom(RoomId),

    LeaveRoom(RoomId),

    PatchRoom(crate::model::RoomPatch),

    Signal {
        room: RoomId,
        to_user_id: i64,
        signal: Value,
    },

    Activity {
        active: bool,
        level_db: i32,
    },
    RingCall {
        conversation_id: i64,
    },
    AcceptCall {
        conversation_id: i64,
    },
    DeclineCall {
        conversation_id: i64,
    },
    CancelCall {
        conversation_id: i64,
    },
}

type Socket = WebSocketStream<MaybeTlsStream<tokio::net::TcpStream>>;

async fn connect(base: &str, auth: &Auth) -> anyhow::Result<Socket> {
    let url = ws_url(base)?;
    let origin = origin(base)?;

    let uri: Uri = url.parse()?;
    let mut request = uri.into_client_request()?;
    let headers = request.headers_mut();
    headers.insert("Origin", HeaderValue::from_str(&origin)?);
    headers.insert(
        "Cookie",
        HeaderValue::from_str(&format!("pw_session={}", auth.token))?,
    );

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

fn event_room(kind: crate::model::RoomKind, value: &Value) -> Option<RoomId> {
    let id = value
        .get("channel_id")
        .or_else(|| value.get("conversation_id"))
        .and_then(Value::as_i64)?;
    if id <= 0 {
        return None;
    }
    Some(RoomId { kind, id })
}

fn parse_participants(value: &Value) -> Vec<Participant> {
    value
        .get("users")
        .and_then(Value::as_array)
        .map(|rows| {
            rows.iter()
                .filter_map(|row| serde_json::from_value::<Participant>(row.clone()).ok())
                .collect()
        })
        .unwrap_or_default()
}

fn profile_user(value: &Value) -> crate::model::User {
    value
        .get("profile")
        .or_else(|| value.get("caller"))
        .or_else(|| value.get("user"))
        .cloned()
        .and_then(|raw| serde_json::from_value(raw).ok())
        .unwrap_or_default()
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
            if let (Some(sub), Some(message)) = (event_sub(&value), parse_message(&value)) {
                let _ = updates.send(Update::ReactionAdded { sub, message });
            }
        }
        "reaction_removed" => {
            if let (Some(sub), Some(message)) = (event_sub(&value), parse_message(&value)) {
                let _ = updates.send(Update::ReactionRemoved { sub, message });
            }
        }

        "voice_state" => {
            if let Some(room) = event_room(crate::model::RoomKind::Voice, &value) {
                let _ = updates.send(Update::RoomRoster {
                    room,
                    participants: parse_participants(&value),
                });
            }
        }
        "voice_peer_joined" => {
            if let Some(room) = event_room(crate::model::RoomKind::Voice, &value) {
                let user_id = value.get("user_id").and_then(Value::as_i64).unwrap_or(0);
                let _ = updates.send(Update::RoomPeerJoined {
                    room,
                    user_id,
                    profile: profile_user(&value),
                });
            }
        }
        "voice_peer_left" => {
            if let Some(room) = event_room(crate::model::RoomKind::Voice, &value) {
                let user_id = value.get("user_id").and_then(Value::as_i64).unwrap_or(0);
                let _ = updates.send(Update::RoomPeerLeft { room, user_id });
            }
        }
        "voice_superseded" => {
            if let Some(room) = event_room(crate::model::RoomKind::Voice, &value) {
                let _ = updates.send(Update::RoomSuperseded { room });
            }
        }
        "voice_activity" => {
            if let Some(room) = event_room(crate::model::RoomKind::Voice, &value) {
                let user_id = value.get("user_id").and_then(Value::as_i64).unwrap_or(0);
                let active = value
                    .get("active")
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                let _ = updates.send(Update::RoomActivity {
                    room,
                    user_id,
                    active,
                });
            }
        }
        "voice_signal" => {
            if let Some(room) = event_room(crate::model::RoomKind::Voice, &value) {
                let from = value
                    .get("from_user_id")
                    .and_then(Value::as_i64)
                    .unwrap_or(0);
                if let Some(signal) = value.get("signal").cloned() {
                    let _ = updates.send(Update::RoomSignal {
                        room,
                        from_user_id: from,
                        signal,
                    });
                }
            }
        }

        "call_state" => {
            if let Some(room) = event_room(crate::model::RoomKind::Call, &value) {
                let _ = updates.send(Update::RoomRoster {
                    room,
                    participants: parse_participants(&value),
                });
            }
        }
        "call_peer_joined" => {
            if let Some(room) = event_room(crate::model::RoomKind::Call, &value) {
                let user_id = value.get("user_id").and_then(Value::as_i64).unwrap_or(0);
                let _ = updates.send(Update::RoomPeerJoined {
                    room,
                    user_id,
                    profile: profile_user(&value),
                });
            }
        }
        "call_peer_left" => {
            if let Some(room) = event_room(crate::model::RoomKind::Call, &value) {
                let user_id = value.get("user_id").and_then(Value::as_i64).unwrap_or(0);
                let _ = updates.send(Update::RoomPeerLeft { room, user_id });
            }
        }
        "call_superseded" => {
            if let Some(room) = event_room(crate::model::RoomKind::Call, &value) {
                let _ = updates.send(Update::RoomSuperseded { room });
            }
        }
        "call_activity" => {
            if let Some(room) = event_room(crate::model::RoomKind::Call, &value) {
                let user_id = value.get("user_id").and_then(Value::as_i64).unwrap_or(0);
                let active = value
                    .get("active")
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                let _ = updates.send(Update::RoomActivity {
                    room,
                    user_id,
                    active,
                });
            }
        }
        "call_signal" => {
            if let Some(room) = event_room(crate::model::RoomKind::Call, &value) {
                let from = value
                    .get("from_user_id")
                    .and_then(Value::as_i64)
                    .unwrap_or(0);
                if let Some(signal) = value.get("signal").cloned() {
                    let _ = updates.send(Update::RoomSignal {
                        room,
                        from_user_id: from,
                        signal,
                    });
                }
            }
        }
        "call_ringing" => {
            if let Some(room) = event_room(crate::model::RoomKind::Call, &value) {
                let _ = updates.send(Update::CallRinging {
                    conversation_id: room.id,
                    profile: profile_user(&value),
                });
            }
        }
        "call_incoming" => {
            if let Some(conversation_id) = value.get("conversation_id").and_then(Value::as_i64) {
                let invite = CallInvite {
                    conversation_id,
                    invite_id: value
                        .get("invite_id")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string(),
                    caller_id: value.get("caller_id").and_then(Value::as_i64).unwrap_or(0),
                    caller_name: profile_user(&value).display_name,
                    caller_username: profile_user(&value).username,
                    avatar_url: profile_user(&value).avatar_url,
                };
                let _ = updates.send(Update::CallIncoming(invite));
            }
        }
        "call_accepted" => {
            if let Some(room) = event_room(crate::model::RoomKind::Call, &value) {
                let _ = updates.send(Update::CallAccepted {
                    conversation_id: room.id,
                });
            }
        }
        "call_declined" => {
            if let Some(room) = event_room(crate::model::RoomKind::Call, &value) {
                let _ = updates.send(Update::CallEnded {
                    conversation_id: room.id,
                    reason: "declined".to_string(),
                });
            }
        }
        "call_cancelled" | "call_missed" => {
            if let Some(room) = event_room(crate::model::RoomKind::Call, &value) {
                let reason = if value.get("type").and_then(Value::as_str) == Some("call_missed") {
                    "missed"
                } else {
                    "cancelled"
                };
                let _ = updates.send(Update::CallEnded {
                    conversation_id: room.id,
                    reason: reason.to_string(),
                });
            }
        }
        "call_ended" => {
            if let Some(room) = event_room(crate::model::RoomKind::Call, &value) {
                let reason = value
                    .get("reason")
                    .and_then(Value::as_str)
                    .unwrap_or("ended")
                    .to_string();
                let _ = updates.send(Update::CallEnded {
                    conversation_id: room.id,
                    reason,
                });
            }
        }

        "notification" => {
            if let Some(event) = value.get("event").cloned() {
                if let Ok(notification) = serde_json::from_value::<Notification>(event) {
                    let _ = updates.send(Update::Notification(notification));
                }
            }
        }
        "presence_online" => {
            let user_id = value.get("user_id").and_then(Value::as_i64).unwrap_or(0);
            let status = value
                .get("status")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            let client_platform = value
                .get("client_platform")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            let _ = updates.send(Update::PresenceOnline {
                user_id,
                status,
                client_platform,
            });
        }
        "presence_offline" => {
            let user_id = value.get("user_id").and_then(Value::as_i64).unwrap_or(0);
            let _ = updates.send(Update::PresenceOffline { user_id });
        }
        "presence_status" => {
            let user_id = value.get("user_id").and_then(Value::as_i64).unwrap_or(0);
            let status = value
                .get("status")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            let client_platform = value
                .get("client_platform")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            let _ = updates.send(Update::PresenceStatus {
                user_id,
                status,
                client_platform,
            });
        }
        "presence_state" => {
            if let Ok(snapshot) = serde_json::from_value::<PresenceSnapshot>(value.clone()) {
                let _ = updates.send(Update::PresenceState(snapshot));
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

fn frame_for(command: &RtCommand) -> Option<String> {
    match command {
        RtCommand::Text(text) => Some(text.clone()),
        RtCommand::JoinRoom(room) => Some(match room.kind {
            crate::model::RoomKind::Voice => {
                json!({ "type": "voice_join", "channel_id": room.id }).to_string()
            }
            crate::model::RoomKind::Call => {
                json!({ "type": "call_join", "conversation_id": room.id }).to_string()
            }
        }),
        RtCommand::LeaveRoom(room) => Some(match room.kind {
            crate::model::RoomKind::Voice => json!({ "type": "voice_leave" }).to_string(),
            crate::model::RoomKind::Call => json!({ "type": "call_leave" }).to_string(),
        }),
        RtCommand::PatchRoom(patch) => Some(match patch.kind {
            crate::model::RoomKind::Voice => {
                json!({ "type": "voice_state", "patch": patch.wire() }).to_string()
            }
            crate::model::RoomKind::Call => {
                json!({ "type": "call_state", "patch": patch.wire() }).to_string()
            }
        }),
        RtCommand::Signal {
            room,
            to_user_id,
            signal,
        } => Some(match room.kind {
            crate::model::RoomKind::Voice => json!({
                "type": "voice_signal",
                "channel_id": room.id,
                "to_user_id": to_user_id,
                "signal": signal,
            })
            .to_string(),
            crate::model::RoomKind::Call => json!({
                "type": "call_signal",
                "conversation_id": room.id,
                "to_user_id": to_user_id,
                "signal": signal,
            })
            .to_string(),
        }),
        RtCommand::Activity { active, level_db } => Some(
            json!({ "type": "voice_activity", "active": active, "level_db": level_db }).to_string(),
        ),
        RtCommand::RingCall { conversation_id } => {
            Some(json!({ "type": "call_ring", "conversation_id": conversation_id }).to_string())
        }
        RtCommand::AcceptCall { conversation_id } => {
            Some(json!({ "type": "call_accept", "conversation_id": conversation_id }).to_string())
        }
        RtCommand::DeclineCall { conversation_id } => {
            Some(json!({ "type": "call_decline", "conversation_id": conversation_id }).to_string())
        }
        RtCommand::CancelCall { conversation_id } => {
            Some(json!({ "type": "call_cancel", "conversation_id": conversation_id }).to_string())
        }
    }
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
                    let _ = write
                        .send(WsMessage::Text(subscribe_text(sub).into()))
                        .await;
                }
                loop {
                    tokio::select! {
                        incoming = inbox.recv() => {
                            match incoming {
                                Some(command) => {
                                    if let Some(frame) = frame_for(&command) {
                                        let _ = write.send(WsMessage::Text(frame.into())).await;
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
#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::Update;
    use crate::model::{RoomKind, Scope};

    #[test]
    fn handshake_request_carries_every_upgrade_header() {
        let url = crate::api::ws_url("https://plainwi.re").unwrap();
        let uri: Uri = url.parse().unwrap();
        let request = uri.into_client_request().unwrap();

        let required = [
            "host",
            "connection",
            "upgrade",
            "sec-websocket-version",
            "sec-websocket-key",
        ];
        for header in required {
            assert_eq!(
                request.headers().get_all(header).iter().count(),
                1,
                "{header} must appear exactly once"
            );
        }
        assert_eq!(request.headers().get("connection").unwrap(), "Upgrade");
        assert_eq!(request.headers().get("upgrade").unwrap(), "websocket");
        assert_eq!(
            request.headers().get("sec-websocket-version").unwrap(),
            "13"
        );
        assert!(
            !request
                .headers()
                .get("sec-websocket-key")
                .unwrap()
                .is_empty()
        );
        assert_eq!(request.uri().path(), "/ws");
    }

    #[test]
    fn handshake_request_carries_auth_and_origin() {
        let url = crate::api::ws_url("https://plainwi.re").unwrap();
        let origin = crate::api::origin("https://plainwi.re").unwrap();
        let auth = crate::api::Auth {
            token: "tok".into(),
            csrf: "csrf".into(),
        };

        let uri: Uri = url.parse().unwrap();
        let mut request = uri.into_client_request().unwrap();
        let headers = request.headers_mut();
        headers.insert("Origin", HeaderValue::from_str(&origin).unwrap());
        headers.insert(
            "Cookie",
            HeaderValue::from_str(&format!("pw_session={}", auth.token)).unwrap(),
        );

        assert_eq!(headers.get("origin").unwrap(), "https://plainwi.re");
        assert_eq!(headers.get("cookie").unwrap(), "pw_session=tok");

        for header in ["host", "connection", "upgrade", "sec-websocket-key"] {
            assert_eq!(request.headers().get_all(header).iter().count(), 1);
        }
    }

    fn json(frame: &str) -> serde_json::Value {
        serde_json::from_str(frame).expect("frame is valid json")
    }

    fn variant(update: &Update) -> &'static str {
        match update {
            Update::Session(_) => "Session",
            Update::SignedIn { .. } => "SignedIn",
            Update::Sync(_) => "Sync",
            Update::Channels { .. } => "Channels",
            Update::Messages { .. } => "Messages",
            Update::MessageUpsert { .. } => "MessageUpsert",
            Update::MessageDeleted { .. } => "MessageDeleted",
            Update::ReactionAdded { .. } => "ReactionAdded",
            Update::ReactionRemoved { .. } => "ReactionRemoved",
            Update::UserSearch { .. } => "UserSearch",
            Update::ProfileView { .. } => "ProfileView",
            Update::Avatar { .. } => "Avatar",
            Update::AvatarFailed { .. } => "AvatarFailed",
            Update::Uploaded { .. } => "Uploaded",
            Update::UploadFailed { .. } => "UploadFailed",
            Update::RtcConfig(_) => "RtcConfig",
            Update::RoomRoster { .. } => "RoomRoster",
            Update::RoomPeerJoined { .. } => "RoomPeerJoined",
            Update::RoomPeerLeft { .. } => "RoomPeerLeft",
            Update::RoomSuperseded { .. } => "RoomSuperseded",
            Update::RoomActivity { .. } => "RoomActivity",
            Update::RoomSignal { .. } => "RoomSignal",
            Update::CallRinging { .. } => "CallRinging",
            Update::CallIncoming(_) => "CallIncoming",
            Update::CallAccepted { .. } => "CallAccepted",
            Update::CallEnded { .. } => "CallEnded",
            Update::Typing { .. } => "Typing",
            Update::Notification(_) => "Notification",
            Update::PresenceOnline { .. } => "PresenceOnline",
            Update::PresenceOffline { .. } => "PresenceOffline",
            Update::PresenceStatus { .. } => "PresenceStatus",
            Update::PresenceState(_) => "PresenceState",
            Update::Realtime(_) => "Realtime",
            Update::LoggedOut => "LoggedOut",
            Update::Error(_) => "Error",
            Update::Status(_) => "Status",
        }
    }

    fn drain() -> (Sender<Update>, std::sync::mpsc::Receiver<Update>) {
        let (tx, rx) = std::sync::mpsc::channel();
        (tx, rx)
    }

    #[test]
    fn outbound_frames_match_the_server_protocol() {
        let room = RoomId::voice(12);
        let frame = |c: &RtCommand| frame_for(c).unwrap();

        assert_eq!(
            json(&frame(&RtCommand::JoinRoom(room))),
            json(r#"{"type":"voice_join","channel_id":12}"#)
        );
        assert_eq!(
            json(&frame(&RtCommand::JoinRoom(RoomId::call(3)))),
            json(r#"{"type":"call_join","conversation_id":3}"#)
        );
        assert_eq!(
            json(&frame(&RtCommand::LeaveRoom(RoomId::voice(12)))),
            json(r#"{"type":"voice_leave"}"#)
        );
        assert_eq!(
            json(&frame(&RtCommand::LeaveRoom(RoomId::call(3)))),
            json(r#"{"type":"call_leave"}"#)
        );
        assert_eq!(
            json(&frame(&RtCommand::RingCall { conversation_id: 3 })),
            json(r#"{"type":"call_ring","conversation_id":3}"#)
        );
        assert_eq!(
            json(&frame(&RtCommand::AcceptCall { conversation_id: 3 })),
            json(r#"{"type":"call_accept","conversation_id":3}"#)
        );
        assert_eq!(
            json(&frame(&RtCommand::DeclineCall { conversation_id: 3 })),
            json(r#"{"type":"call_decline","conversation_id":3}"#)
        );
        assert_eq!(
            json(&frame(&RtCommand::CancelCall { conversation_id: 3 })),
            json(r#"{"type":"call_cancel","conversation_id":3}"#)
        );
        assert_eq!(
            json(&frame(&RtCommand::Activity {
                active: true,
                level_db: -20
            })),
            json(r#"{"type":"voice_activity","active":true,"level_db":-20}"#)
        );
    }

    #[test]
    fn patches_are_routed_to_the_right_frame() {
        let voice = RtCommand::PatchRoom(crate::model::RoomPatch::new(RoomKind::Voice).muted(true));
        assert_eq!(
            json(&frame_for(&voice).unwrap()),
            json(r#"{"type":"voice_state","patch":{"muted":true}}"#)
        );
        let call = RtCommand::PatchRoom(crate::model::RoomPatch::new(RoomKind::Call).screen(true));
        assert_eq!(
            json(&frame_for(&call).unwrap()),
            json(r#"{"type":"call_state","patch":{"screen":true}}"#)
        );
    }

    #[test]
    fn signal_payloads_satisfy_pw_ws_signal_ok() {
        let signal = json!({"kind":"offer","sdp":{"type":"offer","sdp":"v=0\r\n"}});
        let frame = frame_for(&RtCommand::Signal {
            room: RoomId::voice(4),
            to_user_id: 9,
            signal,
        })
        .unwrap();
        assert_eq!(
            json(&frame),
            json(
                r#"{"type":"voice_signal","channel_id":4,"to_user_id":9,
                    "signal":{"kind":"offer","sdp":{"type":"offer","sdp":"v=0\r\n"}}}"#
            )
        );

        let renegotiate = frame_for(&RtCommand::Signal {
            room: RoomId::call(1),
            to_user_id: 2,
            signal: json!({"kind":"renegotiate"}),
        })
        .unwrap();
        assert_eq!(
            json(&renegotiate),
            json(
                r#"{"type":"call_signal","conversation_id":1,"to_user_id":2,
                    "signal":{"kind":"renegotiate"}}"#
            )
        );
    }

    #[test]
    fn voice_state_becomes_a_roster() {
        let (tx, rx) = drain();
        handle_text(
            r#"{"type":"voice_state","channel_id":12,"users":[
                {"user_id":9,"muted":true,"deafened":false,"screen":false,
                 "screen_audio":false,"reconnecting":false,
                 "profile":{"id":9,"username":"ada","display_name":"Ada"}}]}"#,
            &tx,
            &Repainter::new(),
        );
        match rx.recv().unwrap() {
            Update::RoomRoster { room, participants } => {
                assert_eq!(room, RoomId::voice(12));
                assert_eq!(participants.len(), 1);
                assert!(participants[0].muted);
            }
            other => panic!("expected a roster, got {}", variant(&other)),
        }
    }

    #[test]
    fn peer_join_and_leave_are_reported() {
        let (tx, rx) = drain();
        handle_text(
            r#"{"type":"voice_peer_joined","channel_id":12,"user_id":9,
                "profile":{"id":9,"username":"ada","display_name":"Ada"}}"#,
            &tx,
            &Repainter::new(),
        );
        match rx.recv().unwrap() {
            Update::RoomPeerJoined {
                room,
                user_id,
                profile,
            } => {
                assert_eq!(room, RoomId::voice(12));
                assert_eq!(user_id, 9);
                assert_eq!(profile.label(), "Ada");
            }
            other => panic!("expected peer joined, got {}", variant(&other)),
        }

        handle_text(
            r#"{"type":"call_peer_left","conversation_id":3,"user_id":9}"#,
            &tx,
            &Repainter::new(),
        );
        match rx.recv().unwrap() {
            Update::RoomPeerLeft { room, user_id } => {
                assert_eq!(room, RoomId::call(3));
                assert_eq!(user_id, 9);
            }
            other => panic!("expected peer left, got {}", variant(&other)),
        }
    }

    #[test]
    fn relayed_signals_carry_their_source() {
        let (tx, rx) = drain();
        handle_text(
            r#"{"type":"voice_signal","channel_id":12,"from_user_id":9,
                "signal":{"kind":"offer","sdp":{"type":"offer","sdp":"v=0"}}}"#,
            &tx,
            &Repainter::new(),
        );
        match rx.recv().unwrap() {
            Update::RoomSignal {
                room,
                from_user_id,
                signal,
            } => {
                assert_eq!(room, RoomId::voice(12));
                assert_eq!(from_user_id, 9);
                assert_eq!(signal["kind"], "offer");
            }
            other => panic!("expected a signal, got {}", variant(&other)),
        }
    }

    #[test]
    fn incoming_call_invites_are_decoded() {
        let (tx, rx) = drain();
        handle_text(
            r#"{"type":"call_incoming","conversation_id":3,"invite_id":"tok",
                "caller_id":9,"profile":{"id":9,"username":"ada","display_name":"Ada"}}"#,
            &tx,
            &Repainter::new(),
        );
        match rx.recv().unwrap() {
            Update::CallIncoming(invite) => {
                assert_eq!(invite.conversation_id, 3);
                assert_eq!(invite.invite_id, "tok");
                assert_eq!(invite.caller_id, 9);
                assert_eq!(invite.caller_name, "Ada");
            }
            other => panic!("expected an invite, got {}", variant(&other)),
        }
    }

    #[test]
    fn chat_traffic_is_unaffected() {
        let (tx, rx) = drain();
        handle_text(
            r#"{"type":"typing","scope":"channel","scope_id":12,
                "user_id":9,"display_name":"Ada","active":true}"#,
            &tx,
            &Repainter::new(),
        );
        match rx.recv().unwrap() {
            Update::Typing { sub, name, .. } => {
                assert_eq!(
                    sub,
                    Sub {
                        scope: Scope::Channel,
                        id: 12
                    }
                );
                assert_eq!(name, "Ada");
            }
            other => panic!("expected typing, got {}", variant(&other)),
        }
    }
}
