use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};

use tokio::sync::{mpsc, watch};

use serde_json::json;

use crate::api::Api;
use crate::model::{Channel, Message, Profile, Session, SyncPayload};
use crate::realtime::{self, RtCommand, Sub};

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
}

pub enum Command {
    Login {
        base: String,
        username: String,
        password: String,
    },
    ListProfiles,
    ProfileByUserId {
        user_id: i64,
    },
    ProfileByUsername {
        username: String,
    },
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
    SendAttachment {
        sub: Sub,
        message_id: i64,
        image_url: String,
        content_type: String,
    },
    ToggleReaction {
        sub: Sub,
        message_id: i64,
        emoji: String,
    },
    Typing(Sub),
    Logout,
}

#[derive(Clone)]
pub enum Update {
    Session(Session),
    Sync(SyncPayload),
    Channels {
        server_id: i64,
        channels: Vec<Channel>,
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
    Profiles {
        profiles: Vec<Profile>,
    },
    ProfileView {
        profile: Profile,
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

async fn open_sub(
    api: &Api,
    updates: &Sender<Update>,
    subs: &Option<watch::Sender<Option<Sub>>>,
    sub: Sub,
) {
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
                        let _ = updates.send(Update::Session(session));
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
            Command::LoadServer(server_id) => {
                match api.server_detail(server_id).await {
                    Ok(detail) => {
                        let _ = updates.send(Update::Channels {
                            server_id,
                            channels: detail.channels,
                        });
                    }
                    Err(error) => {
                        let _ = updates.send(Update::Error(error.to_string()));
                    }
                }
                repaint.request();
            }
            Command::OpenSub(sub) => {
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
            Command::SendAttachment { sub, message_id, image_url, content_type } => {
                if let Some(out) = &outbox {
                    let frame = json!({
                        "type": "send_attachment",
                        "message_id": message_id,
                        "url": image_url,
                        "content_type": content_type,
                        "sub": sub.scope.as_str(),
                        "scope_id": sub.id,
                    }).to_string();
                    let _ = out.send(RtCommand::Text(frame));
                }
                repaint.request();
            }
            Command::ToggleReaction { sub, message_id, emoji } => {
                if let Some(out) = &outbox {
                    let frame = json!({
                        "type": "toggle_reaction",
                        "message_id": message_id,
                        "emoji": emoji,
                        "scope": sub.scope.as_str(),
                        "scope_id": sub.id,
                    }).to_string();
                    let _ = out.send(RtCommand::ToggleReaction { message_id, emoji });
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
                    }).to_string();
                    let _ = out.send(RtCommand::Text(frame));
                }
            }
            Command::ListProfiles => {
                match api.list_profiles().await {
                    Ok(profiles) => {
                        let _ = updates.send(Update::Profiles { profiles });
                    }
                    Err(error) => {
                        let _ = updates.send(Update::Error(error.to_string()));
                    }
                }
                repaint.request();
            }
            Command::ProfileByUserId { user_id } => {
                match api.profile_by_user_id(user_id).await {
                    Ok(profile) => {
                        let _ = updates.send(Update::ProfileView { profile });
                    }
                    Err(error) => {
                        let _ = updates.send(Update::Error(error.to_string()));
                    }
                }
                repaint.request();
            }
            Command::ProfileByUsername { username } => {
                match api.profile_by_username(&username).await {
                    Ok(profile) => {
                        let _ = updates.send(Update::ProfileView { profile });
                    }
                    Err(error) => {
                        let _ = updates.send(Update::Error(error.to_string()));
                    }
                }
                repaint.request();
            }
            Command::Logout => {
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
