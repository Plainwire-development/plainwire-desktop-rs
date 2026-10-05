use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::sync::mpsc::Receiver;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use eframe::egui::{self, Color32, RichText};

use crate::account::{Account, AccountStore};
use crate::backend::{Backend, Command, Repainter, Update};
use crate::media::Room;
use crate::model::{
    CallInvite, Channel, Conversation, Message, Notification, Participant, Profile, RoomId,
    RoomKind, RoomPatch, RtcConfig, Scope, Server, User,
};
use crate::realtime::Sub;
use crate::tray::Tray;

#[derive(PartialEq, Eq)]
enum Screen {
    Login,
    Chat,
}

type RoomSlot = Arc<Mutex<Option<Arc<Room>>>>;

#[derive(Default)]
struct AvatarCache {
    textures: HashMap<String, egui::TextureHandle>,
    requested: HashMap<String, ()>,
    failed: HashMap<String, ()>,
    attempts: HashMap<String, u8>,
}

const AVATAR_MAX_ATTEMPTS: u8 = 3;

impl AvatarCache {
    fn clear(&mut self) {
        self.textures.clear();
        self.requested.clear();
        self.failed.clear();
        self.attempts.clear();
    }

    fn wants(&mut self, url: &str) -> bool {
        if url.trim().is_empty()
            || self.textures.contains_key(url)
            || self.requested.contains_key(url)
        {
            return false;
        }
        let attempts = self.attempts.entry(url.to_string()).or_insert(0);
        if *attempts >= AVATAR_MAX_ATTEMPTS {
            self.failed.insert(url.to_string(), ());
            return false;
        }
        *attempts += 1;
        self.requested.insert(url.to_string(), ());
        true
    }

    fn arrived(&mut self, url: &str, texture: egui::TextureHandle) {
        self.requested.remove(url);
        self.failed.remove(url);
        self.textures.insert(url.to_string(), texture);
    }
}

pub struct App {
    rt: tokio::runtime::Handle,
    backend: Backend,
    updates: Receiver<Update>,
    repaint: Repainter,
    screen: Screen,

    base: String,
    username: String,
    password: String,
    display_name: String,
    email: String,
    register_mode: bool,
    busy: bool,
    accounts: AccountStore,
    notifications: Vec<Notification>,
    presences: HashMap<i64, String>,

    me: Option<User>,
    my_id: i64,
    servers: Vec<Server>,
    conversations: Vec<Conversation>,
    channels: Vec<Channel>,
    server_id: Option<i64>,
    active: Option<Sub>,
    active_label: String,
    messages: Vec<Message>,
    typing: BTreeMap<i64, (String, Instant)>,
    realtime: bool,
    status: String,
    error: Option<String>,
    composer: String,
    uploading: usize,
    composer_ready: bool,
    last_typing: Instant,
    tray: Option<Tray>,

    avatars: AvatarCache,

    profile: Option<Profile>,
    profile_loading: bool,
    user_query: String,
    user_results: Vec<User>,
    people_open: bool,

    rtc_config: RtcConfig,
    room: RoomSlot,
    roster: Vec<Participant>,
    muted: bool,
    deafened: bool,
    sharing: bool,
    incoming: Option<CallInvite>,
    outgoing_call: Option<i64>,

    pending_join: Option<i64>,
    speaking: HashMap<i64, bool>,
    voice_note: Option<String>,
}

impl App {
    pub fn new(
        rt: tokio::runtime::Handle,
        backend: Backend,
        updates: Receiver<Update>,
        repaint: Repainter,
        ctx: &egui::Context,
    ) -> Self {
        ctx.set_visuals(egui::Visuals::dark());
        repaint.set(ctx.clone());
        let accounts = AccountStore::load();
        let preset = accounts.preferred().and_then(|i| accounts.accounts.get(i));
        let base = preset
            .map(|a| a.base.clone())
            .filter(|b| !b.is_empty())
            .unwrap_or_else(|| "https://plainwi.re".to_string());
        let username = preset.map(|a| a.username.clone()).unwrap_or_default();
        App {
            rt,
            backend,
            updates,
            repaint,
            screen: Screen::Login,
            base,
            username,
            password: String::new(),
            display_name: String::new(),
            email: String::new(),
            register_mode: false,
            busy: false,
            accounts,
            notifications: Vec::new(),
            presences: HashMap::new(),
            me: None,
            my_id: 0,
            servers: Vec::new(),
            conversations: Vec::new(),
            channels: Vec::new(),
            server_id: None,
            active: None,
            active_label: String::new(),
            messages: Vec::new(),
            typing: BTreeMap::new(),
            realtime: false,
            status: String::new(),
            error: None,
            composer: String::new(),
            uploading: 0,
            composer_ready: false,
            last_typing: Instant::now(),
            tray: None,
            avatars: AvatarCache::default(),
            profile: None,
            profile_loading: false,
            user_query: String::new(),
            user_results: Vec::new(),
            people_open: false,
            rtc_config: RtcConfig::default(),
            room: Arc::new(Mutex::new(None)),
            roster: Vec::new(),
            muted: false,
            deafened: false,
            sharing: false,
            incoming: None,
            outgoing_call: None,
            pending_join: None,
            speaking: HashMap::new(),
            voice_note: None,
        }
    }

    fn pump(&mut self) {
        while let Ok(update) = self.updates.try_recv() {
            self.apply(update);
        }
        let now = Instant::now();
        self.typing
            .retain(|_, (_, at)| now.duration_since(*at) < Duration::from_secs(6));
        self.sync_speaking();
    }

    fn sync_speaking(&mut self) {
        let Some(room) = self.room.lock().unwrap().clone() else {
            return;
        };
        for (id, status) in room.media().snapshot().peers {
            self.speaking.insert(id, status.speaking);
        }
    }

    fn apply(&mut self, update: Update) {
        match update {
            Update::Session(session) => {
                self.me = Some(session.user);
                self.screen = Screen::Chat;
                self.password.clear();
                self.busy = false;
                self.error = None;
            }
            Update::SignedIn { user, token, csrf } => {
                let base = self.base.clone();
                let password = std::mem::take(&mut self.password);
                self.me = Some(user.clone());
                self.my_id = user.id;
                self.screen = Screen::Chat;
                self.busy = false;
                self.error = None;
                self.remember(base, password, user, token, csrf);
            }
            Update::RtcConfig(config) => {
                self.rtc_config = config;
            }
            Update::Sync(payload) => {
                self.servers = payload.servers;
                self.conversations = payload.conversations;
                if self.server_id.is_none() {
                    if let Some(server) = self.servers.first() {
                        self.open_server(server.id);
                    }
                }
            }
            Update::Channels { server_id, detail } => {
                if self.server_id == Some(server_id) {
                    self.channels = detail.channels;
                    if self.active.is_none() {
                        if let Some(channel) = self.channels.iter().find(|c| c.is_text()) {
                            let sub = Sub {
                                scope: Scope::Channel,
                                id: channel.id,
                            };
                            let label = channel.name.clone();
                            self.open_sub(sub, label);
                        }
                    }
                }
            }
            Update::Messages {
                sub,
                messages,
                older,
            } => {
                if self.active == Some(sub) {
                    if older {
                        for message in messages {
                            self.insert_message(message);
                        }
                    } else {
                        self.messages = messages;
                        self.messages.sort_by_key(|m| m.id);
                    }
                }
            }
            Update::MessageUpsert { sub, message } => {
                if self.active == Some(sub) {
                    self.insert_message(message);
                }
            }
            Update::MessageDeleted { sub, message_id } => {
                if self.active == Some(sub) {
                    self.messages.retain(|m| m.id != message_id);
                }
            }
            Update::ReactionAdded { sub, message } | Update::ReactionRemoved { sub, message } => {
                if self.active == Some(sub) {
                    self.insert_message(message);
                }
            }
            Update::UserSearch { query, users } => {
                if self.user_query.trim() == query.trim() {
                    self.user_results = users;
                }
            }
            Update::ProfileView { profile } => {
                self.profile = Some(profile);
                self.profile_loading = false;
            }
            Update::Avatar { key, image } => {
                let size = [image.width() as usize, image.height() as usize];
                let color = egui::ColorImage::from_rgba_unmultiplied(size, image.as_raw());
                if let Some(texture) = self.repaint.load_texture(format!("avatar:{key}"), color) {
                    self.avatars.arrived(&key, texture);
                }
            }
            Update::AvatarFailed { key } => {
                self.avatars.requested.remove(&key);
            }
            Update::Uploaded {
                name,
                content_type,
                url,
            } => {
                self.error = None;
                self.uploading = self.uploading.saturating_sub(1);
                let markup = crate::api::attachment_markup(&name, &content_type, &url);
                append_to_composer(&mut self.composer, &markup);
                self.status = format!("{name} ready to send");
            }
            Update::UploadFailed { name, reason } => {
                self.uploading = self.uploading.saturating_sub(1);
                self.error = Some(format!("could not upload {name}: {reason}"));
            }
            Update::RoomRoster { room, participants } => {
                if self.current_room() == Some(room) {
                    self.set_roster(room, participants);
                }
            }
            Update::RoomPeerJoined {
                room,
                user_id,
                profile,
            } => {
                if self.current_room() == Some(room)
                    && !self.roster.iter().any(|p| p.user_id == user_id)
                {
                    self.roster.push(Participant {
                        user_id,
                        profile,
                        ..Default::default()
                    });
                }
            }
            Update::RoomPeerLeft { room, user_id } => {
                if self.current_room() == Some(room) {
                    self.roster.retain(|p| p.user_id != user_id);
                    self.speaking.remove(&user_id);
                }
            }
            Update::RoomSuperseded { room } => {
                if self.current_room() == Some(room) {
                    self.voice_note = Some("Disconnected: another room took over.".into());
                }
            }
            Update::RoomActivity {
                room,
                user_id,
                active,
            } => {
                if self.current_room() == Some(room) {
                    self.speaking.insert(user_id, active);
                }
            }
            Update::RoomSignal {
                room,
                from_user_id,
                signal,
            } => {
                if self.current_room() == Some(room) {
                    if let Some(handle) = self.room.lock().unwrap().clone() {
                        self.rt.spawn(async move {
                            handle.handle_signal(from_user_id, &signal).await;
                        });
                    }
                }
            }
            Update::CallRinging {
                conversation_id,
                profile,
            } => {
                self.voice_note = Some(format!("Ringing {}…", profile.label()));
                self.outgoing_call = Some(conversation_id);
            }
            Update::CallIncoming(invite) => {
                self.incoming = Some(invite);
            }
            Update::CallAccepted { conversation_id } => {
                if self.outgoing_call == Some(conversation_id)
                    || self.pending_join == Some(conversation_id)
                {
                    self.outgoing_call = None;
                    self.pending_join = None;
                    self.voice_note = None;
                    self.enter_call(conversation_id);
                }
            }
            Update::CallEnded {
                conversation_id,
                reason,
            } => {
                if self.outgoing_call == Some(conversation_id) {
                    self.outgoing_call = None;
                }
                if self.pending_join == Some(conversation_id) {
                    self.pending_join = None;
                }
                if self.incoming.as_ref().map(|i| i.conversation_id) == Some(conversation_id) {
                    self.incoming = None;
                }
                if self.current_room() == Some(RoomId::call(conversation_id)) {
                    self.voice_note = Some(format!("Call ended ({reason})"));
                    self.leave_room();
                }
            }
            Update::Typing {
                sub,
                user_id,
                name,
                active,
            } => {
                if self.active == Some(sub) {
                    if active {
                        self.typing.insert(user_id, (name, Instant::now()));
                    } else {
                        self.typing.remove(&user_id);
                    }
                }
            }
            Update::Realtime(online) => {
                self.realtime = online;
                if online {
                    self.status.clear();
                }
            }
            Update::LoggedOut => {
                self.leave_room();
                self.screen = Screen::Login;
                self.me = None;
                self.my_id = 0;
                self.servers.clear();
                self.conversations.clear();
                self.channels.clear();
                self.messages.clear();
                self.active = None;
                self.server_id = None;
                self.realtime = false;
                self.avatars.clear();
                self.profile = None;
                self.notifications.clear();
                self.presences.clear();
                self.busy = false;
            }
            Update::Error(message) => {
                self.busy = false;
                self.error = Some(message);
            }
            Update::Status(message) => {
                self.status = message;
            }
            Update::Notification(notification) => {
                self.notifications.push(notification);
            }
            Update::PresenceOnline {
                user_id, status, ..
            } => {
                self.presences.insert(user_id, status);
            }
            Update::PresenceOffline { user_id } => {
                self.presences.remove(&user_id);
            }
            Update::PresenceStatus {
                user_id, status, ..
            } => {
                self.presences.insert(user_id, status);
            }
            Update::PresenceState(snapshot) => {
                self.presences.clear();
                for (id, status) in snapshot.statuses {
                    self.presences.insert(id, status);
                }
                for id in snapshot.online {
                    self.presences
                        .entry(id)
                        .or_insert_with(|| "online".to_string());
                }
            }
        }
        self.repaint.request();
    }

    fn current_room(&self) -> Option<RoomId> {
        self.room.lock().unwrap().as_ref().map(|r| r.id)
    }

    fn set_roster(&mut self, room: RoomId, participants: Vec<Participant>) {
        self.roster = participants.clone();
        let handle = self.room.lock().unwrap().clone();
        let Some(handle) = handle.filter(|r| r.id == room) else {
            return;
        };
        let media = handle.media();
        let my_id = self.my_id;
        for participant in &participants {
            media.update_peer(
                participant.user_id,
                participant,
                participant.profile.label().to_string(),
            );
            if participant.user_id == my_id {
                self.muted = participant.muted;
                self.deafened = participant.deafened;
                self.sharing = participant.screen;
            }
        }

        let live: Vec<i64> = media
            .snapshot()
            .peers
            .keys()
            .copied()
            .filter(|id| *id != my_id && !participants.iter().any(|p| p.user_id == *id))
            .collect();
        for id in live {
            media.forget(id);
        }
        self.rt.spawn(async move {
            handle.sync_peers(&participants).await;
        });
    }

    fn join_voice(&mut self, channel_id: i64) {
        self.leave_room();
        let backend = self.backend.clone();
        let config = self.rtc_config.clone();
        let my_id = self.my_id;
        let slot = self.room.clone();
        let room_id = RoomId::voice(channel_id);
        let rt = self.rt.clone();
        rt.spawn(async move {
            let sink: crate::media::SignalSink = {
                let backend = backend.clone();
                Arc::new(move |room, to, signal| {
                    backend.send(Command::Signal {
                        room,
                        to_user_id: to,
                        signal,
                    });
                })
            };
            let activity: crate::media::ActivitySink = {
                let backend = backend.clone();
                Arc::new(move |active, level_db| {
                    backend.send(Command::Activity { active, level_db });
                })
            };
            let handle = match Room::new(room_id, my_id, &config, sink, activity).await {
                Ok(handle) => handle,
                Err(error) => {
                    backend.send(Command::Status(format!("Voice unavailable: {error}")));
                    return;
                }
            };
            handle.start_mic().await;
            backend.send(Command::VoiceJoin { channel_id });
            *slot.lock().unwrap() = Some(handle);
        });
        self.reset_room_state();
    }

    fn start_call(&mut self, conversation_id: i64) {
        self.outgoing_call = Some(conversation_id);
        self.backend.send(Command::CallRing { conversation_id });
    }

    fn accept_call(&mut self, conversation_id: i64) {
        self.pending_join = Some(conversation_id);
        self.backend.send(Command::CallAccept { conversation_id });
    }

    fn enter_call(&mut self, conversation_id: i64) {
        self.leave_room();
        let backend = self.backend.clone();
        let config = self.rtc_config.clone();
        let my_id = self.my_id;
        let slot = self.room.clone();
        let room_id = RoomId::call(conversation_id);
        let rt = self.rt.clone();
        rt.spawn(async move {
            let sink: crate::media::SignalSink = {
                let backend = backend.clone();
                Arc::new(move |room, to, signal| {
                    backend.send(Command::Signal {
                        room,
                        to_user_id: to,
                        signal,
                    });
                })
            };
            let activity: crate::media::ActivitySink = {
                let backend = backend.clone();
                Arc::new(move |active, level_db| {
                    backend.send(Command::Activity { active, level_db });
                })
            };
            let handle = match Room::new(room_id, my_id, &config, sink, activity).await {
                Ok(handle) => handle,
                Err(error) => {
                    backend.send(Command::Status(format!("Call unavailable: {error}")));
                    return;
                }
            };
            handle.start_mic().await;

            backend.send(Command::CallJoin { conversation_id });
            *slot.lock().unwrap() = Some(handle);
        });
        self.reset_room_state();
    }

    fn reset_room_state(&mut self) {
        self.roster.clear();
        self.outgoing_call = None;
        self.pending_join = None;
        self.speaking.clear();
        self.muted = false;
        self.deafened = false;
        self.sharing = false;
        self.voice_note = None;
    }

    fn leave_room(&mut self) {
        let taken = self.room.lock().unwrap().take();
        self.reset_room_state();
        let backend = self.backend.clone();
        if let Some(handle) = taken {
            let kind = handle.id.kind;
            self.rt.spawn(async move {
                handle.shutdown().await;
            });
            backend.send(Command::LeaveRoom(kind));
        }
    }

    fn toggle_register(&mut self) {
        self.register_mode = !self.register_mode;
    }

    fn connect(&mut self) {
        self.busy = true;
        self.error = None;
        if self.register_mode {
            self.backend.send(Command::Register {
                base: self.base.clone(),
                username: self.username.clone(),
                display_name: self.display_name.clone(),
                password: self.password.clone(),
                email: self.email.clone(),
            });
        } else {
            self.backend.send(Command::Login {
                base: self.base.clone(),
                username: self.username.clone(),
                password: self.password.clone(),
            });
        }
    }

    fn switch_account(&mut self, index: usize) {
        let Some(account) = self.accounts.accounts.get(index).cloned() else {
            return;
        };
        self.accounts.select(index);
        let _ = self.accounts.save();
        self.base = account.base.clone();
        self.username = account.username.clone();
        self.error = None;
        match (account.token.clone(), account.csrf.clone()) {
            (Some(token), Some(csrf)) => {
                self.busy = true;
                self.backend.send(Command::Resume {
                    base: account.base,
                    token,
                    csrf,
                });
            }
            _ => {
                self.password = account.password.clone();
                if self.password.is_empty() {
                    self.busy = false;
                    self.error = Some(format!(
                        "Enter the password for {} to connect.",
                        account.username
                    ));
                } else {
                    self.connect();
                }
            }
        }
    }

    fn forget_account(&mut self, index: usize) {
        self.accounts.remove(index);
        let _ = self.accounts.save();
    }

    fn remember(
        &mut self,
        base: String,
        password: String,
        user: User,
        token: String,
        csrf: String,
    ) {
        let account = Account {
            base,
            username: user.username.clone(),
            password,
            user_id: user.id,
            display_name: user.display_name.clone(),
            avatar_url: user.avatar_url.clone(),
            token: Some(token),
            csrf: Some(csrf),
        };
        let index = self.accounts.upsert(account);
        self.accounts.select(index);
        let _ = self.accounts.save();
    }

    fn current_account_index(&self) -> Option<usize> {
        if self.my_id > 0 {
            if let Some(index) = self.accounts.by_user_id(self.my_id).and_then(|_| {
                self.accounts
                    .accounts
                    .iter()
                    .position(|a| a.user_id == self.my_id)
            }) {
                return Some(index);
            }
        }
        self.accounts.index_of(&self.base, &self.username)
    }

    fn insert_message(&mut self, message: Message) {
        if let Some(slot) = self.messages.iter_mut().find(|m| m.id == message.id) {
            *slot = message;
        } else {
            self.messages.push(message);
            self.messages.sort_by_key(|m| m.id);
        }
    }

    fn open_server(&mut self, server_id: i64) {
        self.server_id = Some(server_id);
        self.channels.clear();
        self.active = None;
        self.messages.clear();
        self.backend.send(Command::LoadServer(server_id));
    }

    fn open_sub(&mut self, sub: Sub, label: String) {
        if sub.scope == Scope::Direct {
            self.backend.send(Command::MarkRead {
                conversation_id: sub.id,
            });
            if let Some(conversation) = self.conversations.iter_mut().find(|c| c.id == sub.id) {
                conversation.unread = 0;
            }
        }
        self.active = Some(sub);
        self.active_label = label;
        self.messages.clear();
        self.typing.clear();
        self.backend.send(Command::OpenSub(sub));
    }

    pub fn init_tray(&mut self) {
        let icon_path = std::env::current_exe()
            .ok()
            .and_then(|path| {
                let mut parent = path.parent();
                if let Some(p) = parent {
                    parent = p.parent();
                }
                parent
                    .map(|p| p.join("assets"))
                    .or_else(|| std::env::current_dir().ok())
            })
            .and_then(|dir| {
                fs::read_dir(dir).ok().and_then(|mut entries| {
                    entries
                        .next()
                        .map(|e| e.ok())
                        .and_then(|e| e.map(|e| e.path()))
                })
            });
        self.tray = Some(Tray::new(icon_path));
        if let Some(ref mut tray) = self.tray {
            tray.build("Plainwire");
            tray.show();
        }
    }

    fn load_profile_by_id(&mut self, user_id: i64) {
        self.profile_loading = true;
        self.backend.send(Command::ProfileByUserId { user_id });
    }

    fn load_profile_by_username(&mut self, username: &str) {
        self.profile_loading = true;
        self.backend.send(Command::ProfileByUsername {
            username: username.to_string(),
        });
    }

    fn submit(&mut self) {
        let body = self.composer.trim().to_string();
        if body.is_empty() {
            return;
        }
        if let Some(sub) = self.active {
            self.composer.clear();
            self.backend.send(Command::Send { sub, body });
        }
    }

    fn avatar(&mut self, ui: &mut egui::Ui, url: &str, _size: f32) {
        if url.trim().is_empty() || self.avatars.failed.contains_key(url) {
            self.avatar_placeholder(ui);
            return;
        }
        if self.avatars.wants(url) {
            self.backend.send(Command::FetchAvatar {
                key: url.to_string(),
                url: url.to_string(),
            });
        }
        match self.avatars.textures.get(url) {
            Some(texture) => {
                let sized = fixed_box(texture);
                ui.add(
                    egui::Image::new(sized)
                        .fit_to_exact_size(egui::vec2(IMAGE_BOX, IMAGE_BOX))
                        .sense(egui::Sense::hover()),
                );
            }
            None => self.avatar_placeholder(ui),
        }
    }
    fn avatar_placeholder(&mut self, ui: &mut egui::Ui) {
        let size = IMAGE_BOX;
        let (rect, _) = ui.allocate_exact_size(egui::vec2(size, size), egui::Sense::hover());
        ui.painter()
            .circle_filled(rect.center(), size / 2.0, Color32::from_rgb(70, 74, 86));
        let initials = initials_of(self.me.as_ref().map(|u| u.label()).unwrap_or_default());
        ui.painter().text(
            rect.center(),
            egui::Align2::CENTER_CENTER,
            &initials,
            egui::FontId::proportional(size * 0.4),
            Color32::from_rgb(225, 228, 235),
        );
    }

    fn presence_dot(&self, ui: &mut egui::Ui, user_id: i64) {
        match self.presences.get(&user_id) {
            Some(status) if !status.is_empty() => {
                let color = presence_color(status);
                let (rect, _) = ui.allocate_exact_size(egui::vec2(8.0, 8.0), egui::Sense::hover());
                ui.painter().circle_filled(rect.center(), 4.0, color);
                ui.label(RichText::new(status.as_str()).small().weak());
            }
            _ => {
                let (rect, _) = ui.allocate_exact_size(egui::vec2(8.0, 8.0), egui::Sense::hover());
                ui.painter()
                    .circle_filled(rect.center(), 4.0, Color32::from_rgb(80, 84, 96));
            }
        }
    }
}

const IMAGE_BOX: f32 = 20.0;

fn fixed_box(handle: &egui::TextureHandle) -> egui::load::SizedTexture {
    egui::load::SizedTexture::new(handle.id(), egui::vec2(IMAGE_BOX, IMAGE_BOX))
}

const SEND_BUTTON_WIDTH: f32 = 64.0;

const ATTACH_BUTTON_WIDTH: f32 = 34.0;

const MIN_COMPOSER_WIDTH: f32 = 20.0;

const ATTACHMENT_MAX_EDGE: f32 = 240.0;

pub fn fit_attachment_size(width: usize, height: usize, max_edge: f32) -> egui::Vec2 {
    if width == 0 || height == 0 || max_edge <= 0.0 {
        return egui::vec2(max_edge.max(1.0), max_edge.max(1.0));
    }
    let longest = width.max(height) as f32;
    let scale = (max_edge / longest).min(1.0);
    egui::vec2(
        (width as f32 * scale).max(1.0),
        (height as f32 * scale).max(1.0),
    )
}

fn composer_edit(text: &mut String) -> egui::TextEdit<'_> {
    egui::TextEdit::singleline(text)
        .hint_text("Message")
        .desired_width(f32::INFINITY)
        .return_key(None::<egui::KeyboardShortcut>)
}

pub fn composer_width(available: f32, item_spacing: f32) -> f32 {
    (available - SEND_BUTTON_WIDTH - ATTACH_BUTTON_WIDTH - item_spacing * 2.0)
        .max(MIN_COMPOSER_WIDTH)
}

pub fn paste_shortcut(modifiers: egui::Modifiers, v_pressed: bool) -> bool {
    let command = modifiers.command || modifiers.ctrl;
    v_pressed && command && modifiers.shift
}

pub fn paste_gesture(ui: &egui::Ui) -> bool {
    ui.input(|i| {
        i.events
            .iter()
            .any(|event| matches!(event, egui::Event::Paste(_)))
            || paste_shortcut(i.modifiers, i.key_pressed(egui::Key::V))
    })
}

pub fn paste_armed(composer_ready: bool, focused: Option<egui::Id>) -> bool {
    composer_ready || focused.is_none()
}

pub fn append_to_composer(text: &mut String, addition: &str) {
    if !text.is_empty() && !text.ends_with(char::is_whitespace) {
        text.push(' ');
    }
    text.push_str(addition);
}

pub fn percent_decode_component(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' && index + 2 < bytes.len() {
            let hex = std::str::from_utf8(&bytes[index + 1..index + 3]).unwrap_or("");
            if let Ok(byte) = u8::from_str_radix(hex, 16) {
                out.push(byte);
                index += 3;
                continue;
            }
        }
        out.push(bytes[index]);
        index += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

pub enum ClipAttachment {
    Image(Vec<u8>),
    File(std::path::PathBuf),
    PlainText(String),
}

pub fn single_line(text: &str) -> String {
    text.split_whitespace().collect::<Vec<&str>>().join(" ")
}

pub fn path_from_clipboard_text(text: &str) -> Option<std::path::PathBuf> {
    for line in text.lines() {
        let candidate = line.trim();
        if candidate.is_empty() || candidate.starts_with('#') {
            continue;
        }
        let path = match candidate.strip_prefix("file://") {
            Some(rest) => std::path::PathBuf::from(percent_decode_component(rest)),
            None => std::path::PathBuf::from(candidate),
        };
        if path.is_file() {
            return Some(path);
        }
    }
    None
}

fn clipboard_attachment() -> Result<ClipAttachment, String> {
    let mut clipboard =
        arboard::Clipboard::new().map_err(|error| format!("cannot open the clipboard: {error}"))?;
    if let Ok(data) = clipboard.get_image() {
        let rgba = image::RgbaImage::from_raw(
            data.width as u32,
            data.height as u32,
            data.bytes.into_owned(),
        );
        if let Some(rgba) = rgba {
            let mut encoded = std::io::Cursor::new(Vec::new());
            rgba.write_to(&mut encoded, image::ImageFormat::Png)
                .map_err(|error| format!("cannot encode the pasted image: {error}"))?;
            return Ok(ClipAttachment::Image(encoded.into_inner()));
        }
    }
    if let Ok(text) = clipboard.get_text() {
        if let Some(path) = path_from_clipboard_text(&text) {
            return Ok(ClipAttachment::File(path));
        }
        if !text.trim().is_empty() {
            return Ok(ClipAttachment::PlainText(text));
        }
    }
    Err("the clipboard holds no image or file".to_string())
}

fn composer_enter_requested(ui: &egui::Ui, response: &egui::Response) -> bool {
    response.has_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter) && !i.modifiers.shift)
}

fn initials_of(name: &str) -> String {
    let parts: Vec<&str> = name.split_whitespace().filter(|p| !p.is_empty()).collect();
    let text = match parts.len() {
        0 => "?".to_string(),
        1 => parts[0].chars().take(2).collect::<String>(),
        _ => parts
            .iter()
            .take(2)
            .filter_map(|p| p.chars().next())
            .collect(),
    };
    text.to_uppercase()
}

enum SidebarAction {
    OpenServer(i64),
    OpenSub(Sub, String),
    JoinVoice(i64),
    LeaveRoom,
    Call(i64),
    TogglePeople,
    SignOut,
    AddAccount,
    SwitchAccount(usize),
    RemoveAccount(usize),
}

enum VoiceAction {
    ToggleMute,
    ToggleDeafen,
    ToggleShare,
    Leave,
}

impl eframe::App for App {
    fn ui(&mut self, root: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.pump();
        root.ctx().request_repaint_after(Duration::from_millis(250));
        match self.screen {
            Screen::Login => self.draw_login(root),
            Screen::Chat => {
                self.draw_sidebar(root);
                if self.people_open {
                    self.draw_people(root);
                }
                if self.profile.is_some() {
                    self.draw_profile(root);
                } else {
                    self.draw_conversation(root);
                }

                let ctx = root.ctx().clone();
                let handle = self.room.lock().unwrap().clone();
                if let Some(handle) = handle {
                    self.draw_voice(&ctx, handle);
                }
                let incoming = self.incoming.clone();
                if let Some(invite) = incoming {
                    self.draw_incoming_call(&ctx, invite);
                }
            }
        }
    }
}

impl App {
    fn draw_login(&mut self, root: &mut egui::Ui) {
        egui::CentralPanel::default().show(root, |ui| {
            ui.vertical_centered(|ui| {
                ui.add_space(56.0);
                ui.heading(RichText::new("Plainwire Desktop").size(30.0));
                ui.add_space(20.0);
            });
            let width = 380.0f32.min(ui.available_width());
            ui.vertical_centered(|ui| {
                ui.allocate_ui_with_layout(
                    egui::vec2(width, 0.0),
                    egui::Layout::top_down(egui::Align::Min),
                    |ui| {
                        if !self.accounts.accounts.is_empty() {
                            ui.label(RichText::new("Your accounts").small().weak());
                            let accounts = self.accounts.accounts.clone();
                            for (index, account) in accounts.iter().enumerate() {
                                ui.horizontal(|ui| {
                                    let selected = self.accounts.selected == Some(index);
                                    let label = if account.display_name.is_empty() {
                                        account.username.clone()
                                    } else {
                                        format!("{} (@{})", account.display_name, account.username)
                                    };
                                    if ui
                                        .selectable_label(selected, label)
                                        .on_hover_text(&account.base)
                                        .clicked()
                                    {
                                        self.switch_account(index);
                                    }
                                    if ui
                                        .small_button("✕")
                                        .on_hover_text("Forget this account")
                                        .clicked()
                                    {
                                        self.forget_account(index);
                                    }
                                });
                            }
                            ui.add_space(10.0);
                            ui.separator();
                            ui.add_space(8.0);
                        }

                        if self.register_mode {
                            ui.label("Display name");
                            ui.add_sized(
                                [width, 28.0],
                                egui::TextEdit::singleline(&mut self.display_name),
                            );
                            ui.add_space(8.0);
                            ui.label("Email");
                            ui.add_sized(
                                [width, 28.0],
                                egui::TextEdit::singleline(&mut self.email),
                            );
                            ui.add_space(8.0);
                        }
                        ui.label("Server");
                        ui.add_sized(
                            [width, 28.0],
                            egui::TextEdit::singleline(&mut self.base)
                                .hint_text("https://plainwi.re"),
                        );
                        ui.add_space(8.0);
                        ui.label("Username");
                        ui.add_sized(
                            [width, 28.0],
                            egui::TextEdit::singleline(&mut self.username),
                        );
                        ui.add_space(8.0);
                        ui.label("Password");
                        ui.add_sized(
                            [width, 28.0],
                            egui::TextEdit::singleline(&mut self.password).password(true),
                        );
                        ui.add_space(16.0);
                        ui.horizontal(|ui| {
                            let toggle_label = if self.register_mode {
                                "Login"
                            } else {
                                "Register"
                            };
                            if ui.small_button(toggle_label).clicked() {
                                self.toggle_register();
                            }
                        });
                        let enabled = !self.busy;
                        let button = ui.add_enabled(
                            enabled,
                            egui::Button::new(if self.busy {
                                "Connecting…"
                            } else if self.register_mode {
                                "Register"
                            } else {
                                "Connect"
                            })
                            .min_size(egui::vec2(width, 30.0)),
                        );
                        let enter = ui.input(|i| i.key_pressed(egui::Key::Enter));
                        if (button.clicked() || enter) && enabled {
                            self.connect();
                        }
                        if let Some(error) = &self.error {
                            ui.add_space(12.0);
                            ui.colored_label(Color32::from_rgb(230, 110, 110), error);
                        }
                    },
                );
            });
        });
    }

    fn draw_sidebar(&mut self, root: &mut egui::Ui) {
        let mut actions: Vec<SidebarAction> = Vec::new();
        let my_id = self.my_id;
        let my_label = self
            .me
            .as_ref()
            .map(|u| u.label().to_string())
            .unwrap_or_default();
        let my_avatar = self
            .me
            .as_ref()
            .map(|u| u.avatar_url.clone())
            .unwrap_or_default();

        egui::Panel::left("navigation")
            .resizable(true)
            .default_size(262.0)
            .show(root, |ui| {
                ui.add_space(6.0);
                egui::Frame::group(ui.style())
                    .inner_margin(8.0)
                    .show(ui, |ui| {
                        ui.horizontal(|ui| {
                            self.avatar(ui, &my_avatar, 32.0);
                            ui.vertical(|ui| {
                                ui.label(RichText::new(&my_label).strong());
                                ui.label(RichText::new(self.instance_label()).small().weak());
                            });
                        });
                        ui.add_space(6.0);
                        self.account_menu(ui, my_id, &mut actions);
                    });
                ui.separator();

                egui::ScrollArea::vertical().show(ui, |ui| {
                    ui.label(RichText::new("Servers").small().weak());
                    let servers = self.servers.clone();
                    for server in servers {
                        let selected = self.server_id == Some(server.id);
                        if ui.selectable_label(selected, &server.name).clicked() {
                            actions.push(SidebarAction::OpenServer(server.id));
                        }
                    }

                    let channels = self.channels.clone();
                    if !channels.is_empty() {
                        ui.add_space(8.0);
                        ui.label(RichText::new("Channels").small().weak());
                        for channel in channels {
                            if channel.is_voice() {
                                let joined = self.current_room() == Some(RoomId::voice(channel.id));
                                if ui
                                    .selectable_label(joined, format!("🔊 {}", channel.name))
                                    .on_hover_text("Click to join or leave")
                                    .clicked()
                                {
                                    actions.push(if joined {
                                        SidebarAction::LeaveRoom
                                    } else {
                                        SidebarAction::JoinVoice(channel.id)
                                    });
                                }
                                continue;
                            }
                            let selected = self.active
                                == Some(Sub {
                                    scope: Scope::Channel,
                                    id: channel.id,
                                });
                            if ui
                                .selectable_label(selected, format!("# {}", channel.name))
                                .clicked()
                            {
                                actions.push(SidebarAction::OpenSub(
                                    Sub {
                                        scope: Scope::Channel,
                                        id: channel.id,
                                    },
                                    channel.name.clone(),
                                ));
                            }
                        }
                    }

                    ui.add_space(8.0);
                    ui.label(RichText::new("Direct messages").small().weak());
                    let conversations = self.conversations.clone();
                    for conversation in conversations {
                        let selected = self.active
                            == Some(Sub {
                                scope: Scope::Direct,
                                id: conversation.id,
                            });
                        let mut text = conversation.label();
                        if conversation.unread > 0 {
                            text = format!("{}  ({})", text, conversation.unread);
                        }
                        let avatar = conversation.avatar().to_string();
                        let in_call = self.current_room() == Some(RoomId::call(conversation.id));
                        let peer_id = conversation.peer_id;
                        ui.horizontal(|ui| {
                            if ui.selectable_label(selected, text).clicked() {
                                actions.push(SidebarAction::OpenSub(
                                    Sub {
                                        scope: Scope::Direct,
                                        id: conversation.id,
                                    },
                                    conversation.label(),
                                ));
                            }
                            self.avatar(ui, &avatar, 18.0);
                            self.presence_dot(ui, peer_id);
                            let label = if in_call { "⏹" } else { "📞" };
                            if ui
                                .small_button(label)
                                .on_hover_text(if in_call {
                                    "Leave the call"
                                } else {
                                    "Start a call"
                                })
                                .clicked()
                            {
                                actions.push(if in_call {
                                    SidebarAction::LeaveRoom
                                } else {
                                    SidebarAction::Call(conversation.id)
                                });
                            }
                        });
                    }

                    if !self.notifications.is_empty() {
                        ui.add_space(10.0);
                        ui.label(RichText::new("Notifications").small().weak());
                        let notifs = self.notifications.clone();
                        for n in notifs.iter().rev().take(10) {
                            let who = if n.event == "friend_request" {
                                n.from_user_id
                            } else {
                                n.user_id
                            };
                            let text = match n.event.as_str() {
                                "friend_request" => format!("User #{} sent a friend request", who),
                                "friend_accept" => {
                                    format!("User #{} accepted your friend request", who)
                                }
                                other => format!("Notification: {}", other),
                            };
                            ui.label(RichText::new(text).small());
                        }
                        if notifs.len() > 10 {
                            ui.label(
                                RichText::new(format!("+ {} more", notifs.len() - 10))
                                    .small()
                                    .weak(),
                            );
                        }
                    }
                    ui.add_space(10.0);
                    ui.horizontal(|ui| {
                        if ui.selectable_label(self.people_open, "👥 People").clicked() {
                            actions.push(SidebarAction::TogglePeople);
                        }
                        if ui.small_button("Sign out").clicked() {
                            actions.push(SidebarAction::SignOut);
                        }
                    });
                });
            });

        for action in actions {
            self.run_sidebar(action);
        }
    }

    fn instance_label(&self) -> String {
        self.current_account_index()
            .and_then(|i| self.accounts.accounts.get(i))
            .map(|a| a.base.clone())
            .unwrap_or_else(|| self.base.clone())
    }

    fn account_menu(&mut self, ui: &mut egui::Ui, my_id: i64, actions: &mut Vec<SidebarAction>) {
        let current = self.current_account_index();
        let accounts = self.accounts.accounts.clone();
        let mut selection = self.accounts.selected;
        egui::ComboBox::from_id_salt("account-switcher")
            .selected_text(match current {
                Some(index) => format!("Account: {}", accounts[index].label()),
                None => "No account selected".to_string(),
            })
            .width(ui.available_width())
            .show_ui(ui, |ui| {
                if accounts.is_empty() {
                    ui.label(RichText::new("No saved accounts yet").weak());
                }
                for (index, account) in accounts.iter().enumerate() {
                    let mark = if Some(index) == current { "● " } else { "" };
                    let label = format!("{mark}{} — {}", account.label(), account.base);
                    if ui
                        .selectable_label(Some(index) == selection, label)
                        .clicked()
                    {
                        selection = Some(index);
                    }
                }
                ui.separator();
                if ui.button("Connect to this account").clicked() {
                    if let Some(index) = selection {
                        actions.push(SidebarAction::SwitchAccount(index));
                    }
                }
                if ui.button("＋ Add another account").clicked() {
                    actions.push(SidebarAction::AddAccount);
                }
                if ui.button("💾 Save current session").clicked() {
                    actions.push(SidebarAction::AddAccount);
                }
                if let Some(index) = current {
                    let is_me = accounts
                        .get(index)
                        .map(|a| a.user_id == my_id)
                        .unwrap_or(false);
                    if ui
                        .button(if is_me {
                            "Remove saved account"
                        } else {
                            "Remove account"
                        })
                        .clicked()
                    {
                        actions.push(SidebarAction::RemoveAccount(index));
                    }
                }
            });
        if selection != self.accounts.selected && selection.is_some() {
            self.accounts.selected = selection;
        }
    }

    fn run_sidebar(&mut self, action: SidebarAction) {
        match action {
            SidebarAction::OpenServer(id) => self.open_server(id),
            SidebarAction::OpenSub(sub, label) => self.open_sub(sub, label),
            SidebarAction::JoinVoice(id) => self.join_voice(id),
            SidebarAction::LeaveRoom => {
                let room = self.current_room();
                self.leave_room();
                if let Some(RoomId {
                    kind: RoomKind::Call,
                    id,
                }) = room
                {
                    self.backend.send(Command::CallCancel {
                        conversation_id: id,
                    });
                }
            }
            SidebarAction::Call(id) => self.start_call(id),
            SidebarAction::TogglePeople => {
                self.people_open = !self.people_open;
                self.profile = None;
            }
            SidebarAction::SignOut => self.backend.send(Command::Logout),
            SidebarAction::AddAccount => {
                if self.me.is_some() {
                    let base = self.base.clone();
                    let user = self.me.clone().unwrap();
                    if let Some(index) = self
                        .accounts
                        .accounts
                        .iter()
                        .position(|a| a.user_id == user.id)
                    {
                        let token = self.accounts.accounts[index].token.clone();
                        let csrf = self.accounts.accounts[index].csrf.clone();
                        self.accounts.select(index);
                        let _ = self.accounts.save();
                        let _ = (base, token, csrf);
                    }
                }
                self.profile = None;
                self.people_open = false;
                self.backend.send(Command::Logout);
            }
            SidebarAction::SwitchAccount(index) => {
                self.profile = None;
                self.people_open = false;
                self.switch_account(index);
            }
            SidebarAction::RemoveAccount(index) => self.forget_account(index),
        }
    }

    fn draw_people(&mut self, root: &mut egui::Ui) {
        let mut open: Vec<i64> = Vec::new();
        let mut open_me = false;
        egui::Panel::right("people")
            .default_size(280.0)
            .show(root, |ui| {
                ui.label(RichText::new("People").strong());
                let response = ui.add(
                    egui::TextEdit::singleline(&mut self.user_query)
                        .hint_text("Search name or username")
                        .desired_width(f32::INFINITY),
                );
                if response.changed() {
                    let query = self.user_query.trim().to_string();
                    if query.chars().count() >= 2 {
                        self.backend.send(Command::SearchUsers { query });
                    }
                }
                ui.separator();
                let results = self.user_results.clone();
                for user in results {
                    let avatar = user.avatar_url.clone();
                    let user_id = user.id;
                    ui.horizontal(|ui| {
                        self.avatar(ui, &avatar, 26.0);
                        self.presence_dot(ui, user_id);
                        if ui
                            .selectable_label(false, user.label().to_string())
                            .on_hover_text(format!("@{}", user.username))
                            .clicked()
                        {
                            open.push(user.id);
                        }
                    });
                }
                if self.user_results.is_empty() {
                    ui.label(
                        RichText::new("Type at least two characters to search.")
                            .weak()
                            .small(),
                    );
                }
                ui.separator();
                if ui.button("View my own profile").clicked() {
                    open_me = true;
                }
            });
        if open_me {
            let username = self.username.clone();
            if !username.is_empty() {
                self.load_profile_by_username(&username);
            }
        }
        for id in open {
            self.load_profile_by_id(id);
        }
    }

    fn draw_profile(&mut self, root: &mut egui::Ui) {
        let profile = self.profile.clone();
        let mut go_back = false;
        let mut open_dm: Option<(Sub, String)> = None;
        egui::CentralPanel::default().show(root, |ui| {
            ui.horizontal(|ui| {
                if ui.button("← Back").clicked() {
                    go_back = true;
                }
                ui.label(RichText::new("Profile").strong().size(16.0));
            });
            ui.separator();
            if self.profile_loading {
                ui.centered_and_justified(|ui| {
                    ui.label(RichText::new("Loading profile…").size(18.0));
                });
                return;
            }
            let Some(profile) = profile else {
                ui.label(RichText::new("Profile unavailable").weak());
                return;
            };
            let avatar = profile.user.avatar_url.clone();
            self.avatar(ui, &avatar, 88.0);
            ui.add_space(8.0);
            ui.label(RichText::new(profile.user.label()).strong().size(22.0));
            ui.label(RichText::new(format!("@{}", profile.user.username)).weak());
            if !profile.user.bio.is_empty() {
                ui.add(egui::Label::new(RichText::new(&profile.user.bio)).wrap());
            }
            ui.add_space(6.0);
            ui.label(
                RichText::new(format!(
                    "Joined {} · status {}",
                    format_time(profile.user.created_at),
                    if profile.user.status.is_empty() {
                        "unknown"
                    } else {
                        &profile.user.status
                    }
                ))
                .weak()
                .small(),
            );
            if !profile.relationship.is_empty() {
                ui.label(
                    RichText::new(format!("Relationship: {}", profile.relationship.describe()))
                        .weak()
                        .small(),
                );
            }
            if profile.user.id == self.my_id {
                ui.label(RichText::new("(this is you)").small().weak());
            }
            ui.add_space(12.0);
            let is_me = profile.user.id == self.my_id;
            ui.add_enabled_ui(!is_me, |ui| {
                if ui.button("Send a message").clicked() {
                    if let Some(conversation) = self
                        .conversations
                        .iter()
                        .find(|c| c.peer_id == profile.user.id)
                        .cloned()
                    {
                        open_dm = Some((
                            Sub {
                                scope: Scope::Direct,
                                id: conversation.id,
                            },
                            conversation.label(),
                        ));
                    }
                }
            });
        });
        if go_back {
            self.profile = None;
            self.profile_loading = false;
        }
        if let Some((sub, label)) = open_dm {
            self.profile = None;
            self.open_sub(sub, label);
        }
    }

    fn draw_conversation(&mut self, root: &mut egui::Ui) {
        let mut load_older = None;
        let mut send = false;
        egui::CentralPanel::default().show(root, |ui| {
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                if self.active_label.is_empty() {
                    ui.label(RichText::new("Select a channel or conversation").weak());
                } else {
                    ui.label(RichText::new(&self.active_label).strong().size(16.0));
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let (dot, tint) = if self.realtime {
                        ("● live", Color32::from_rgb(120, 200, 130))
                    } else {
                        ("● offline", Color32::from_rgb(210, 160, 90))
                    };
                    ui.label(RichText::new(dot).color(tint).small());
                });
            });
            if let Some(error) = &self.error {
                ui.colored_label(Color32::from_rgb(230, 110, 110), error);
            } else if !self.status.is_empty() {
                ui.colored_label(Color32::from_rgb(200, 170, 110), &self.status);
            }
            ui.separator();

            let composer_height = 30.0;
            let list_height = (ui.available_height() - composer_height - 12.0).max(60.0);
            egui::ScrollArea::vertical()
                .stick_to_bottom(true)
                .max_height(list_height)
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    if let Some(first) = self.messages.first() {
                        if ui.button("Load older messages").clicked() {
                            load_older = Some(first.id);
                        }
                    }
                    let my_id = self.my_id;
                    let messages = self.messages.clone();
                    for message in &messages {
                        self.draw_message(ui, message, my_id);
                    }
                    if !self.typing.is_empty() {
                        let names: Vec<String> =
                            self.typing.values().map(|(name, _)| name.clone()).collect();
                        ui.label(
                            RichText::new(format!("{} is typing…", names.join(", ")))
                                .weak()
                                .italics(),
                        );
                    }
                });
            if let Some(before) = load_older {
                if let Some(sub) = self.active {
                    self.backend.send(Command::LoadOlder { sub, before });
                }
            }

            ui.add_space(4.0);
            self.poll_attachments(ui);
            let mut attach = false;
            ui.horizontal(|ui| {
                let response = ui.add_sized(
                    [
                        composer_width(ui.available_width(), ui.spacing().item_spacing.x),
                        composer_height,
                    ],
                    composer_edit(&mut self.composer),
                );
                self.composer_ready = response.has_focus();
                if response.changed() && !self.composer.trim().is_empty() {
                    if self.last_typing.elapsed() > Duration::from_secs(2) {
                        self.last_typing = Instant::now();
                        if let Some(sub) = self.active {
                            self.backend.send(Command::Typing(sub));
                        }
                    }
                }
                attach = ui
                    .add_sized(
                        [ATTACH_BUTTON_WIDTH, composer_height],
                        egui::Button::new("📎"),
                    )
                    .on_hover_text("Attach the image or file on the clipboard (Ctrl+Shift+V)")
                    .clicked();
                let clicked = ui
                    .add_sized(
                        [SEND_BUTTON_WIDTH, composer_height],
                        egui::Button::new("Send"),
                    )
                    .clicked();
                if (clicked || composer_enter_requested(ui, &response))
                    && !self.composer.trim().is_empty()
                {
                    send = true;
                }
            });
            if attach {
                self.attach_from_clipboard(ui);
            }
            if self.uploading > 0 {
                ui.label(
                    RichText::new(format!("attaching {}…", self.uploading))
                        .small()
                        .weak(),
                );
            }
        });
        if send {
            self.submit();
        }
    }

    fn poll_attachments(&mut self, ui: &egui::Ui) {
        if self.active.is_none() {
            return;
        }
        let armed = paste_armed(self.composer_ready, ui.memory(|m| m.focused()));
        if armed && paste_gesture(ui) {
            self.attach_from_clipboard(ui);
        }
        let dropped: Vec<std::path::PathBuf> = ui.input(|i| {
            i.raw
                .dropped_files
                .iter()
                .map(|file| file.path().to_path_buf())
                .collect()
        });
        for path in dropped {
            self.uploading += 1;
            self.error = None;
            let name = path
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_else(|| "file".to_string());
            self.status = format!("uploading {name}...");
            self.backend.send(Command::UploadPath { path });
        }
    }

    fn attach_from_clipboard(&mut self, ui: &egui::Ui) {
        ui.ctx().input_mut(|i| {
            i.events
                .retain(|event| !matches!(event, egui::Event::Paste(_)));
        });
        self.error = None;
        match clipboard_attachment() {
            Ok(ClipAttachment::Image(bytes)) => {
                self.uploading += 1;
                self.status = "uploading pasted image...".to_string();
                self.backend.send(Command::UploadBytes {
                    name: "pasted-image.png".to_string(),
                    content_type: "image/png".to_string(),
                    bytes,
                });
            }
            Ok(ClipAttachment::File(path)) => {
                self.uploading += 1;
                let name = path
                    .file_name()
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or_else(|| "file".to_string());
                self.status = format!("uploading {name}...");
                self.backend.send(Command::UploadPath { path });
            }
            Err(reason) => {
                self.error = Some(format!(
                    "{reason} - use the attach button or drag a file onto the window"
                ));
            }
            Ok(ClipAttachment::PlainText(text)) => {
                let line = single_line(&text);
                if !line.is_empty() {
                    append_to_composer(&mut self.composer, &line);
                }
            }
        }
    }

    fn open_external(&mut self, url: &str) {
        let target = crate::api::absolute_media_url(&self.base, url);
        self.status = format!("opening {target}");
        self.rt.spawn(async move {
            let _ = tokio::task::spawn_blocking(move || {
                let _ = std::process::Command::new("xdg-open")
                    .arg(&target)
                    .stdout(std::process::Stdio::null())
                    .stderr(std::process::Stdio::null())
                    .spawn();
            })
            .await;
        });
    }

    fn draw_body(&mut self, ui: &mut egui::Ui, body: &str) {
        for part in crate::model::parse_body(body) {
            match part {
                crate::model::BodyPart::Text(text) => {
                    if !text.trim().is_empty() {
                        ui.add(
                            egui::Label::new(
                                RichText::new(text).color(Color32::from_rgb(228, 228, 232)),
                            )
                            .wrap(),
                        );
                    }
                }
                crate::model::BodyPart::Image { alt, url } => {
                    self.draw_attachment_image(ui, &alt, &url);
                }
                crate::model::BodyPart::Link { text, url } => {
                    let link = ui
                        .add(
                            egui::Label::new(
                                RichText::new(text).color(Color32::from_rgb(130, 180, 250)),
                            )
                            .wrap()
                            .sense(egui::Sense::click()),
                        )
                        .on_hover_text(&url);
                    if link.clicked() {
                        self.open_external(&url);
                    }
                }
            }
        }
    }

    fn draw_attachment_image(&mut self, ui: &mut egui::Ui, alt: &str, url: &str) {
        match self.avatar_texture(url) {
            Some(texture) => {
                let size = texture.size();
                let target = fit_attachment_size(size[0], size[1], ATTACHMENT_MAX_EDGE);
                let response = ui
                    .add(
                        egui::Image::new(&texture)
                            .fit_to_exact_size(target)
                            .sense(egui::Sense::click()),
                    )
                    .on_hover_text(if alt.is_empty() {
                        "click to open full size".to_string()
                    } else {
                        format!("{alt} - click to open full size")
                    });
                if response.clicked() {
                    self.open_external(url);
                }
            }
            None => {
                ui.horizontal(|ui| {
                    ui.label(RichText::new("loading attachment").weak().small());
                });
            }
        }
    }

    fn draw_message(&mut self, ui: &mut egui::Ui, message: &Message, my_id: i64) {
        if message.deleted_at.is_some() {
            ui.label(RichText::new("message deleted").weak().italics());
            return;
        }
        let color = if message.user_id == my_id {
            Color32::from_rgb(150, 190, 245)
        } else {
            parse_color(&message.role_color).unwrap_or(Color32::from_rgb(200, 200, 205))
        };
        let mut open_profile = false;
        let avatar = message.avatar_url.clone();
        let label = message.label().to_string();
        let username = message.username.clone();
        ui.horizontal(|ui| {
            self.avatar(ui, &avatar, 32.0);
            if ui
                .label(RichText::new(&label).strong().color(color))
                .on_hover_text(format!("@{username} — click for profile"))
                .clicked()
            {
                open_profile = true;
            }
            if message.is_bot {
                ui.label(RichText::new("BOT").small().weak());
            }
            ui.label(
                RichText::new(format_time(message.created_at))
                    .small()
                    .weak(),
            );
            if message.edited_at.is_some() {
                ui.label(RichText::new("(edited)").small().weak());
            }
        });
        if open_profile {
            self.load_profile_by_id(message.user_id);
        }
        if !message.body.is_empty() {
            self.draw_body(ui, &message.body);
        }
        if let Some(image_url) = message.image_url.clone() {
            let image = self.avatar_texture(&image_url);
            match image {
                Some(texture) => {
                    let size = texture.size();
                    let target = fit_attachment_size(size[0], size[1], ATTACHMENT_MAX_EDGE);
                    ui.add(egui::Image::new(&texture).fit_to_exact_size(target));
                }
                None => {
                    ui.label(RichText::new("[attachment]").weak());
                }
            }
        }
        if !message.reactions.is_empty() {
            let reactions = message.reactions.clone();
            let mut toggled = None;
            ui.horizontal(|ui| {
                for reaction in reactions {
                    let text = format!("{} {}", reaction.emoji, reaction.count);
                    if ui
                        .selectable_label(reaction.me, text)
                        .on_hover_text("Toggle reaction")
                        .clicked()
                    {
                        toggled = Some(reaction.emoji.clone());
                    }
                }
            });
            if let Some(emoji) = toggled {
                if let Some(sub) = self.active {
                    let _ = sub;
                    self.backend.send(Command::ToggleReaction {
                        message_id: message.id,
                        emoji,
                    });
                }
            }
        }
    }

    fn avatar_texture(&mut self, url: &str) -> Option<egui::TextureHandle> {
        if url.trim().is_empty() || self.avatars.failed.contains_key(url) {
            return None;
        }
        if self.avatars.wants(url) {
            self.backend.send(Command::FetchAvatar {
                key: url.to_string(),
                url: url.to_string(),
            });
        }
        self.avatars.textures.get(url).cloned()
    }

    fn draw_voice(&mut self, ctx: &egui::Context, room: Arc<Room>) {
        let mut actions: Vec<VoiceAction> = Vec::new();
        let snapshot = room.media().snapshot();
        let roster = self.roster.clone();
        let speaking = self.speaking.clone();
        let my_id = self.my_id;
        let muted = self.muted;
        let deafened = self.deafened;
        let sharing = self.sharing;
        let note = self.voice_note.clone();
        let ringing = self.outgoing_call;

        egui::Window::new(match room.id.kind {
            RoomKind::Voice => "Voice channel",
            RoomKind::Call => "Call",
        })
        .collapsible(false)
        .resizable(false)
        .anchor(egui::Align2::RIGHT_BOTTOM, egui::vec2(-16.0, -16.0))
        .show(ctx, |ui| {
            egui::ScrollArea::vertical()
                .max_height(240.0)
                .show(ui, |ui| {
                    for participant in &roster {
                        let is_me = participant.user_id == my_id;
                        let label = if is_me {
                            format!("{} (you)", participant.profile.label())
                        } else {
                            participant.profile.label().to_string()
                        };
                        let mut marks = Vec::new();
                        if participant.muted {
                            marks.push("🔇");
                        }
                        if participant.deafened {
                            marks.push("🎧");
                        }
                        if participant.screen {
                            marks.push("🖥");
                        }
                        if speaking.get(&participant.user_id).copied().unwrap_or(false) {
                            marks.push("🔊");
                        }
                        let avatar = participant.profile.avatar_url.clone();
                        let link = snapshot.links.get(&participant.user_id);
                        let link_note = match link {
                            Some(crate::media::PeerLink::Connected) => String::new(),
                            Some(crate::media::PeerLink::Connecting) => "connecting".to_string(),
                            Some(crate::media::PeerLink::Failed(reason)) => {
                                format!("connection failed: {reason}")
                            }
                            Some(crate::media::PeerLink::Closed) => "disconnected".to_string(),
                            None => String::new(),
                        };
                        ui.horizontal(|ui| {
                            self.avatar(ui, &avatar, 22.0);
                            let text = if link_note.is_empty() {
                                label
                            } else {
                                format!("{label} ({link_note})")
                            };
                            if link_note.is_empty() {
                                ui.label(text);
                            } else {
                                ui.label(RichText::new(text).weak());
                            }
                            if !marks.is_empty() {
                                ui.label(RichText::new(marks.join(" ")).small());
                            }
                        });
                    }
                    if roster.is_empty() {
                        ui.label(RichText::new("Waiting for others to join…").weak());
                    }
                });
            ui.separator();
            if let Some(error) = &snapshot.capture_error {
                ui.colored_label(Color32::from_rgb(230, 110, 110), error);
            }
            if !snapshot.mic_working && snapshot.capture_error.is_none() {
                ui.label(RichText::new("Microphone is starting…").weak().small());
            }
            if snapshot.screen_active {
                ui.label(RichText::new("Sharing your screen").small());
            }
            if let Some(note) = note {
                ui.label(RichText::new(note).small().weak());
            }
            if let Some(id) = ringing {
                ui.label(RichText::new(format!("Ringing call {id}…")).small().weak());
            }
            ui.horizontal(|ui| {
                if ui
                    .selectable_label(muted, if muted { "🔇 Muted" } else { "🎙 Mute" })
                    .clicked()
                {
                    actions.push(VoiceAction::ToggleMute);
                }
                if ui
                    .selectable_label(
                        deafened,
                        if deafened {
                            "🎧 Deafened"
                        } else {
                            "🎧 Deafen"
                        },
                    )
                    .clicked()
                {
                    actions.push(VoiceAction::ToggleDeafen);
                }
                if ui
                    .selectable_label(
                        sharing,
                        if sharing {
                            "🖥 Stop share"
                        } else {
                            "🖥 Share"
                        },
                    )
                    .on_hover_text("Share your screen with everyone in this room")
                    .clicked()
                {
                    actions.push(VoiceAction::ToggleShare);
                }
                if ui.button("Leave").clicked() {
                    actions.push(VoiceAction::Leave);
                }
            });
        });

        for action in actions {
            self.run_voice(action, room.clone());
        }
    }

    fn run_voice(&mut self, action: VoiceAction, room: Arc<Room>) {
        match action {
            VoiceAction::ToggleMute => {
                self.muted = !self.muted;
                let patch = RoomPatch::new(room.id.kind)
                    .muted(self.muted)
                    .deafened(self.deafened);
                self.backend.send(Command::PatchRoom { patch });
            }
            VoiceAction::ToggleDeafen => {
                self.deafened = !self.deafened;
                if self.deafened {
                    self.muted = true;
                }
                let patch = RoomPatch::new(room.id.kind)
                    .muted(self.muted)
                    .deafened(self.deafened);
                self.backend.send(Command::PatchRoom { patch });
            }
            VoiceAction::ToggleShare => {
                let handle = room.clone();
                self.rt.spawn(async move {
                    let _ = handle.toggle_screen().await;
                });
                self.sharing = !self.sharing;
                let patch = RoomPatch::new(room.id.kind).screen(self.sharing);
                self.backend.send(Command::PatchRoom { patch });
            }
            VoiceAction::Leave => {
                let id = room.id;
                self.leave_room();
                if id.kind == RoomKind::Call {
                    self.backend.send(Command::CallCancel {
                        conversation_id: id.id,
                    });
                }
            }
        }
    }

    fn draw_incoming_call(&mut self, ctx: &egui::Context, invite: CallInvite) {
        let mut accept = false;
        let mut decline = false;
        egui::Window::new("Incoming call")
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
            .show(ctx, |ui| {
                let name = if invite.caller_name.is_empty() {
                    invite.caller_username.clone()
                } else {
                    invite.caller_name.clone()
                };
                ui.label(RichText::new(format!("Incoming call from {}", name)).size(16.0));
                ui.add_space(10.0);
                ui.horizontal(|ui| {
                    if ui.button("Accept").clicked() {
                        accept = true;
                    }
                    if ui.button("Decline").clicked() {
                        decline = true;
                    }
                });
            });
        if accept {
            let conversation_id = invite.conversation_id;
            self.incoming = None;
            self.accept_call(conversation_id);
        }
        if decline {
            let conversation_id = invite.conversation_id;
            self.incoming = None;
            self.backend.send(Command::CallDecline { conversation_id });
        }
    }
}

fn presence_color(status: &str) -> Color32 {
    match status {
        "online" => Color32::from_rgb(82, 217, 82),
        "idle" | "busy" => Color32::from_rgb(255, 193, 7),
        "dnd" => Color32::from_rgb(230, 90, 90),
        _ => Color32::from_rgb(80, 84, 96),
    }
}

fn parse_color(raw: &str) -> Option<Color32> {
    let hex = raw.trim().trim_start_matches('#');
    if hex.len() != 6 {
        return None;
    }
    let value = u32::from_str_radix(hex, 16).ok()?;
    Some(Color32::from_rgb(
        ((value >> 16) & 0xff) as u8,
        ((value >> 8) & 0xff) as u8,
        (value & 0xff) as u8,
    ))
}

fn format_time(ms: i64) -> String {
    if ms <= 0 {
        return String::new();
    }
    let seconds = ms / 1000;
    let day = seconds.rem_euclid(86_400);
    format!("{:02}:{:02}", day / 3600, (day % 3600) / 60)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ui_thread_can_spawn_through_the_runtime_handle() {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .unwrap();
        let rt = runtime.handle().clone();

        let (tx, rx) = std::sync::mpsc::channel();

        std::thread::spawn(move || {
            rt.spawn(async move {
                let _ = tx.send("joined");
            });
        })
        .join()
        .unwrap();

        assert_eq!(rx.recv_timeout(Duration::from_secs(5)).unwrap(), "joined");
    }

    #[test]
    fn images_are_always_drawn_in_the_fixed_box() {
        assert_eq!(IMAGE_BOX, 20.0);

        let solid = |w: usize, h: usize| egui::ColorImage::new([w, h], vec![Color32::GRAY; w * h]);
        let ctx = egui::Context::default();

        let big = ctx.load_texture("big", solid(2048, 1024), egui::TextureOptions::LINEAR);
        let small = ctx.load_texture("small", solid(16, 16), egui::TextureOptions::LINEAR);

        for handle in [&big, &small] {
            assert_eq!(fixed_box(handle).size, egui::vec2(20.0, 20.0));
        }
    }

    fn enter_seen_by_composer(use_composer_edit: bool) -> bool {
        fn edit<'a>(text: &'a mut String, use_composer_edit: bool) -> egui::TextEdit<'a> {
            if use_composer_edit {
                composer_edit(text)
            } else {
                egui::TextEdit::singleline(text)
            }
        }

        let ctx = egui::Context::default();
        let mut text = String::from("hello");

        let mut focus_frame = egui::RawInput::default();
        focus_frame.focused = true;
        ctx.run_ui(focus_frame, |ui| {
            let response = ui.add(edit(&mut text, use_composer_edit));
            response.request_focus();
        })
        .drop_without_applying_deltas();

        let mut enter_frame = egui::RawInput::default();
        enter_frame.focused = true;
        enter_frame.events.push(egui::Event::Key {
            key: egui::Key::Enter,
            physical_key: Some(egui::Key::Enter),
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers::NONE,
        });

        let mut seen = false;
        ctx.run_ui(enter_frame, |ui| {
            let response = ui.add(edit(&mut text, use_composer_edit));
            seen = composer_enter_requested(ui, &response);
        })
        .drop_without_applying_deltas();

        seen
    }

    #[test]
    fn default_singleline_editor_hides_the_enter_press() {
        assert!(
            !enter_seen_by_composer(false),
            "egui's default singleline editor is expected to drop focus on Enter"
        );
    }

    #[test]
    fn enter_in_the_composer_requests_a_send() {
        assert!(
            enter_seen_by_composer(true),
            "Enter in the chat composer must be detected while the composer keeps focus"
        );
    }

    #[test]
    fn plain_ctrl_v_never_reaches_us_so_images_need_the_shift_shortcut() {
        let ctrl = egui::Modifiers {
            command: true,
            ..egui::Modifiers::CTRL
        };
        let ctrl_shift = egui::Modifiers {
            shift: true,
            ..ctrl
        };
        assert!(
            !paste_shortcut(ctrl, true),
            "egui-winit swallows ctrl+v and only emits Event::Paste when the clipboard has text, \
             so an image paste produces no key event at all"
        );
        assert!(
            paste_shortcut(ctrl_shift, true),
            "ctrl+shift+v is not a paste command in egui-winit, so it must reach the attachment path"
        );
        assert!(!paste_shortcut(ctrl_shift, false));
        assert!(!paste_shortcut(egui::Modifiers::NONE, true));
    }

    #[test]
    fn ctrl_shift_v_reaches_the_paste_gesture_in_a_real_frame() {
        let ctx = egui::Context::default();
        let mut frame = egui::RawInput::default();
        frame.focused = true;
        let held = egui::Modifiers {
            shift: true,
            ctrl: true,
            command: true,
            mac_cmd: false,
            alt: false,
        };
        frame.events.push(egui::Event::ModifiersChanged(held));
        frame.events.push(egui::Event::Key {
            key: egui::Key::V,
            physical_key: Some(egui::Key::V),
            pressed: true,
            repeat: false,
            modifiers: held,
        });
        let mut seen = false;
        ctx.run_ui(frame, |ui| seen = paste_gesture(ui))
            .drop_without_applying_deltas();
        assert!(seen, "ctrl+shift+v must be seen as a paste gesture");

        let ctx = egui::Context::default();
        let mut plain = egui::RawInput::default();
        plain.focused = true;
        let ctrl = egui::Modifiers {
            ctrl: true,
            command: true,
            ..egui::Modifiers::default()
        };
        plain.events.push(egui::Event::ModifiersChanged(ctrl));
        plain.events.push(egui::Event::Key {
            key: egui::Key::V,
            physical_key: Some(egui::Key::V),
            pressed: true,
            repeat: false,
            modifiers: ctrl,
        });
        let mut plain_seen = false;
        ctx.run_ui(plain, |ui| plain_seen = paste_gesture(ui))
            .drop_without_applying_deltas();
        assert!(
            !plain_seen,
            "plain ctrl+v is left to egui, which only reports a text paste"
        );

        let ctx = egui::Context::default();
        let mut text_frame = egui::RawInput::default();
        text_frame.focused = true;
        text_frame
            .events
            .push(egui::Event::Paste("hello".to_string()));
        let mut from_event = false;
        ctx.run_ui(text_frame, |ui| from_event = paste_gesture(ui))
            .drop_without_applying_deltas();
        assert!(from_event, "a text paste is still handled as a gesture");
    }

    #[test]
    fn paste_is_armed_for_the_composer_or_for_an_idle_window() {
        let focused = egui::Id::new("composer");
        assert!(paste_armed(true, Some(focused)));
        assert!(
            paste_armed(false, None),
            "with no focused widget the paste gesture must still be accepted"
        );
        assert!(
            !paste_armed(false, Some(focused)),
            "another text field, like the people search, keeps its own paste"
        );
    }

    #[test]
    fn a_failed_avatar_is_retried_before_it_is_given_up_on() {
        let mut cache = AvatarCache::default();
        let url = "/api/media/me.png";
        assert!(cache.wants(url), "the first look requests the avatar");
        assert!(!cache.wants(url), "one request in flight is enough");
        cache.requested.remove(url);
        assert!(
            cache.wants(url),
            "a transient failure must not poison the avatar forever"
        );
        for _ in 0..AVATAR_MAX_ATTEMPTS {
            cache.requested.remove(url);
            cache.wants(url);
        }
        cache.requested.remove(url);
        assert!(!cache.wants(url), "the cache stops after a few attempts");
        assert!(cache.failed.contains_key(url));
    }

    #[test]
    fn the_attach_button_is_clickable_at_every_width() {
        fn composer_row(ui: &mut egui::Ui, width: f32, out: &mut Option<egui::Response>) {
            ui.set_width(width);
            let mut composer = String::new();
            *out = Some(
                ui.horizontal(|ui| {
                    ui.add_sized(
                        [
                            composer_width(ui.available_width(), ui.spacing().item_spacing.x),
                            30.0,
                        ],
                        composer_edit(&mut composer),
                    );
                    let attach = ui.add_sized([ATTACH_BUTTON_WIDTH, 30.0], egui::Button::new("📎"));
                    ui.add_sized([SEND_BUTTON_WIDTH, 30.0], egui::Button::new("Send"));
                    attach
                })
                .inner,
            );
        }

        for available in [140.0, 200.0, 360.0, 800.0] {
            let ctx = egui::Context::default();
            let screen = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(available, 400.0));
            let mut frame = egui::RawInput::default();
            frame.focused = true;
            frame.screen_rect = Some(screen);
            frame.viewport_id = egui::ViewportId::ROOT;
            let mut attach = None;
            ctx.run_ui(frame, |ui| composer_row(ui, available, &mut attach))
                .drop_without_applying_deltas();
            let attach_rect = attach.expect("the row renders the attach button").rect;

            assert!(
                attach_rect.width() > 1.0 && attach_rect.height() > 1.0,
                "the attach button collapses at available={available}"
            );
            assert!(
                attach_rect.max.x <= available + 0.5,
                "the attach button runs past the panel at available={available}"
            );

            let mut click = egui::RawInput::default();
            click.focused = true;
            click.screen_rect = Some(screen);
            click.viewport_id = egui::ViewportId::ROOT;
            click
                .events
                .push(egui::Event::PointerMoved(attach_rect.center()));
            click.events.push(egui::Event::PointerButton {
                pos: attach_rect.center(),
                button: egui::PointerButton::Primary,
                pressed: true,
                modifiers: egui::Modifiers::NONE,
            });
            click.events.push(egui::Event::PointerButton {
                pos: attach_rect.center(),
                button: egui::PointerButton::Primary,
                pressed: false,
                modifiers: egui::Modifiers::NONE,
            });
            let mut hit = None;
            ctx.run_ui(click, |ui| composer_row(ui, available, &mut hit))
                .drop_without_applying_deltas();
            assert!(
                hit.map(|response| response.clicked()).unwrap_or(false),
                "the attach button does not register a click"
            );
        }
    }

    #[test]
    fn the_composer_makes_room_for_the_attach_button() {
        let with_send_only = 500.0 - SEND_BUTTON_WIDTH - 8.0;
        let width = composer_width(500.0, 8.0);
        assert_eq!(width, with_send_only - ATTACH_BUTTON_WIDTH - 8.0);
        assert!(width > 0.0, "the composer keeps a usable width at 500px");
        assert!(composer_width(10.0, 8.0) >= MIN_COMPOSER_WIDTH);
    }

    #[test]
    fn attachments_are_large_enough_to_actually_see() {
        assert!(ATTACHMENT_MAX_EDGE >= 120.0);
        let small = fit_attachment_size(64, 64, ATTACHMENT_MAX_EDGE);
        assert_eq!(
            small,
            egui::vec2(64.0, 64.0),
            "small images are not upscaled"
        );
        let huge = fit_attachment_size(4000, 3000, ATTACHMENT_MAX_EDGE);
        assert_eq!(
            huge,
            egui::vec2(ATTACHMENT_MAX_EDGE, ATTACHMENT_MAX_EDGE * 0.75)
        );
        let tall = fit_attachment_size(300, 1200, ATTACHMENT_MAX_EDGE);
        assert_eq!(tall.x < tall.y, true, "aspect ratio is preserved");
        assert!(tall.x <= ATTACHMENT_MAX_EDGE && tall.y <= ATTACHMENT_MAX_EDGE);
        let degenerate = fit_attachment_size(0, 0, ATTACHMENT_MAX_EDGE);
        assert!(degenerate.x > 0.0 && degenerate.y > 0.0);
    }

    #[test]
    fn pasted_text_stays_on_one_line() {
        assert_eq!(single_line("hello   world\nagain"), "hello world again");
        assert_eq!(single_line("  \n "), "");
        let mut composer = String::from("hi");
        append_to_composer(&mut composer, &single_line("there\nfriend"));
        assert_eq!(composer, "hi there friend");
    }

    #[test]
    fn clipboard_text_resolves_to_a_real_file() {
        assert_eq!(path_from_clipboard_text(""), None);
        assert_eq!(
            path_from_clipboard_text("just some words"),
            None,
            "ordinary pasted prose must not be mistaken for a file, or pasting text would raise an upload error"
        );
        assert_eq!(path_from_clipboard_text("/does/not/exist.png"), None);

        let dir = std::env::temp_dir();
        let path = dir.join("plainwire-clip-test.png");
        std::fs::write(&path, b"x").unwrap();

        let plain = path.to_string_lossy().to_string();
        assert_eq!(path_from_clipboard_text(&plain), Some(path.clone()));

        let uri = format!("file://{}", plain.replace(' ', "%20"));
        assert_eq!(path_from_clipboard_text(&uri), Some(path.clone()));

        let multi = format!("#comment\nnot a path\n{uri}\n");
        assert_eq!(path_from_clipboard_text(&multi), Some(path.clone()));

        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn the_composer_never_gets_a_negative_width() {
        assert_eq!(composer_width(400.0, 8.0), 400.0 - 64.0 - 34.0 - 16.0);
        for available in [0.0, 1.0, 20.0, 71.0, 72.0, 100.0] {
            let width = composer_width(available, 8.0);
            assert!(
                width >= MIN_COMPOSER_WIDTH && width.is_finite(),
                "composer width {width} is unusable at available={available}"
            );
        }
    }

    #[test]
    fn an_uploaded_paste_ends_up_as_a_picture_in_the_message() {
        let uploaded = crate::api::Uploaded {
            name: "image.png".to_string(),
            content_type: "image/png".to_string(),
            url: "/api/files/4lnbEp-b7NTbZqMxHi38pfFWin5THA4R".to_string(),
        };
        let markup =
            crate::api::attachment_markup(&uploaded.name, &uploaded.content_type, &uploaded.url);
        assert_eq!(
            markup, "![image.png](/api/files/4lnbEp-b7NTbZqMxHi38pfFWin5THA4R)",
            "the composer shows the same markdown the web client inserts"
        );

        let mut composer = String::new();
        append_to_composer(&mut composer, &markup);

        let parts = crate::model::parse_body(&composer);
        assert_eq!(
            parts,
            vec![crate::model::BodyPart::Image {
                alt: "image.png".to_string(),
                url: uploaded.url.clone(),
            }],
            "the sent body must render as a picture, never as raw markdown text"
        );
    }

    #[test]
    fn attachment_markup_is_appended_without_breaking_it() {
        let mut composer = String::new();
        append_to_composer(&mut composer, "![a.png](/api/files/1)");
        assert_eq!(composer, "![a.png](/api/files/1)");

        append_to_composer(&mut composer, "![b.png](/api/files/2)");
        assert_eq!(
            composer, "![a.png](/api/files/1) ![b.png](/api/files/2)",
            "two attachments must not run together or lose their markdown"
        );

        let mut trailing_space = "hello ".to_string();
        append_to_composer(&mut trailing_space, "![c.png](/api/files/3)");
        assert_eq!(trailing_space, "hello ![c.png](/api/files/3)");
    }

    #[test]
    fn percent_decoding_handles_clipboard_file_urls() {
        assert_eq!(percent_decode_component("my%20image.png"), "my image.png");
        assert_eq!(percent_decode_component("/home/a/b.png"), "/home/a/b.png");
        assert_eq!(percent_decode_component("a%2Bb"), "a+b");
        assert_eq!(
            percent_decode_component("100%real.png"),
            "100%real.png",
            "a stray percent must not be dropped"
        );
        assert_eq!(
            percent_decode_component(&crate::api::percent_encode_component(
                "h\u{e9}llo w\u{f6}rld.png"
            )),
            "h\u{e9}llo w\u{f6}rld.png",
            "encoding then decoding must round trip"
        );
    }
}

// ts fucked my psychology but its so fucking fun
