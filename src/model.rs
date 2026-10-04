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
    #[serde(default)]
    pub bio: String,
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    pub created_at: i64,
    #[serde(default)]
    pub is_bot: bool,
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
    pub description: String,
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

    pub fn is_voice(&self) -> bool {
        self.kind == "voice"
    }
}

#[derive(Clone, Debug, Default, Deserialize)]
pub struct Conversation {
    #[serde(default)]
    pub id: i64,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub avatar_url: String,
    #[serde(default)]
    pub peer_id: i64,
    #[serde(default)]
    pub peer_name: String,
    #[serde(default)]
    pub peer_avatar_url: String,
    #[serde(default)]
    pub peer_username: String,
    #[serde(default)]
    pub last_body: Option<String>,
    #[serde(default)]
    pub member_count: i64,
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

    pub fn avatar(&self) -> &str {
        if !self.peer_avatar_url.is_empty() {
            &self.peer_avatar_url
        } else {
            &self.avatar_url
        }
    }
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

    #[serde(default, alias = "emoji_reactions")]
    pub reactions: Vec<EmojiReaction>,
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
    pub now: i64,
    #[serde(default)]
    pub servers: Vec<Server>,
    #[serde(default)]
    pub conversations: Vec<Conversation>,
    #[serde(default)]
    pub friends: Vec<User>,
    #[serde(default)]
    pub sync_degraded: bool,
    #[serde(default)]
    pub sync_warnings: Vec<String>,
}

#[derive(Clone, Debug, Default, Deserialize)]
pub struct ServerDetail {
    #[serde(default)]
    pub server: Server,
    #[serde(default)]
    pub channels: Vec<Channel>,
}

#[derive(Clone, Debug, Default)]
pub struct Relationship {
    pub status: String,
    pub incoming: bool,
    pub outgoing: bool,
    pub blocked_by_me: bool,
}

impl Relationship {
    pub fn is_empty(&self) -> bool {
        self.status.trim().is_empty() || self.status == "none"
    }

    pub fn describe(&self) -> String {
        if self.is_empty() {
            return String::new();
        }
        match self.status.as_str() {
            "accepted" => "friends".to_string(),
            "pending" => {
                if self.incoming {
                    "wants to be your friend".to_string()
                } else if self.outgoing {
                    "friend request sent".to_string()
                } else {
                    "friend request pending".to_string()
                }
            }
            "blocked" => {
                if self.blocked_by_me {
                    "you blocked this person".to_string()
                } else {
                    "blocked".to_string()
                }
            }
            other => other.to_string(),
        }
    }
}

impl<'de> Deserialize<'de> for Relationship {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Shape {
            Map {
                #[serde(default)]
                status: String,
                #[serde(default)]
                incoming: bool,
                #[serde(default)]
                outgoing: bool,
                #[serde(default)]
                blocked_by_me: bool,
            },
            Text(String),
            Nothing(()),
        }
        Ok(match Shape::deserialize(deserializer)? {
            Shape::Map {
                status,
                incoming,
                outgoing,
                blocked_by_me,
            } => Relationship {
                status,
                incoming,
                outgoing,
                blocked_by_me,
            },
            Shape::Text(status) => Relationship {
                status,
                ..Default::default()
            },
            Shape::Nothing(()) => Relationship::default(),
        })
    }
}

#[derive(Clone, Debug, Default, Deserialize)]
pub struct ProfileResponse {
    #[serde(default)]
    pub user: User,
    #[serde(default)]
    pub relationship: Relationship,
}

#[derive(Clone, Debug, Default)]
pub struct Profile {
    pub user: User,
    pub relationship: Relationship,
    pub servers: Vec<Server>,
    pub conversations: Vec<Conversation>,
}

impl Profile {
    pub fn id(&self) -> i64 {
        self.user.id
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum RoomKind {

    Voice,

    Call,
}

impl RoomKind {
    pub fn as_str(self) -> &'static str {
        match self {
            RoomKind::Voice => "voice",
            RoomKind::Call => "call",
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct RoomId {
    pub kind: RoomKind,
    pub id: i64,
}

impl RoomId {
    pub fn voice(id: i64) -> Self {
        RoomId {
            kind: RoomKind::Voice,
            id,
        }
    }

    pub fn call(id: i64) -> Self {
        RoomId {
            kind: RoomKind::Call,
            id,
        }
    }
}

impl std::fmt::Display for RoomId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}:{}", self.kind.as_str(), self.id)
    }
}

#[derive(Clone, Debug, Default, Deserialize)]
pub struct Participant {
    #[serde(default)]
    pub user_id: i64,
    #[serde(default)]
    pub muted: bool,
    #[serde(default)]
    pub deafened: bool,
    #[serde(default)]
    pub screen: bool,
    #[serde(default)]
    pub screen_audio: bool,
    #[serde(default)]
    pub reconnecting: bool,
    #[serde(default)]
    pub profile: User,
}

#[derive(Clone, Debug, Default, Deserialize)]
pub struct CallInvite {
    #[serde(default)]
    pub conversation_id: i64,
    #[serde(default)]
    pub invite_id: String,
    #[serde(default)]
    pub caller_id: i64,
    #[serde(default)]
    pub caller_name: String,
    #[serde(default)]
    pub caller_username: String,
    #[serde(default)]
    pub avatar_url: String,
}

#[derive(Clone, Copy, Debug)]
pub struct RoomPatch {
    pub kind: RoomKind,
    pub muted: Option<bool>,
    pub deafened: Option<bool>,
    pub screen: Option<bool>,
    pub screen_audio: Option<bool>,
}

impl RoomPatch {
    pub fn new(kind: RoomKind) -> Self {
        RoomPatch {
            kind,
            muted: None,
            deafened: None,
            screen: None,
            screen_audio: None,
        }
    }

    pub fn muted(mut self, value: bool) -> Self {
        self.muted = Some(value);
        self
    }

    pub fn deafened(mut self, value: bool) -> Self {
        self.deafened = Some(value);
        self
    }

    pub fn screen(mut self, value: bool) -> Self {
        self.screen = Some(value);
        self
    }

    pub fn screen_audio(mut self, value: bool) -> Self {
        self.screen_audio = Some(value);
        self
    }

    pub fn wire(&self) -> serde_json::Value {
        let mut map = serde_json::Map::new();
        if let Some(value) = self.muted {
            map.insert("muted".into(), serde_json::Value::Bool(value));
        }
        if let Some(value) = self.deafened {
            map.insert("deafened".into(), serde_json::Value::Bool(value));
        }
        if let Some(value) = self.screen {
            map.insert("screen".into(), serde_json::Value::Bool(value));
        }
        if let Some(value) = self.screen_audio {
            map.insert("screen_audio".into(), serde_json::Value::Bool(value));
        }
        serde_json::Value::Object(map)
    }
}

#[derive(Clone, Debug, Default, Deserialize)]
pub struct IceServer {
    #[serde(default)]
    pub urls: Vec<String>,
    #[serde(default, rename = "username")]
    pub username: Option<String>,
    #[serde(default, rename = "credential")]
    pub credential: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize)]
pub struct RtcConfig {
    #[serde(default, rename = "iceServers")]
    pub ice_servers: Vec<IceServer>,
    #[serde(default, rename = "iceTransportPolicy")]
    pub ice_transport_policy: String,
    #[serde(default, rename = "turnStatus")]
    pub turn_status: String,
    #[serde(default, rename = "refreshAfterSeconds")]
    pub refresh_after_seconds: i64,
}

#[derive(Clone, Debug, PartialEq)]
pub enum BodyPart {
    Text(String),
    Image { alt: String, url: String },
    Link { text: String, url: String },
}

pub fn parse_body(body: &str) -> Vec<BodyPart> {
    let bytes: Vec<char> = body.chars().collect();
    let mut parts: Vec<BodyPart> = Vec::new();
    let mut text = String::new();
    let mut index = 0usize;

    while index < bytes.len() {
        let is_image = bytes[index] == '!' && bytes.get(index + 1) == Some(&'[');
        let is_link = bytes[index] == '[' && !is_image;
        if !is_image && !is_link {
            text.push(bytes[index]);
            index += 1;
            continue;
        }
        let label_start = if is_image { index + 2 } else { index + 1 };
        let Some(label_end) = find_close(&bytes, label_start, '[', ']') else {
            text.push(bytes[index]);
            index += 1;
            continue;
        };
        if bytes.get(label_end + 1) != Some(&'(') {
            text.push(bytes[index]);
            index += 1;
            continue;
        }
        let url_start = label_end + 2;
        let Some(url_end) = find_close(&bytes, url_start, '(', ')') else {
            text.push(bytes[index]);
            index += 1;
            continue;
        };
        let label: String = bytes[label_start..label_end].iter().collect();
        let url: String = bytes[url_start..url_end].iter().collect();
        let url = url.trim().to_string();
        if url.is_empty() {
            text.push(bytes[index]);
            index += 1;
            continue;
        }
        if !text.is_empty() {
            parts.push(BodyPart::Text(std::mem::take(&mut text)));
        }
        parts.push(if is_image {
            BodyPart::Image { alt: label, url }
        } else {
            BodyPart::Link {
                text: label,
                url,
            }
        });
        index = url_end + 1;
    }

    if !text.is_empty() {
        parts.push(BodyPart::Text(text));
    }
    parts
}

fn find_close(chars: &[char], start: usize, open: char, close: char) -> Option<usize> {
    let mut depth = 0usize;
    let mut index = start;
    while index < chars.len() {
        let current = chars[index];
        if current == '\n' && chars.get(index + 1) == Some(&'\n') {
            return None;
        }
        if current == open {
            depth += 1;
        } else if current == close {
            if depth == 0 {
                return Some(index);
            }
            depth -= 1;
        }
        index += 1;
    }
    None
}
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_pasted_image_becomes_an_image_part() {
        assert_eq!(
            parse_body("![image.png](/api/files/4lnbEp)"),
            vec![BodyPart::Image {
                alt: "image.png".into(),
                url: "/api/files/4lnbEp".into(),
            }]
        );
    }

    #[test]
    fn attachment_markup_is_extracted_from_surrounding_text() {
        assert_eq!(
            parse_body("look at this ![cat.png](/api/files/abc) please"),
            vec![
                BodyPart::Text("look at this ".into()),
                BodyPart::Image {
                    alt: "cat.png".into(),
                    url: "/api/files/abc".into(),
                },
                BodyPart::Text(" please".into()),
            ]
        );
    }

    #[test]
    fn plain_and_broken_markup_stay_text() {
        assert_eq!(parse_body(""), Vec::<BodyPart>::new());
        assert_eq!(parse_body("hello"), vec![BodyPart::Text("hello".into())]);
        assert_eq!(
            parse_body("![no parens](/api/files/abc"),
            vec![BodyPart::Text("![no parens](/api/files/abc".into())]
        );
        assert_eq!(
            parse_body("![](/api/files/abc)"),
            vec![BodyPart::Image {
                alt: String::new(),
                url: "/api/files/abc".into(),
            }],
            "an empty alt is still valid markdown and renders as an image"
        );
        assert_eq!(
            parse_body("a [b] c"),
            vec![BodyPart::Text("a [b] c".into())]
        );
        assert_eq!(
            parse_body("an ![emoji](:) smile"),
            vec![
                BodyPart::Text("an ".into()),
                BodyPart::Image { alt: "emoji".into(), url: ":".into() },
                BodyPart::Text(" smile".into())
            ]
        );
    }

    #[test]
    fn non_image_attachments_are_links() {
        assert_eq!(
            parse_body("[notes.txt](/api/files/abc)"),
            vec![BodyPart::Link {
                text: "notes.txt".into(),
                url: "/api/files/abc".into(),
            }]
        );
    }

    #[test]
    fn several_attachments_in_one_message_all_parse() {
        let parts = parse_body("![a.png](/api/files/1) and ![b.png](/api/files/2)");
        let images = parts
            .iter()
            .filter(|p| matches!(p, BodyPart::Image { .. }))
            .count();
        assert_eq!(images, 2);
    }

    #[test]
    fn nested_brackets_in_the_url_are_tolerated() {
        assert_eq!(
            parse_body("[x](https://e.com/a(b))"),
            vec![BodyPart::Link {
                text: "x".into(),
                url: "https://e.com/a(b)".into(),
            }]
        );
    }

    #[test]
    fn room_patch_matches_the_hub_whitelist() {

        let patch = RoomPatch::new(RoomKind::Voice)
            .muted(true)
            .screen(false)
            .wire();
        let map = patch.as_object().unwrap();
        assert_eq!(map.len(), 2);
        assert_eq!(map.get("muted"), Some(&serde_json::Value::Bool(true)));
        assert_eq!(map.get("screen"), Some(&serde_json::Value::Bool(false)));
        assert!(!map.contains_key("deafened"));
        assert!(!map.contains_key("screen_audio"));
    }

    #[test]
    fn call_patch_uses_the_same_shape() {
        let patch = RoomPatch::new(RoomKind::Call).deafened(true).wire();
        assert_eq!(patch.to_string(), r#"{"deafened":true}"#);
    }

    #[test]
    fn sync_payload_tolerates_the_real_envelope() {

        let raw = r#"{
            "now": 1,
            "since": 0,
            "servers": [{"id": 7, "name": "Test", "role": "owner"}],
            "conversations": [{"id": 3, "name": "", "peer_id": 9,
                               "peer_name": "Ada", "peer_username": "ada",
                               "peer_avatar_url": "/api/media/abc",
                               "last_body": "hi", "unread": 2, "updated_at": 5}],
            "sync_degraded": false,
            "sync_warnings": []
        }"#;
        let sync: SyncPayload = serde_json::from_str(raw).unwrap();
        assert_eq!(sync.servers[0].id, 7);
        assert_eq!(sync.conversations[0].label(), "Ada");
        assert_eq!(
            sync.conversations[0].avatar(),
            "/api/media/abc",
            "the DM avatar should come from peer_avatar_url"
        );
    }

    #[test]
    fn profile_response_is_wrapped_not_flat() {

        let raw = r#"{"user":{"id":9,"username":"ada","display_name":"Ada",
                     "avatar_url":"/api/media/x","status":"online"},
                     "relationship":{"status":"accepted","incoming":false,
                                     "outgoing":false,"blocked_by_me":false}}"#;
        let response: ProfileResponse = serde_json::from_str(raw).unwrap();
        let profile = Profile {
            user: response.user,
            relationship: response.relationship,
            servers: vec![],
            conversations: vec![],
        };
        assert_eq!(profile.id(), 9);
        assert_eq!(profile.user.label(), "Ada");
        assert_eq!(profile.relationship.status, "accepted");
        assert_eq!(profile.relationship.describe(), "friends");
    }

    #[test]
    fn the_relationship_is_a_map_and_not_a_string() {
        let none = r#"{"user":{"id":9},"relationship":{"status":"none","blocked_by_me":false}}"#;
        let response: ProfileResponse = serde_json::from_str(none).unwrap();
        assert!(response.relationship.is_empty());
        assert_eq!(response.relationship.describe(), "");

        let incoming = r#"{"relationship":{"status":"pending","incoming":true,"outgoing":false}}"#;
        let response: ProfileResponse = serde_json::from_str(incoming).unwrap();
        assert!(response.relationship.incoming);
        assert_eq!(response.relationship.describe(), "wants to be your friend");

        let outgoing = r#"{"relationship":{"status":"pending","incoming":false,"outgoing":true}}"#;
        let response: ProfileResponse = serde_json::from_str(outgoing).unwrap();
        assert_eq!(response.relationship.describe(), "friend request sent");

        let blocked = r#"{"relationship":{"status":"blocked","blocked_by_me":true}}"#;
        let response: ProfileResponse = serde_json::from_str(blocked).unwrap();
        assert_eq!(response.relationship.describe(), "you blocked this person");

        let legacy = r#"{"relationship":"friends"}"#;
        let response: ProfileResponse = serde_json::from_str(legacy).unwrap();
        assert_eq!(
            response.relationship.status, "friends",
            "a plain string relationship still decodes"
        );

        let missing = r#"{"user":{"id":9}}"#;
        let response: ProfileResponse = serde_json::from_str(missing).unwrap();
        assert!(response.relationship.is_empty());
    }

    #[test]
    fn message_reads_reactions_from_the_server_field_name() {

        let raw = r#"{"id":1,"user_id":2,"display_name":"Ada","body":"hi",
                     "reactions":[{"emoji":"👍","count":2,"me":true}]}"#;
        let message: Message = serde_json::from_str(raw).unwrap();
        assert_eq!(message.reactions.len(), 1);
        assert!(message.reactions[0].me);
    }

    #[test]
    fn participant_roster_decodes() {

        let raw = r#"{"channel_id":5,"users":[
            {"user_id":9,"muted":true,"deafened":false,"screen":true,
             "screen_audio":false,"reconnecting":false,
             "profile":{"id":9,"username":"ada","display_name":"Ada"}},
            {"user_id":10,"muted":false,"deafened":false,"screen":false,
             "screen_audio":false,"reconnecting":false,"profile":{}}]}"#;
        let participants: Vec<Participant> =
            serde_json::from_value(serde_json::from_str::<serde_json::Value>(raw).unwrap()["users"].clone())
                .unwrap();
        assert_eq!(participants.len(), 2);
        assert!(participants[0].muted);
        assert!(participants[0].screen);
        assert_eq!(participants[0].profile.label(), "Ada");
    }

    #[test]
    fn rtc_config_decodes_camel_case() {
        let raw = r#"{"iceServers":[{"urls":["stun:stun.l.google.com:19302"],
                       "username":"u","credential":"p"}],
                     "iceTransportPolicy":"relay","turnStatus":"ready",
                     "refreshAfterSeconds":300}"#;
        let config: RtcConfig = serde_json::from_str(raw).unwrap();
        assert_eq!(config.ice_servers.len(), 1);
        assert_eq!(config.ice_servers[0].username.as_deref(), Some("u"));
        assert_eq!(config.ice_transport_policy, "relay");
    }
}
