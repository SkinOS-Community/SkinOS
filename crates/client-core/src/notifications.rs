//! Real account notifications; only deduplication IDs are persisted, never bodies.
use crate::{model::Event, session, social::SocialState};
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, HashSet, VecDeque},
    path::PathBuf,
    time::{SystemTime, UNIX_EPOCH},
};
use steam_cm_protocol::{
    FriendsEvent, notifications::NotificationBatch, protobuf::SteamNotificationData,
};
const MAX_INBOX: usize = 200;
const MAX_SEEN: usize = 4096;
const NOTIFICATION_URL: &str = "https://steamcommunity.com/my/notifications/";
/// Background notification trouble is feature-scoped, never a core/game error.
/// Only an explicit notification request receives a correlated error response.
pub fn failure(
    steamid: u64,
    id: Option<&str>,
    code: &str,
    message: &str,
    retryable: bool,
) -> Event {
    match id {
        Some(id) => Event::error(Some(id), code, message, retryable),
        None => Event::new(
            None,
            "steam_notifications_status",
            json!({"steamid":steamid.to_string(),"state":if code=="notifications_storage_failed" {"degraded"} else {"unavailable"},"code":code,"message":message,"retryable":retryable}),
        ),
    }
}
pub fn ready(steamid: u64) -> Event {
    Event::new(
        None,
        "steam_notifications_status",
        json!({"steamid":steamid.to_string(),"state":"ready","code":null,"message":null,"retryable":false}),
    )
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct Notice {
    pub id: String,
    pub r#type: i32,
    pub category: String,
    pub title: String,
    pub body: String,
    pub url: Option<String>,
    pub appid: Option<u32>,
    pub friend_steamid: Option<String>,
    pub timestamp: u64,
    pub read: bool,
}
#[derive(Default, Serialize, Deserialize)]
struct SavedSeen {
    steamid: String,
    seen_ids: VecDeque<String>,
    #[serde(default)]
    friend_requests: BTreeMap<u64, String>,
    #[serde(default)]
    sequence: u64,
}
pub struct NotificationState {
    steamid: u64,
    path: Option<PathBuf>,
    seen: SavedSeen,
    rows: BTreeMap<String, Notice>,
    session_start: u64,
    baseline: bool,
    friend_baseline: bool,
    pending_gifts: u32,
    pending_friends: u32,
    pending_family: u32,
}
impl NotificationState {
    pub fn load(steamid: u64) -> Result<Self> {
        Self::load_path(
            steamid,
            session::data_dir()?
                .join("notifications")
                .join(format!("{steamid}.json")),
        )
    }
    fn load_path(steamid: u64, path: PathBuf) -> Result<Self> {
        ensure!(
            steamid.to_string().len() == 17,
            "Invalid notification account"
        );
        ensure!(
            !path.is_symlink(),
            "Notification metadata cannot be a symlink"
        );
        let mut state = Self::memory(steamid);
        state.path = Some(path.clone());
        if path.exists() {
            ensure!(
                std::fs::metadata(&path)?.len() <= 1_048_576,
                "Notification metadata exceeds limit"
            );
            let saved: SavedSeen = serde_json::from_slice(&std::fs::read(path)?)?;
            ensure!(
                saved.steamid == steamid.to_string(),
                "Notification metadata belongs to another account"
            );
            ensure!(
                saved.seen_ids.len() <= MAX_SEEN && saved.friend_requests.len() <= MAX_SEEN,
                "Notification metadata exceeds limit"
            );
            ensure!(
                saved
                    .seen_ids
                    .iter()
                    .chain(saved.friend_requests.values())
                    .all(|id| id.len() <= 192
                        && id
                            .bytes()
                            .all(|c| c.is_ascii_alphanumeric() || b":_-".contains(&c))),
                "Invalid notification identifier"
            );
            state.seen = saved;
        }
        Ok(state)
    }
    pub fn memory(steamid: u64) -> Self {
        Self {
            steamid,
            path: None,
            seen: SavedSeen {
                steamid: steamid.to_string(),
                ..Default::default()
            },
            rows: BTreeMap::new(),
            session_start: now(),
            baseline: false,
            friend_baseline: false,
            pending_gifts: 0,
            pending_friends: 0,
            pending_family: 0,
        }
    }
    pub fn snapshot(&self, id: Option<&str>, initial: bool) -> Event {
        let mut rows = self.rows.values().cloned().collect::<Vec<_>>();
        rows.sort_by_key(|row| std::cmp::Reverse(row.timestamp));
        rows.truncate(MAX_INBOX);
        Event::new(
            id,
            "steam_notifications",
            json!({"steamid":self.steamid.to_string(),"unread_count":rows.iter().filter(|row|!row.read).count(),"notifications":rows,"initial":initial,"pending_gift_count":self.pending_gifts,"pending_friend_count":self.pending_friends,"pending_family_invite_count":self.pending_family}),
        )
    }
    fn remember(&mut self, id: &str) -> bool {
        if self.seen.seen_ids.iter().any(|old| old == id) {
            return false;
        }
        self.seen.seen_ids.push_back(id.into());
        while self.seen.seen_ids.len() > MAX_SEEN {
            self.seen.seen_ids.pop_front();
        }
        true
    }
    fn insert(&mut self, row: Notice, allow_notify: bool, initial: bool, events: &mut Vec<Event>) {
        let fresh = self.remember(&row.id);
        let changed = self.rows.get(&row.id) != Some(&row);
        let notify = allow_notify && fresh && !row.read;
        if !initial && (fresh || changed) {
            events.push(Event::new(None,"steam_notification",json!({"steamid":self.steamid.to_string(),"notification":row,"is_new":fresh,"notify":notify})));
        }
        self.rows.insert(row.id.clone(), row);
    }
    fn finish(&mut self, events: &mut Vec<Event>, id: Option<&str>, initial: bool) {
        if self.rows.len() > MAX_INBOX {
            let mut order = self
                .rows
                .values()
                .map(|row| (row.timestamp, row.id.clone()))
                .collect::<Vec<_>>();
            order.sort();
            for (_, id) in order.into_iter().take(self.rows.len() - MAX_INBOX) {
                self.rows.remove(&id);
            }
        }
        if let Err(error) = self.persist() {
            events.push(failure(
                self.steamid,
                None,
                "notifications_storage_failed",
                &error.to_string(),
                true,
            ));
        }
        // Incremental notices deliberately precede the snapshot, so the UI
        // does not seed its dedup state before delivering a genuine new notice.
        events.push(self.snapshot(id, initial));
    }
    fn persist(&self) -> Result<()> {
        use std::{
            io::Write,
            os::unix::fs::{OpenOptionsExt, PermissionsExt},
        };
        let Some(path) = &self.path else {
            return Ok(());
        };
        let parent = path.parent().context("Notification directory missing")?;
        ensure!(
            !parent.is_symlink() && !path.is_symlink(),
            "Notification metadata cannot be a symlink"
        );
        std::fs::create_dir_all(parent)?;
        std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700))?;
        let temp = path.with_extension(format!(
            "{}-{}.tmp",
            std::process::id(),
            SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos()
        ));
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&temp)?;
        let result = (|| {
            file.write_all(&serde_json::to_vec(&self.seen)?)?;
            file.sync_all()?;
            std::fs::rename(&temp, path)?;
            Ok(())
        })();
        if result.is_err() {
            let _ = std::fs::remove_file(temp);
        }
        result
    }
    pub fn ingest(
        &mut self,
        batch: NotificationBatch,
        replace: bool,
        id: Option<&str>,
    ) -> Vec<Event> {
        if batch
            .owner_steamid
            .is_some_and(|owner| owner != self.steamid)
        {
            return vec![failure(
                self.steamid,
                id,
                "notifications_account_mismatch",
                "Notification account mismatch",
                false,
            )];
        }
        let initial = !self.baseline;
        let mut events = vec![];
        if replace {
            self.rows.retain(|id, _| !id.starts_with("steam:"));
        }
        if let Some(value) = batch.pending_gift_count {
            self.pending_gifts = value;
        }
        if let Some(value) = batch.pending_friend_count {
            self.pending_friends = value;
        }
        if let Some(value) = batch.pending_family_invite_count {
            self.pending_family = value;
        }
        for raw in batch.notifications.into_iter().take(MAX_INBOX) {
            let key = raw.notification_id.map(|id| format!("steam:{id}"));
            if raw.hidden.unwrap_or(false)
                || raw
                    .expiry
                    .is_some_and(|expiry| expiry > 0 && u64::from(expiry) <= now())
            {
                if let Some(key) = key {
                    self.rows.remove(&key);
                }
                continue;
            }
            if let Some(row) = normalize_steam(raw) {
                // FriendMessages and ClientFriendsList own these desktop paths,
                // avoiding a second toast for the same event from another API.
                let desktop = !initial
                    && !matches!(row.r#type, 5 | 13)
                    && row.timestamp >= self.session_start;
                self.insert(row, desktop, initial, &mut events);
            }
        }
        self.baseline = true;
        self.finish(&mut events, id, initial);
        events
    }
    pub fn social(&mut self, event: &FriendsEvent, social: &SocialState) -> Vec<Event> {
        let mut events = vec![];
        match event {
            FriendsEvent::SteamNotifications(batch) => {
                return self.ingest(batch.clone(), false, None);
            }
            FriendsEvent::IncomingMessage(message)
                if !message.from_local && message.timestamp > 0 =>
            {
                // Same canonical ID as chat_message, no separate history fetch.
                let id = format!(
                    "{}:{}:{}:{}",
                    message.steamid, message.steamid, message.timestamp, message.ordinal
                );
                let title = social
                    .persona_name(message.steamid)
                    .unwrap_or_else(|| "Новое сообщение Steam".into());
                let row = Notice {
                    id,
                    r#type: 13,
                    category: "direct_message".into(),
                    title: plain(&title, 100),
                    body: plain(&message.message, 512),
                    url: None,
                    appid: None,
                    friend_steamid: Some(message.steamid.to_string()),
                    timestamp: u64::from(message.timestamp),
                    read: false,
                };
                let desktop = row.timestamp >= self.session_start;
                self.insert(row, desktop, false, &mut events);
                self.finish(&mut events, None, false);
            }
            FriendsEvent::FriendsList {
                friends,
                incremental,
            } => {
                let initial = !self.friend_baseline;
                if !incremental {
                    let pending = friends
                        .iter()
                        .filter(|friend| friend.relationship == 2)
                        .map(|friend| friend.steamid)
                        .collect::<HashSet<_>>();
                    let removed = self
                        .seen
                        .friend_requests
                        .iter()
                        .filter(|(id, _)| !pending.contains(id))
                        .map(|(id, key)| (*id, key.clone()))
                        .collect::<Vec<_>>();
                    for (id, key) in removed {
                        self.seen.friend_requests.remove(&id);
                        self.rows.remove(&key);
                    }
                }
                for friend in friends.iter().take(MAX_SEEN) {
                    if friend.steamid.to_string().len() != 17 {
                        continue;
                    }
                    if friend.relationship != 2 {
                        if let Some(key) = self.seen.friend_requests.remove(&friend.steamid) {
                            self.rows.remove(&key);
                        }
                        continue;
                    }
                    let key = if let Some(key) = self.seen.friend_requests.get(&friend.steamid) {
                        key.clone()
                    } else {
                        if self.seen.friend_requests.len() >= MAX_SEEN {
                            continue;
                        }
                        self.seen.sequence = self.seen.sequence.saturating_add(1);
                        let key = format!("friend:{}:{}", friend.steamid, self.seen.sequence);
                        self.seen
                            .friend_requests
                            .insert(friend.steamid, key.clone());
                        key
                    };
                    let name = social.persona_name(friend.steamid);
                    let body = name
                        .map(|name| format!("{} хочет добавить вас в друзья", plain(&name, 100)))
                        .unwrap_or_else(|| "Получена заявка в друзья в Steam".into());
                    let row = Notice {
                        id: key,
                        r#type: 5,
                        category: "friend_request".into(),
                        title: "Заявка в друзья".into(),
                        body,
                        url: Some("https://steamcommunity.com/my/friends/pending".into()),
                        appid: None,
                        friend_steamid: Some(friend.steamid.to_string()),
                        timestamp: now(),
                        read: false,
                    };
                    self.insert(row, *incremental && !initial, initial, &mut events);
                }
                self.pending_friends = self.seen.friend_requests.len() as u32;
                self.friend_baseline = true;
                self.finish(&mut events, None, initial);
            }
            FriendsEvent::PersonaStates(personas) => {
                for persona in personas
                    .iter()
                    .filter(|persona| persona.name_present && !persona.name.is_empty())
                {
                    if let Some(id) = self.seen.friend_requests.get(&persona.steamid)
                        && let Some(old) = self.rows.get(id)
                    {
                        let mut row = old.clone();
                        row.body =
                            format!("{} хочет добавить вас в друзья", plain(&persona.name, 100));
                        self.insert(row, false, false, &mut events);
                    }
                }
                if !events.is_empty() {
                    self.finish(&mut events, None, false);
                }
            }
            _ => {}
        }
        events
    }
}
fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}
fn number(value: Option<&Value>) -> Option<u64> {
    let value = value?;
    value
        .as_u64()
        .or_else(|| value.as_str()?.parse().ok())
        .filter(|v| *v > 0)
}
fn plain(value: &str, limit: usize) -> String {
    static TAGS: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    let regex = TAGS.get_or_init(|| regex::Regex::new(r"<[^>]{0,4096}>").unwrap());
    let bounded = value.chars().take(16_384).collect::<String>();
    let decoded = html_escape::decode_html_entities(&bounded);
    let without = regex.replace_all(&decoded, " ");
    without
        .chars()
        .filter_map(|c| {
            if c == '\n' || c == '\r' || c == '\t' {
                Some(' ')
            } else if c.is_control()
                || matches!(c,'\u{202a}'..='\u{202e}'|'\u{2066}'..='\u{2069}'|'\u{200e}'|'\u{200f}')
            {
                None
            } else {
                Some(c)
            }
        })
        .take(limit)
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}
fn safe_url(value: &str) -> Option<String> {
    if value.len() > 2048 {
        return None;
    }
    let mut url = reqwest::Url::parse(value).ok()?;
    if url.scheme() != "https"
        || !matches!(
            url.host_str(),
            Some("steamcommunity.com" | "store.steampowered.com" | "help.steampowered.com")
        )
        || !url.username().is_empty()
        || url.password().is_some()
        || url.port().is_some()
    {
        return None;
    }
    // Notification destinations never carry credential/query values into UI or
    // diagnostics. Steam receives the owned webview's existing session cookies.
    url.set_query(None);
    url.set_fragment(None);
    Some(url.into())
}
fn normalize_steam(raw: SteamNotificationData) -> Option<Notice> {
    let id = raw.notification_id.filter(|id| *id > 0)?;
    let kind = raw.notification_type.filter(|kind| *kind > 0)?;
    let data = raw
        .body_data
        .filter(|body| body.len() <= 16 * 1024)
        .and_then(|body| serde_json::from_str::<Value>(&body).ok())
        .filter(Value::is_object)
        .unwrap_or_default();
    let (category, title) = match kind {
        2 => ("gift", "Подарок в Steam"),
        3 => ("comment", "Новый комментарий"),
        4 => ("item", "Новые предметы"),
        5 => ("friend_request", "Заявка в друзья"),
        8 => ("wishlist", "Список желаемого Steam"),
        9 => ("trade_offer", "Предложение обмена"),
        16 => ("family_invite", "Приглашение в семью Steam"),
        _ => ("general", "Уведомление Steam"),
    };
    // Authentication/confirmation bodies may include sensitive context. Keep
    // these generic and leave the actual confirmation in Steam's own page.
    let sensitive = matches!(kind, 25 | 26);
    let title = if sensitive {
        title.to_owned()
    } else {
        data["title"]
            .as_str()
            .map(|v| plain(v, 100))
            .filter(|v| !v.is_empty())
            .unwrap_or_else(|| title.into())
    };
    let body = if sensitive {
        None
    } else {
        ["body", "message", "text", "comment", "description"]
            .iter()
            .find_map(|key| data[*key].as_str())
            .map(|value| plain(value, 512))
            .filter(|value| !value.is_empty())
    }
    .unwrap_or_else(|| "Откройте уведомление в Steam для подробностей".into());
    let appid = number(data.get("appid")).and_then(|value| u32::try_from(value).ok());
    let friend_steamid = [
        "steamid",
        "steamid_sender",
        "sender_steamid",
        "steamid_friend",
        "friendid",
    ]
    .iter()
    .find_map(|key| number(data.get(*key)))
    .map(|id| id.to_string())
    .filter(|id| id.len() == 17);
    let url = if sensitive {
        None
    } else {
        ["url", "link"]
            .iter()
            .find_map(|key| data[*key].as_str().and_then(safe_url))
    }
    .unwrap_or_else(|| {
        if kind == 9 {
            number(data.get("tradeoffer_id"))
                .map(|id| format!("https://steamcommunity.com/tradeoffer/{id}"))
                .unwrap_or_else(|| NOTIFICATION_URL.into())
        } else {
            NOTIFICATION_URL.into()
        }
    });
    Some(Notice {
        id: format!("steam:{id}"),
        r#type: kind,
        category: category.into(),
        title,
        body,
        url: Some(url),
        appid,
        friend_steamid,
        timestamp: u64::from(raw.timestamp.unwrap_or(0)),
        read: raw.read.unwrap_or(false),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use steam_cm_protocol::{ChatMessage, Friend};
    const SELF: u64 = 76561198000000001;
    const FRIEND: u64 = 76561198000000002;
    fn raw(id: u64, kind: i32, timestamp: u32, body: &str) -> SteamNotificationData {
        SteamNotificationData {
            notification_id: Some(id),
            notification_type: Some(kind),
            timestamp: Some(timestamp),
            body_data: Some(body.into()),
            read: Some(false),
            ..Default::default()
        }
    }
    fn batch(rows: Vec<SteamNotificationData>) -> NotificationBatch {
        NotificationBatch {
            owner_steamid: Some(SELF),
            notifications: rows,
            pending_gift_count: Some(1),
            pending_friend_count: Some(2),
            pending_family_invite_count: Some(0),
        }
    }
    fn desktop(events: &[Event]) -> usize {
        events
            .iter()
            .filter(|event| event.event == "steam_notification" && event.payload["notify"] == true)
            .count()
    }
    #[test]
    fn initial_snapshot_is_silent_new_push_precedes_snapshot_and_dedup_survives_restart() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("seen.json");
        let mut state = NotificationState::load_path(SELF, path.clone()).unwrap();
        state.session_start = 1000;
        let initial = state.ingest(
            batch(vec![raw(1, 9, 1001, r#"{"body":"private test text"}"#)]),
            true,
            None,
        );
        assert_eq!(desktop(&initial), 0);
        assert_eq!(initial.last().unwrap().payload["initial"], true);
        let fresh = state.ingest(
            batch(vec![raw(2, 3, 1002, r#"{"body":"new comment"}"#)]),
            false,
            None,
        );
        assert_eq!(desktop(&fresh), 1);
        assert_eq!(fresh[0].event, "steam_notification");
        assert_eq!(fresh.last().unwrap().event, "steam_notifications");
        assert_eq!(
            desktop(&state.ingest(
                batch(vec![raw(2, 3, 1002, r#"{"body":"new comment"}"#)]),
                false,
                None
            )),
            0
        );
        let saved = std::fs::read_to_string(&path).unwrap();
        assert!(!saved.contains("private test text"));
        assert!(!saved.contains("new comment"));
        assert!(!saved.contains("https://"));
        let mut restored = NotificationState::load_path(SELF, path.clone()).unwrap();
        restored.session_start = 1000;
        assert_eq!(
            desktop(&restored.ingest(batch(vec![raw(2, 3, 1002, "{}")]), true, None)),
            0
        );
        assert_eq!(
            desktop(&restored.ingest(batch(vec![raw(2, 3, 1003, "{}")]), false, None)),
            0
        );
        assert!(NotificationState::load_path(FRIEND, path).is_err());
    }
    #[test]
    fn wrong_account_hidden_expired_and_already_read_do_not_notify() {
        let mut state = NotificationState::memory(SELF);
        state.session_start = 1000;
        state.ingest(batch(vec![]), true, None);
        let mut wrong = batch(vec![raw(1, 9, 1001, "{}")]);
        wrong.owner_steamid = Some(FRIEND);
        let rejected = state.ingest(wrong, false, None);
        assert_eq!(rejected[0].event, "steam_notifications_status");
        assert_eq!(
            rejected[0].payload["code"],
            "notifications_account_mismatch"
        );
        assert!(state.rows.is_empty());
        let mut hidden = raw(2, 3, 1001, "{}");
        hidden.hidden = Some(true);
        let mut expired = raw(3, 3, 1001, "{}");
        expired.expiry = Some(1);
        let mut read = raw(4, 3, 1001, "{}");
        read.read = Some(true);
        let events = state.ingest(batch(vec![hidden, expired, read]), false, None);
        assert_eq!(desktop(&events), 0);
        assert_eq!(state.rows.len(), 1);
        assert_eq!(events.last().unwrap().payload["unread_count"], 0);
    }
    #[test]
    fn incoming_dm_uses_chat_id_and_history_echo_or_replay_never_notifies() {
        let mut state = NotificationState::memory(SELF);
        state.session_start = 1000;
        let mut social = SocialState::new(SELF, "owner".into());
        let message = ChatMessage {
            steamid: FRIEND,
            message: "hello".into(),
            timestamp: 1001,
            ordinal: 3,
            from_local: false,
        };
        let notice = state.social(&FriendsEvent::IncomingMessage(message.clone()), &social);
        assert_eq!(desktop(&notice), 1);
        assert_eq!(
            notice[0].payload["notification"]["category"],
            "direct_message"
        );
        let chat = social.handle(FriendsEvent::IncomingMessage(message.clone()));
        assert_eq!(
            notice[0].payload["notification"]["id"],
            chat[0].payload["message"]["id"]
        );
        assert_eq!(
            desktop(&state.social(&FriendsEvent::IncomingMessage(message.clone()), &social)),
            0
        );
        let mut echo = message.clone();
        echo.from_local = true;
        assert!(
            state
                .social(&FriendsEvent::IncomingMessage(echo), &social)
                .is_empty()
        );
        let mut history = message;
        history.timestamp = 50;
        assert_eq!(
            desktop(&state.social(&FriendsEvent::IncomingMessage(history), &social)),
            0
        );
    }
    #[test]
    fn friend_request_baseline_is_silent_and_new_relationship_is_account_deduped() {
        let mut state = NotificationState::memory(SELF);
        let social = SocialState::new(SELF, "owner".into());
        let full = FriendsEvent::FriendsList {
            friends: vec![Friend {
                steamid: FRIEND,
                relationship: 2,
            }],
            incremental: false,
        };
        assert_eq!(desktop(&state.social(&full, &social)), 0);
        let repeated = FriendsEvent::FriendsList {
            friends: vec![Friend {
                steamid: FRIEND,
                relationship: 2,
            }],
            incremental: true,
        };
        assert_eq!(desktop(&state.social(&repeated, &social)), 0);
        state.social(
            &FriendsEvent::FriendsList {
                friends: vec![Friend {
                    steamid: FRIEND,
                    relationship: 0,
                }],
                incremental: true,
            },
            &social,
        );
        assert_eq!(
            desktop(&state.social(&repeated, &social)),
            1,
            "a genuinely new invitation after removal needs a new identity"
        );
        assert_eq!(desktop(&state.social(&repeated, &social)), 0);
    }
    #[test]
    fn payload_is_plain_bounded_and_destinations_cannot_escape_steam() {
        let value = raw(
            1,
            777,
            1,
            r#"{"title":"<b>Hi</b>\u202E","body":"&lt;script&gt;no execution&lt;/script&gt;\ntext","url":"https://steamcommunity.com@evil.example/path?token=secret","refresh_token":"not displayed"}"#,
        );
        let notice = normalize_steam(value).unwrap();
        assert_eq!(notice.category, "general");
        assert_eq!(notice.title, "Hi");
        assert!(!notice.body.contains('<'));
        assert_eq!(notice.url.as_deref(), Some(NOTIFICATION_URL));
        let serial = serde_json::to_string(&notice).unwrap();
        assert!(!serial.contains("not displayed"));
        assert!(!serial.contains("secret"));
        assert_eq!(
            safe_url("https://steamcommunity.com/tradeoffer/123?token=secret#secret").as_deref(),
            Some("https://steamcommunity.com/tradeoffer/123")
        );
        for url in [
            "steam://run/42",
            "file:///etc/passwd",
            "https://steamcommunity.com.evil.test/x",
            "https://steamcommunity.com:444/x",
        ] {
            assert!(safe_url(url).is_none());
        }
        let sensitive=normalize_steam(raw(2,25,1,r#"{"title":"secret device","body":"private confirmation","url":"https://steamcommunity.com/private"}"#)).unwrap();
        assert!(
            !serde_json::to_string(&sensitive)
                .unwrap()
                .contains("private")
        );
        let mut state = NotificationState::memory(SELF);
        let events = state.ingest(
            batch((1..500).map(|id| raw(id, 10, 1, "{}")).collect()),
            true,
            None,
        );
        assert!(state.rows.len() <= MAX_INBOX);
        assert!(
            events.last().unwrap().payload["notifications"]
                .as_array()
                .unwrap()
                .len()
                <= MAX_INBOX
        );
    }
    #[test]
    fn background_notification_failures_are_scoped_and_keep_the_loaded_inbox() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("seen.json");
        let mut state = NotificationState::load_path(SELF, path.clone()).unwrap();
        state.session_start = 1000;
        state.ingest(batch(vec![raw(1, 9, 1001, "{}")]), true, None);
        std::fs::remove_file(&path).unwrap();
        std::fs::create_dir(&path).unwrap();
        let events = state.ingest(batch(vec![raw(2, 3, 1002, "{}")]), false, None);
        assert!(
            !events
                .iter()
                .any(|event| event.event == "error" && event.id.is_none()),
            "notification storage trouble became a global game error"
        );
        assert!(
            events
                .iter()
                .any(|event| event.event == "steam_notifications_status"
                    && event.payload["state"] == "degraded")
        );
        assert_eq!(
            state.rows.len(),
            2,
            "notification trouble must not clear the inbox"
        );
        let background = failure(
            SELF,
            None,
            "notifications_failed",
            "request timed out",
            true,
        );
        assert_eq!(background.event, "steam_notifications_status");
        assert_eq!(background.payload["steamid"], SELF.to_string());
        assert_eq!(background.payload["state"], "unavailable");
        let explicit = failure(
            SELF,
            Some("manual-refresh"),
            "notifications_failed",
            "request timed out",
            true,
        );
        assert_eq!(explicit.event, "error");
        assert_eq!(explicit.id.as_deref(), Some("manual-refresh"));
        assert_eq!(ready(SELF).payload["state"], "ready");
    }
}
