#![allow(dead_code)]

use serde::Deserialize;

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Scope {
    Channel,
    Direct,
}

impl Scope {
    pub fn as_str(self) -> &'static str {
        match self {
            Scope::Channel => "channel",
            Scope::Direct => "direct",
        }
    }
}

#[derive(Clone, Debug, Default, Deserialize)]
pub struct User {
    #[serde(default)]
    pub id: i64,
    #[serde(default)]
    pub username: String,
    #[serde(default)]
    pub display_name: String,
    #[serde(default)]
    pub avatar_url: String,
}

impl User {
    pub fn label(&self) -> &str {
        if self.display_name.is_empty() {
            &self.username
        } else {
            &self.display_name
        }
    }
}

#[derive(Clone, Debug, Default, Deserialize)]
pub struct Server {
    #[serde(default)]
    pub id: i64,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub icon_url: String,
    #[serde(default)]
    pub role: String,
    #[serde(default)]
    pub member_count: i64,
}

#[derive(Clone, Debug, Default, Deserialize)]
pub struct Channel {
    #[serde(default)]
    pub id: i64,
    #[serde(default)]
    pub server_id: i64,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub kind: String,
    #[serde(default)]
    pub position: i64,
    #[serde(default)]
    pub topic: String,
    #[serde(default)]
    pub category_id: Option<i64>,
}

impl Channel {
    pub fn is_text(&self) -> bool {
        self.kind != "voice"
    }
}

#[derive(Clone, Debug, Default, Deserialize)]
pub struct Conversation {
    #[serde(default)]
    pub id: i64,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub peer_id: i64,
    #[serde(default)]
    pub peer_name: String,
    #[serde(default)]
    pub peer_username: String,
    #[serde(default)]
    pub last_body: Option<String>,
    #[serde(default)]
    pub unread: i64,
    #[serde(default)]
    pub updated_at: i64,
}

impl Conversation {
    pub fn label(&self) -> String {
        let name = self.peer_name.trim();
        if !name.is_empty() {
            return name.to_string();
        }
        let fallback = self.name.trim();
        if !fallback.is_empty() {
            return fallback.to_string();
        }
        if !self.peer_username.is_empty() {
            return self.peer_username.clone();
        }
        format!("Conversation {}", self.id)
    }
}

#[derive(Clone, Debug, Default, Deserialize)]
pub struct Message {
    #[serde(default)]
    pub id: i64,
    #[serde(default)]
    pub scope: String,
    #[serde(default)]
    pub scope_id: i64,
    #[serde(default)]
    pub user_id: i64,
    #[serde(default)]
    pub username: String,
    #[serde(default)]
    pub display_name: String,
    #[serde(default)]
    pub avatar_url: String,
    #[serde(default)]
    pub body: String,
    #[serde(default)]
    pub reply_to_id: Option<i64>,
    #[serde(default)]
    pub created_at: i64,
    #[serde(default)]
    pub edited_at: Option<i64>,
    #[serde(default)]
    pub deleted_at: Option<i64>,
    #[serde(default)]
    pub kind: String,
    #[serde(default)]
    pub is_bot: bool,
    #[serde(default)]
    pub role_color: String,
    #[serde(default)]
    pub pinned: bool,
    #[serde(default)]
    pub image_url: Option<String>,
    #[serde(default)]
    pub image_content_type: Option<String>,
    #[serde(default)]
    pub emoji_reactions: Vec<EmojiReaction>,
}

#[derive(Clone, Debug, Default, Deserialize)]
pub struct EmojiReaction {
    #[serde(default)]
    pub emoji: String,
    #[serde(default)]
    pub count: i64,
    #[serde(default)]
    pub me: bool,
}

impl Message {
    pub fn label(&self) -> &str {
        if self.display_name.is_empty() {
            &self.username
        } else {
            &self.display_name
        }
    }
}

#[derive(Clone, Debug, Default, Deserialize)]
pub struct Session {
    #[serde(default)]
    pub csrf: String,
    #[serde(default)]
    pub user: User,
}

#[derive(Clone, Debug, Default, Deserialize)]
pub struct SyncPayload {
    #[serde(default)]
    pub servers: Vec<Server>,
    #[serde(default)]
    pub conversations: Vec<Conversation>,
}

#[derive(Clone, Debug, Default, Deserialize)]
pub struct ServerDetail {
    #[serde(default)]
    pub server: Server,
    #[serde(default)]
    pub channels: Vec<Channel>,
}

#[derive(Clone, Debug, Default, Deserialize)]
pub struct Profile {
    #[serde(default)]
    pub id: i64,
    #[serde(default)]
    pub user_id: i64,
    #[serde(default)]
    pub username: String,
    #[serde(default)]
    pub display_name: String,
    #[serde(default)]
    pub avatar_url: String,
    #[serde(default)]
    pub bio: String,
    #[serde(default)]
    pub joined_at: i64,
    #[serde(default)]
    pub role: String,
    #[serde(default)]
    pub member_count: i64,
    #[serde(default)]
    pub roles: Vec<String>,
    #[serde(default)]
    pub servers: Vec<Server>,
    #[serde(default)]
    pub conversations: Vec<Conversation>,
}
