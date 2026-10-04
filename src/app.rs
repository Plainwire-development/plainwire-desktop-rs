use std::collections::BTreeMap;
use std::fs;
use std::sync::mpsc::Receiver;
use std::time::{Duration, Instant};
use std::path::PathBuf;

use eframe::egui::{self, Color32, RichText};

use crate::backend::{Backend, Command, Repainter, Update};
use crate::model::{Channel, Conversation, Message, Profile, Scope, Server, User};
use crate::realtime::Sub;
use crate::tray::Tray;
use crate::account::{Account, AccountStore};

#[derive(PartialEq, Eq)]
enum Screen {
    Login,
    Chat,
}

pub struct App {
    backend: Backend,
    updates: Receiver<Update>,
    repaint: Repainter,
    screen: Screen,
    base: String,
    username: String,
    password: String,
    busy: bool,
    me: Option<User>,
    servers: Vec<Server>,
    conversations: Vec<Conversation>,
    channels: Vec<Channel>,
    server_id: Option<i64>,
    active: Option<Sub>,
    active_label: String,
    profiles: Vec<Profile>,
    selected_profile: Option<i64>,
    selected_profile_username: Option<String>,
    viewing_profile: bool,
    profile_user_id: Option<i64>,
    profile_loading: bool,
    profile_error: Option<String>,
    messages: Vec<Message>,
    typing: BTreeMap<i64, (String, Instant)>,
    realtime: bool,
    status: String,
    error: Option<String>,
    composer: String,
    last_typing: Instant,
    auto_loaded: bool,
    tray: Option<Tray>,
    accounts: AccountStore,
    selected_account: Option<usize>,
}

impl App {
    pub fn new(
        backend: Backend,
        updates: Receiver<Update>,
        repaint: Repainter,
        ctx: &egui::Context,
    ) -> Self {
        ctx.set_visuals(egui::Visuals::dark());
        repaint.set(ctx.clone());
        Self {
            backend,
            updates,
            repaint,
            screen: Screen::Login,
            base: "https://plainwi.re".to_string(),
            username: String::new(),
            password: String::new(),
            busy: false,
            me: None,
            servers: Vec::new(),
            conversations: Vec::new(),
            channels: Vec::new(),
            server_id: None,
            active: None,
            active_label: String::new(),
            messages: Vec::new(),
            typing: BTreeMap::new(),
            realtime: false,
            profiles: Vec::new(),
            selected_profile: None,
            selected_profile_username: None,
            viewing_profile: false,
            profile_user_id: None,
            profile_loading: false,
            profile_error: None,
            status: String::new(),
            error: None,
            composer: String::new(),
            last_typing: Instant::now(),
            auto_loaded: false,
            tray: None,
            accounts: AccountStore::load(),
            selected_account: None,
        }
    }

    fn pump(&mut self) {
        while let Ok(update) = self.updates.try_recv() {
            self.apply(update);
        }
        let now = Instant::now();
        self.typing
            .retain(|_, (_, at)| now.duration_since(*at) < Duration::from_secs(6));
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
            Update::Sync(payload) => {
                self.servers = payload.servers;
                self.conversations = payload.conversations;
                if !self.auto_loaded {
                    if let Some(server) = self.servers.first() {
                        self.auto_loaded = true;
                        let id = server.id;
                        self.open_server(id);
                    }
                }
            }
            Update::Channels {
                server_id,
                channels,
            } => {
                if self.server_id == Some(server_id) {
                    self.channels = channels;
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
            Update::ReactionAdded { sub, message } => {
                if self.active == Some(sub) {
                    self.insert_message(message);
                }
            }
            Update::ReactionRemoved { sub, message } => {
                if self.active == Some(sub) {
                    self.insert_message(message);
                }
            }
            Update::Profiles { profiles } => {
                self.profiles = profiles;
            }
            Update::ProfileView { profile } => {
                self.viewing_profile = true;
                self.selected_profile = None;
                self.selected_profile_username = None;
                self.profile_user_id = Some(profile.id);
                self.profile_loading = false;
                self.profile_error = None;
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
                self.status.clear();
            }
            Update::LoggedOut => {
                self.screen = Screen::Login;
                self.me = None;
                self.servers.clear();
                self.conversations.clear();
                self.channels.clear();
                self.messages.clear();
                self.active = None;
                self.server_id = None;
                self.auto_loaded = false;
                self.busy = false;
                self.realtime = false;
            }
            Update::Error(message) => {
                self.busy = false;
                self.error = Some(message);
            }
            Update::Status(message) => {
                self.status = message;
            }
        }
        self.repaint.request();
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
                fs::read_dir(dir)
                    .ok()
                    .and_then(|mut entries| {
                        entries.next()
                            .map(|e| e.ok())
                            .and_then(|e| e.map(|e| e.path()))
                    })
            });
        self.init_tray_with_path(icon_path);
        self.load_account_ui();
    }

    fn init_tray_with_path(&mut self, icon_path: Option<std::path::PathBuf>) {
        let window_title = "Plainwire";
        self.tray = Some(Tray::new(icon_path));
        if let Some(ref mut tray) = self.tray {
            tray.build(window_title);
            tray.show();
        }
    }

    fn load_account_ui(&mut self) {
        self.selected_account = self
            .accounts
            .accounts
            .iter()
            .position(|a| a.username == self.username && a.base == self.base);
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

    fn load_profiles(&mut self) {
        self.profile_loading = true;
        self.profile_error = None;
        self.backend.send(Command::ListProfiles);
    }

    fn load_profile(&mut self, user_id: i64) {
        self.profile_user_id = Some(user_id);
        self.profile_loading = true;
        self.profile_error = None;
        self.backend.send(Command::ProfileByUserId { user_id });
    }

    fn load_profile_by_username(&mut self, username: &str) {
        self.selected_profile_username = Some(username.to_string());
        self.profile_loading = true;
        self.profile_error = None;
        self.backend.send(Command::ProfileByUsername {
            username: username.to_string(),
        });
    }

    fn close_profile(&mut self) {
        self.viewing_profile = false;
        self.selected_profile = None;
        self.selected_profile_username = None;
        self.profile_user_id = None;
        self.profile_loading = false;
        self.profile_error = None;
    }

    fn send_reaction(&mut self, message_id: i64, emoji: &str) {
        if let Some(sub) = self.active {
            self.backend.send(Command::ToggleReaction {
                sub,
                message_id,
                emoji: emoji.to_string(),
            });
        }
    }

    fn send_image(&mut self, sub: Sub, image_url: String, content_type: String) {
        self.backend.send(Command::SendAttachment {
            sub,
            message_id: 0,
            image_url,
            content_type,
        });
    }

    fn draw_login(&mut self, root: &mut egui::Ui) {
        egui::CentralPanel::default().show(root, |ui| {
            ui.vertical_centered(|ui| {
                ui.add_space(80.0);
                ui.heading(RichText::new("Plainwire Desktop").size(30.0));
                ui.add_space(24.0);
            });
            let width = 360.0f32.min(ui.available_width());
            ui.vertical_centered(|ui| {
                ui.allocate_ui_with_layout(
                    egui::vec2(width, 0.0),
                    egui::Layout::top_down(egui::Align::Min),
                    |ui| {
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
                        let enabled = !self.busy;
                        let button = ui.add_enabled(
                            enabled,
                            egui::Button::new(if self.busy {
                                "Connecting..."
                            } else {
                                "Connect"
                            }),
                        );
                        let enter = ui.input(|i| i.key_pressed(egui::Key::Enter));
                        if (button.clicked() || enter) && enabled {
                            self.busy = true;
                            self.error = None;
                            self.backend.send(Command::Login {
                                base: self.base.clone(),
                                username: self.username.clone(),
                                password: self.password.clone(),
                            });
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

    fn draw_chat(&mut self, root: &mut egui::Ui) {
        egui::Panel::left("navigation")
            .resizable(true)
            .default_size(250.0)
            .show(root, |ui| {
                ui.add_space(6.0);
                let me = self
                    .me
                    .as_ref()
                    .map(|u| u.label().to_string())
                    .unwrap_or_default();
                ui.horizontal(|ui| {
                    ui.label(RichText::new(me).strong());
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.small_button("sign out").clicked() {
                            self.backend.send(Command::Logout);
                        }
                    });
                });
                ui.separator();
                egui::ScrollArea::vertical().show(ui, |ui| {
                    ui.label(RichText::new("Servers").small().weak());
                    for server in self.servers.clone() {
                        let selected = self.server_id == Some(server.id);
                        if ui.selectable_label(selected, &server.name).clicked() {
                            self.open_server(server.id);
                        }
                    }
                    ui.add_space(8.0);
                    if !self.channels.is_empty() {
                        ui.label(RichText::new("Channels").small().weak());
                        for channel in self.channels.clone() {
                            if !channel.is_text() {
                                ui.label(RichText::new(format!("🔊 {}", channel.name)).weak());
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
                                self.open_sub(
                                    Sub {
                                        scope: Scope::Channel,
                                        id: channel.id,
                                    },
                                    channel.name.clone(),
                                );
                            }
                        }
                    }
                    ui.add_space(8.0);
                    ui.label(RichText::new("Accounts").small().weak());
                    if self.accounts.accounts.is_empty() {
                        ui.label(RichText::new("No accounts saved").weak());
                    } else {
                        for (i, account) in self.accounts.accounts.iter().enumerate() {
                            let selected = self.selected_account == Some(i);
                            if ui
                                .selectable_label(selected, &account.username)
                                .clicked()
                            {
                                self.selected_account = Some(i);
                                self.username = account.username.clone();
                                self.base = account.base.clone();
                            }
                        }
                    }
                    if ui.button("Save account").clicked() {
                        if let Some(index) = self.selected_account {
                            self.accounts.accounts[index] = Account {
                                base: self.base.clone(),
                                username: self.username.clone(),
                                password: self.password.clone(),
                                user_id: self.me.clone().map(|u| u.id).unwrap_or(0),
                                display_name: self
                                    .me
                                    .clone()
                                    .map(|u| u.display_name)
                                    .unwrap_or_default(),
                            };
                            if let Err(e) = self.accounts.save() {
                                self.error = Some(format!("Failed to save: {e}"));
                            }
                        } else {
                            let account = Account {
                                base: self.base.clone(),
                                username: self.username.clone(),
                                password: self.password.clone(),
                                user_id: 0,
                                display_name: String::new(),
                            };
                            self.accounts.accounts.push(account);
                            self.selected_account = Some(self.accounts.accounts.len() - 1);
                            if let Err(e) = self.accounts.save() {
                                self.error = Some(format!("Failed to save: {e}"));
                            }
                        }
                    }
                    ui.add_space(4.0);
                    if ui.button("Sign out").clicked() {
                        self.backend.send(Command::Logout);
                        self.load_account_ui();
                    }
                    ui.label(RichText::new("Direct messages").small().weak());
                    for conversation in self.conversations.clone() {
                        let selected = self.active
                            == Some(Sub {
                                scope: Scope::Direct,
                                id: conversation.id,
                            });
                        let mut text = conversation.label();
                        if conversation.unread > 0 {
                            text = format!("{}  ({})", text, conversation.unread);
                        }
                        if ui.selectable_label(selected, text).clicked() {
                            let sub = Sub {
                                scope: Scope::Direct,
                                id: conversation.id,
                            };
                            let label = conversation.label();
                            self.open_sub(sub, label);
                        }
                    }
                    ui.add_space(8.0);
                    if ui.button("Profiles").clicked() {
                        self.load_profiles();
                    }
                    if !self.profiles.is_empty() {
                        ui.add_space(4.0);
                        let profile_ids: Vec<i64> = self.profiles.iter().map(|p| p.id).collect();
                        for id in profile_ids {
                            if let Some(profile) = self.profiles.iter().find(|p| p.id == id) {
                                if ui
                                    .small_button(profile.display_name.as_str())
                                    .clicked()
                                {
                                    self.load_profile(id);
                                }
                            }
                        }
                    }
                });
            });

        if self.viewing_profile {
            self.draw_profile_screen(root);
            return;
        }

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
                            if let Some(sub) = self.active {
                                self.backend.send(Command::LoadOlder {
                                    sub,
                                    before: first.id,
                                });
                            }
                        }
                    }
                    for message in &self.messages {
                        draw_message(ui, message, self.me.as_ref().map(|u| u.id));
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

            ui.add_space(4.0);
            let response = ui.add_sized(
                [ui.available_width(), composer_height],
                egui::TextEdit::singleline(&mut self.composer)
                    .hint_text("Message")
                    .desired_width(f32::INFINITY),
            );
            if response.changed() && !self.composer.trim().is_empty() {
                if self.last_typing.elapsed() > Duration::from_secs(2) {
                    self.last_typing = Instant::now();
                    if let Some(sub) = self.active {
                        self.backend.send(Command::Typing(sub));
                    }
                }
            }
            let enter = response.has_focus()
                && ui.input(|i| i.key_pressed(egui::Key::Enter) && !i.modifiers.shift);
            if enter || (response.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter))) {
                self.submit();
            }
        });
    }

    fn draw_profile_screen(&mut self, root: &mut egui::Ui) {
        egui::CentralPanel::default().show(root, |ui| {
            if self.profile_loading {
                ui.centered_and_justified(|ui| {
                    ui.label(egui::RichText::new("Loading profile…").size(18.0));
                });
                return;
            }
            if let Some(error) = &self.profile_error {
                ui.colored_label(Color32::from_rgb(230, 110, 110), error);
                return;
            }
            let profile = match self.profile_user_id {
                Some(id) => self
                    .profiles
                    .iter()
                    .find(|p| p.id == id),
                None => {

                    if let Some(me) = &self.me {
                        self.profiles
                            .iter()
                            .find(|p| p.user_id == me.id)
                    } else {
                        None
                    }
                }
            };
            if let Some(profile) = profile {
                draw_profile_header(ui, profile, self.me.as_ref().map(|u| u.id) == Some(profile.id));
                ui.add_space(12.0);

                let mut conversation_clicks = Vec::new();
                {
                    egui::ScrollArea::vertical().show(ui, |ui| {
                        ui.label(RichText::new("Membership").small().strong());
                        ui.add_space(4.0);
                        for server in &profile.servers {
                            ui.horizontal(|ui| {
                                if !server.icon_url.is_empty() {
                                    ui.add(egui::Image::new(&server.icon_url)
                                        .max_size([24.0, 24.0].into()));
                                }
                                ui.label(&server.name);
                                ui.label(
                                    RichText::new(format!("  ({})", server.member_count))
                                        .weak()
                                        .small(),
                                );
                            });
                        }
                        ui.add_space(12.0);
                        ui.label(RichText::new("Recent conversations").small().strong());
                        ui.add_space(4.0);
                        let conversation_ids: Vec<i64> = profile.conversations.iter().map(|c| c.id).collect();
                        for id in conversation_ids {
                            if let Some(conversation) = profile.conversations.iter().find(|c| c.id == id) {
                                let selected = self.active == Some(Sub {
                                    scope: Scope::Direct,
                                    id: conversation.id,
                                });
                                if ui
                                    .selectable_label(selected, conversation.label())
                                    .clicked()
                                {
                                    let sub = Sub {
                                        scope: Scope::Direct,
                                        id: conversation.id,
                                    };
                                    let label = conversation.label();
                                    conversation_clicks.push((sub, label));
                                }
                            }
                        }
                    });
                }

                for (sub, label) in conversation_clicks {
                    self.open_sub(sub, label);
                }
            } else {
                ui.label(RichText::new("Profile not found").weak());
            }
        });
    }
}

fn draw_message(ui: &mut egui::Ui, message: &Message, me_id: Option<i64>) {
    if message.deleted_at.is_some() {
        ui.label(RichText::new("message deleted").weak().italics());
        return;
    }
    let color = if message.user_id == me_id.unwrap_or(-1) {
        Color32::from_rgb(150, 190, 245)
    } else {
        parse_color(&message.role_color).unwrap_or(Color32::from_rgb(200, 200, 205))
    };
    ui.horizontal(|ui| {
        ui.label(RichText::new(message.label()).strong().color(color));
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
    if !message.body.is_empty() {
        ui.add(
            egui::Label::new(RichText::new(&message.body).color(Color32::from_rgb(228, 228, 232)))
                .wrap(),
        );
    }
    if let Some(image_url) = &message.image_url {
        ui.add_space(4.0);
        draw_image(ui, image_url, message.image_content_type.as_deref());
    }
    ui.add_space(4.0);
    if !message.emoji_reactions.is_empty() {
        draw_reactions(ui, message, me_id);
    }
}

fn draw_image(ui: &mut egui::Ui, url: &str, content_type: Option<&str>) {
    let is_image = content_type
        .map(|ct| ct.starts_with("image/"))
        .unwrap_or(false);
    if is_image {
        ui.add(egui::Image::new(url)
            .max_size([320.0, 320.0].into()));
    } else {
        ui.label(RichText::new("[attachment]").weak());
    }
}

fn draw_reactions(ui: &mut egui::Ui, message: &Message, me_id: Option<i64>) {
    let mut items: Vec<(&str, bool)> = message
        .emoji_reactions
        .iter()
        .map(|r| {
            (
                r.emoji.as_str(),
                r.me || message.user_id == me_id.unwrap_or(-1),
            )
        })
        .collect();
    items.sort_by(|a, b| a.0.cmp(b.0));
    for (emoji, me) in items {
        let selected = me;
        if ui.selectable_label(selected, emoji).clicked() {

        }
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

fn draw_profile_header(ui: &mut egui::Ui, profile: &Profile, is_self: bool) {
    let avatar_size = 80.0f32;
    ui.add_space(12.0);
    ui.horizontal(|ui| {
        if !profile.avatar_url.is_empty() {
            ui.add(egui::Image::new(&profile.avatar_url)
                .max_size([avatar_size, avatar_size].into()));
        } else {
            ui.colored_label(Color32::from_rgb(140, 140, 150), "No avatar");
        }
        ui.add_space(12.0);
        ui.vertical(|ui| {
            ui.horizontal(|ui| {
                ui.label(
                    RichText::new(&profile.display_name)
                        .strong()
                        .size(20.0),
                );
                if is_self {
                    ui.label(RichText::new("(you)").small().weak());
                }
            });
            ui.label(RichText::new(profile.username.as_str()).weak().small());
            ui.label(RichText::new(profile.bio.as_str()).weak().small());

            ui.horizontal(|ui| {
                ui.label(
                    RichText::new(format!("Joined {}", format_time(profile.joined_at)))
                        .weak()
                        .small(),
                );
                ui.label(
                    RichText::new(format!(" · {} servers", profile.member_count))
                        .weak()
                        .small(),
                );
            });
        });
    });
    ui.add_space(8.0);
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.pump();
        ui.ctx().request_repaint_after(Duration::from_millis(400));
        match self.screen {
            Screen::Login => self.draw_login(ui),
            Screen::Chat => self.draw_chat(ui),
        }
    }
}
