use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};

use serde_json::Value;
use tokio::sync::{mpsc, watch};

use serde_json::json;

use crate::api::Api;
use crate::model::{
    CallInvite, Message, Participant, Profile, ProfileResponse, RoomId, RoomKind, RtcConfig,
    ServerDetail, Session, SyncPayload, User,
};
use crate::realtime::Sub;
use crate::realtime::{self, RtCommand};

#[derive(Clone, Default)]
pub struct Repainter(Arc<Mutex<Option<eframe::egui::Context>>>);

impl Repainter {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn set(&self, ctx: eframe::egui::Context) {
        *self.0.lock().unwrap() = Some(ctx);
    }

    pub fn request(&self) {
        if let Some(ctx) = self.0.lock().unwrap().as_ref() {
            ctx.request_repaint();
        }
    }

    pub fn load_texture(
        &self,
        name: String,
        image: eframe::egui::ColorImage,
    ) -> Option<eframe::egui::TextureHandle> {
        let ctx = self.0.lock().unwrap().clone()?;
        Some(ctx.load_texture(name, image, eframe::egui::TextureOptions::LINEAR))
    }
}

pub enum Command {
    Login {
        base: String,
        username: String,
        password: String,
    },

    Resume {
        base: String,
        token: String,
        csrf: String,
    },
    Logout,
    LoadServer(i64),
    OpenSub(Sub),
    LoadOlder {
        sub: Sub,
        before: i64,
    },
    Send {
        sub: Sub,
        body: String,
    },
    MarkRead {
        conversation_id: i64,
    },
    SearchUsers {
        query: String,
    },
    ProfileByUserId {
        user_id: i64,
    },
    ProfileByUsername {
        username: String,
    },

    FetchAvatar {
        key: String,
        url: String,
    },
    ToggleReaction {
        message_id: i64,
        emoji: String,
    },
    Typing(Sub),

    VoiceJoin {
        channel_id: i64,
    },

    LeaveRoom(RoomKind),
    CallRing {
        conversation_id: i64,
    },
    CallAccept {
        conversation_id: i64,
    },
    CallJoin {
        conversation_id: i64,
    },
    CallDecline {
        conversation_id: i64,
    },
    CallCancel {
        conversation_id: i64,
    },

    PatchRoom {
        patch: crate::model::RoomPatch,
    },

    Signal {
        room: RoomId,
        to_user_id: i64,
        signal: Value,
    },
    Activity {
        active: bool,
        level_db: i32,
    },

    UploadBytes {
        name: String,
        content_type: String,
        bytes: Vec<u8>,
    },

    UploadPath {
        path: std::path::PathBuf,
    },

    Status(String),
}

#[derive(Clone)]
pub enum Update {
    Session(Session),

    SignedIn {
        user: User,
        token: String,
        csrf: String,
    },
    Sync(SyncPayload),
    Channels {
        server_id: i64,
        detail: ServerDetail,
    },
    Messages {
        sub: Sub,
        messages: Vec<Message>,
        older: bool,
    },
    MessageUpsert {
        sub: Sub,
        message: Message,
    },
    MessageDeleted {
        sub: Sub,
        message_id: i64,
    },
    ReactionAdded {
        sub: Sub,
        message: Message,
    },
    ReactionRemoved {
        sub: Sub,
        message: Message,
    },
    UserSearch {
        query: String,
        users: Vec<User>,
    },
    ProfileView {
        profile: Profile,
    },

    Avatar {
        key: String,
        image: image::RgbaImage,
    },
    AvatarFailed {
        key: String,
    },

    Uploaded {
        name: String,
        content_type: String,
        url: String,
    },
    UploadFailed {
        name: String,
        reason: String,
    },
    RtcConfig(RtcConfig),

    RoomRoster {
        room: RoomId,
        participants: Vec<Participant>,
    },
    RoomPeerJoined {
        room: RoomId,
        user_id: i64,
        profile: User,
    },
    RoomPeerLeft {
        room: RoomId,
        user_id: i64,
    },
    RoomSuperseded {
        room: RoomId,
    },
    RoomActivity {
        room: RoomId,
        user_id: i64,
        active: bool,
    },
    RoomSignal {
        room: RoomId,
        from_user_id: i64,
        signal: Value,
    },
    CallRinging {
        conversation_id: i64,
        profile: User,
    },
    CallIncoming(CallInvite),
    CallAccepted {
        conversation_id: i64,
    },
    CallEnded {
        conversation_id: i64,
        reason: String,
    },
    Typing {
        sub: Sub,
        user_id: i64,
        name: String,
        active: bool,
    },
    Realtime(bool),
    LoggedOut,
    Error(String),
    Status(String),
}

#[derive(Clone)]
pub struct Backend {
    commands: mpsc::UnboundedSender<Command>,
}

impl Backend {
    pub fn new(commands: mpsc::UnboundedSender<Command>) -> Self {
        Self { commands }
    }

    pub fn send(&self, command: Command) {
        let _ = self.commands.send(command);
    }
}

async fn open_sub(api: &Api, updates: &Sender<Update>, subs: &Option<watch::Sender<Option<Sub>>>, sub: Sub) {
    if let Some(tx) = subs {
        let _ = tx.send(Some(sub));
    }
    match api.messages(sub.scope, sub.id, None).await {
        Ok(mut messages) => {
            messages.sort_by_key(|m| m.id);
            let _ = updates.send(Update::Messages {
                sub,
                messages,
                older: false,
            });
        }
        Err(error) => {
            let _ = updates.send(Update::Error(error.to_string()));
        }
    }
}

fn to_profile(response: ProfileResponse) -> Profile {
    Profile {
        user: response.user,
        relationship: response.relationship,
        servers: Vec::new(),
        conversations: Vec::new(),
    }
}

pub async fn run(
    mut commands: mpsc::UnboundedReceiver<Command>,
    updates: Sender<Update>,
    repaint: Repainter,
) {
    let api = match Api::new() {
        Ok(api) => api,
        Err(error) => {
            let _ = updates.send(Update::Error(error.to_string()));
            return;
        }
    };
    let mut outbox: Option<mpsc::UnboundedSender<RtCommand>> = None;
    let mut subs: Option<watch::Sender<Option<Sub>>> = None;

    while let Some(command) = commands.recv().await {
        match command {
            Command::Login {
                base,
                username,
                password,
            } => {
                match api.login(&base, &username, &password).await {
                    Ok(session) => {
                        let _ = updates.send(Update::Session(session.clone()));
                        if let Some(auth) = api.auth() {
                            let server = api.base();
                            let (out_tx, out_rx) = mpsc::unbounded_channel();
                            let (sub_tx, sub_rx) = watch::channel(None);
                            let relay = updates.clone();
                            let wake = repaint.clone();
                            tokio::spawn(realtime::run(server, auth, out_rx, sub_rx, relay, wake));
                            outbox = Some(out_tx);
                            subs = Some(sub_tx);
                        }
                        if let Some(auth) = api.auth() {
                            let _ = updates.send(Update::SignedIn {
                                user: session.user,
                                token: auth.token,
                                csrf: auth.csrf,
                            });
                        }

                        if let Ok(config) = api.rtc_config().await {
                            let _ = updates.send(Update::RtcConfig(config));
                        }
                        match api.sync().await {
                            Ok(sync) => {
                                let _ = updates.send(Update::Sync(sync));
                            }
                            Err(error) => {
                                let _ = updates.send(Update::Error(error.to_string()));
                            }
                        }
                    }
                    Err(error) => {
                        let _ = updates.send(Update::Error(error.to_string()));
                    }
                }
                repaint.request();
            }
            Command::Resume { base, token, csrf } => {
                let normalized = crate::api::normalize_base(&base);
                api.set_base(&normalized);
                api.set_auth(token.clone(), csrf.clone());

                match api.session().await {
                    Ok(session) => {
                        let csrf = if session.csrf.is_empty() {
                            csrf
                        } else {
                            session.csrf
                        };
                        api.set_auth(token.clone(), csrf.clone());
                        if let Some(auth) = api.auth() {
                            let server = api.base();
                            let (out_tx, out_rx) = mpsc::unbounded_channel();
                            let (sub_tx, sub_rx) = watch::channel(None);
                            let relay = updates.clone();
                            let wake = repaint.clone();
                            tokio::spawn(realtime::run(server, auth, out_rx, sub_rx, relay, wake));
                            outbox = Some(out_tx);
                            subs = Some(sub_tx);
                        }
                        let _ = updates.send(Update::SignedIn {
                            user: session.user,
                            token,
                            csrf,
                        });
                        if let Ok(config) = api.rtc_config().await {
                            let _ = updates.send(Update::RtcConfig(config));
                        }
                        match api.sync().await {
                            Ok(sync) => {
                                let _ = updates.send(Update::Sync(sync));
                            }
                            Err(error) => {
                                let _ = updates.send(Update::Error(error.to_string()));
                            }
                        }
                    }
                    Err(error) => {
                        api.clear();
                        let _ = updates.send(Update::Error(format!(
                            "could not resume {}: {error}",
                            normalized
                        )));
                    }
                }
                repaint.request();
            }
            Command::LoadServer(server_id) => {
                match api.server_detail(server_id).await {
                    Ok(detail) => {
                        let _ = updates.send(Update::Channels {
                            server_id,
                            detail,
                        });
                    }
                    Err(error) => {
                        let _ = updates.send(Update::Error(error.to_string()));
                    }
                }
                repaint.request();
            }
            Command::OpenSub(sub) => {
                if sub.scope == crate::model::Scope::Direct {
                    let _ = api.mark_conversation_read(sub.id).await;
                }
                open_sub(&api, &updates, &subs, sub).await;
                repaint.request();
            }
            Command::LoadOlder { sub, before } => {
                match api.messages(sub.scope, sub.id, Some(before)).await {
                    Ok(mut messages) => {
                        messages.sort_by_key(|m| m.id);
                        let _ = updates.send(Update::Messages {
                            sub,
                            messages,
                            older: true,
                        });
                    }
                    Err(error) => {
                        let _ = updates.send(Update::Error(error.to_string()));
                    }
                }
                repaint.request();
            }
            Command::Send { sub, body } => {
                match api.send(sub.scope, sub.id, &body).await {
                    Ok(message) => {
                        let _ = updates.send(Update::MessageUpsert { sub, message });
                    }
                    Err(error) => {
                        let _ = updates.send(Update::Error(error.to_string()));
                    }
                }
                repaint.request();
            }
            Command::MarkRead { conversation_id } => {
                let _ = api.mark_conversation_read(conversation_id).await;
                repaint.request();
            }
            Command::SearchUsers { query } => {
                match api.search_users(&query).await {
                    Ok(users) => {
                        let _ = updates.send(Update::UserSearch { query, users });
                    }
                    Err(error) => {
                        let _ = updates.send(Update::Error(error.to_string()));
                    }
                }
                repaint.request();
            }
            Command::ProfileByUserId { user_id } => {
                match api.profile_by_user_id(user_id).await {
                    Ok(response) => {
                        let _ = updates.send(Update::ProfileView {
                            profile: to_profile(response),
                        });
                    }
                    Err(error) => {
                        let _ = updates.send(Update::Error(error.to_string()));
                    }
                }
                repaint.request();
            }
            Command::ProfileByUsername { username } => {
                match api.profile_by_username(&username).await {
                    Ok(response) => {
                        let _ = updates.send(Update::ProfileView {
                            profile: to_profile(response),
                        });
                    }
                    Err(error) => {
                        let _ = updates.send(Update::Error(error.to_string()));
                    }
                }
                repaint.request();
            }
            Command::FetchAvatar { key, url } => {
                match api.fetch_image(&url).await {
                    Ok(bytes) => match crate::api::decode_image(&bytes) {
                        Ok(image) => {
                            let _ = updates.send(Update::Avatar { key, image });
                        }
                        Err(error) => {
                            let _ = updates.send(Update::AvatarFailed { key });
                            let _ = updates.send(Update::Error(error.to_string()));
                        }
                    },
                    Err(error) => {
                        let _ = updates.send(Update::AvatarFailed { key });
                        let _ = updates.send(Update::Status(format!(
                            "could not load avatar: {error}"
                        )));
                    }
                }
                repaint.request();
            }
            Command::UploadBytes {
                name,
                content_type,
                bytes,
            } => {
                match api.upload_file(&name, &content_type, bytes).await {
                    Ok(uploaded) => {
                        let _ = updates.send(Update::Uploaded {
                            name: uploaded.name,
                            content_type: uploaded.content_type,
                            url: uploaded.url,
                        });
                    }
                    Err(error) => {
                        let _ = updates.send(Update::UploadFailed {
                            name,
                            reason: error.to_string(),
                        });
                    }
                }
                repaint.request();
            }
            Command::UploadPath { path } => {
                let name = path
                    .file_name()
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or_else(|| "file".to_string());
                let content_type = crate::api::guess_content_type(&path);
                let read_path = path.clone();
                let read = tokio::task::spawn_blocking(move || std::fs::read(read_path)).await;
                let bytes = match read {
                    Ok(Ok(bytes)) => Some(bytes),
                    Ok(Err(error)) => {
                        let _ = updates.send(Update::UploadFailed {
                            name: name.clone(),
                            reason: error.to_string(),
                        });
                        None
                    }
                    Err(error) => {
                        let _ = updates.send(Update::UploadFailed {
                            name: name.clone(),
                            reason: error.to_string(),
                        });
                        None
                    }
                };
                if let Some(bytes) = bytes {
                    match api.upload_file(&name, content_type, bytes).await {
                        Ok(uploaded) => {
                            let _ = updates.send(Update::Uploaded {
                                name: uploaded.name,
                                content_type: uploaded.content_type,
                                url: uploaded.url,
                            });
                        }
                        Err(error) => {
                            let _ = updates.send(Update::UploadFailed {
                                name,
                                reason: error.to_string(),
                            });
                        }
                    }
                }
                repaint.request();
            }
            Command::ToggleReaction { message_id, emoji } => {
                if let Some(out) = &outbox {
                    let _ = out.send(RtCommand::Text(
                        json!({
                            "type": "toggle_reaction",
                            "message_id": message_id,
                            "emoji": emoji,
                        })
                        .to_string(),
                    ));
                }
                repaint.request();
            }
            Command::Typing(sub) => {
                if let Some(out) = &outbox {
                    let frame = json!({
                        "type": "typing",
                        "scope": sub.scope.as_str(),
                        "scope_id": sub.id,
                        "active": true,
                    })
                    .to_string();
                    let _ = out.send(RtCommand::Text(frame));
                }
            }
            Command::VoiceJoin { channel_id } => {
                if let Some(out) = &outbox {
                    let _ = out.send(RtCommand::JoinRoom(RoomId::voice(channel_id)));
                }
                repaint.request();
            }
            Command::LeaveRoom(kind) => {

                let room = RoomId {
                    kind,
                    id: 0,
                };
                if let Some(out) = &outbox {
                    let _ = out.send(RtCommand::LeaveRoom(room));
                }
                repaint.request();
            }
            Command::CallRing { conversation_id } => {
                if let Some(out) = &outbox {
                    let _ = out.send(RtCommand::RingCall { conversation_id });
                }
                repaint.request();
            }
            Command::CallAccept { conversation_id } => {
                if let Some(out) = &outbox {
                    let _ = out.send(RtCommand::AcceptCall { conversation_id });
                }
                repaint.request();
            }
            Command::CallJoin { conversation_id } => {
                if let Some(out) = &outbox {
                    let _ = out.send(RtCommand::JoinRoom(RoomId::call(conversation_id)));
                }
                repaint.request();
            }
            Command::CallDecline { conversation_id } => {
                if let Some(out) = &outbox {
                    let _ = out.send(RtCommand::DeclineCall { conversation_id });
                }
                repaint.request();
            }
            Command::CallCancel { conversation_id } => {
                if let Some(out) = &outbox {
                    let _ = out.send(RtCommand::CancelCall { conversation_id });
                }
                repaint.request();
            }
            Command::PatchRoom { patch } => {
                if let Some(out) = &outbox {
                    let _ = out.send(RtCommand::PatchRoom(patch));
                }
                repaint.request();
            }
            Command::Signal {
                room,
                to_user_id,
                signal,
            } => {
                if let Some(out) = &outbox {
                    let _ = out.send(RtCommand::Signal {
                        room,
                        to_user_id,
                        signal,
                    });
                }
            }
            Command::Activity { active, level_db } => {
                if let Some(out) = &outbox {
                    let _ = out.send(RtCommand::Activity { active, level_db });
                }
            }
            Command::Status(message) => {
                let _ = updates.send(Update::Status(message));
                repaint.request();
            }
            Command::Logout => {
                let _ = api.logout().await;
                api.clear();
                let _ = subs.as_ref().map(|tx| tx.send(None));
                outbox = None;
                subs = None;
                let _ = updates.send(Update::LoggedOut);
                repaint.request();
            }
        }
    }
}