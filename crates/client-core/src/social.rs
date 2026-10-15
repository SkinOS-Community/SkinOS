//! Session-scoped friends and chat state. No messages or friend identities are persisted.
use crate::{
    daemon::SharedConnection,
    model::{Event, Request},
};
use anyhow::{Context, Result, bail};
use serde::Serialize;
use serde_json::json;
use std::{collections::BTreeMap, time::Duration};
use steam_cm_protocol::{
    ChatMessage, Friend, FriendsEvent, Persona, PersonaState, chat, emsg::EMsg, friends,
};

#[derive(Clone, Serialize)]
pub struct FriendRow {
    pub steamid: String,
    pub persona_name: String,
    pub avatar_url: String,
    pub state: String,
    pub in_game_appid: Option<u32>,
    pub in_game_name: Option<String>,
    pub relationship: &'static str,
}
impl FriendRow {
    fn empty(steamid: u64) -> Self {
        Self {
            steamid: steamid.to_string(),
            persona_name: steamid.to_string(),
            avatar_url: avatar_url(None),
            state: "offline".into(),
            in_game_appid: None,
            in_game_name: None,
            relationship: "friend",
        }
    }
    fn update(&mut self, persona: Persona) {
        if persona.name_present && !persona.name.is_empty() {
            self.persona_name = persona.name;
        }
        if persona.state_present {
            self.state = presence_name(&persona.state).into();
        }
        if persona.avatar_present {
            self.avatar_url = avatar_url(persona.avatar_hash.as_deref());
        }
        if persona.game_fields_present {
            self.in_game_appid = persona.game_app_id;
            self.in_game_name = persona.game_name;
        }
    }
}

pub struct SocialState {
    own_id: u64,
    account_name: String,
    own: FriendRow,
    presence: String,
    friends: BTreeMap<u64, FriendRow>,
    pending_personas: BTreeMap<u64, Persona>,
}
impl SocialState {
    pub fn new(own_id: u64, account_name: String) -> Self {
        let mut own = FriendRow::empty(own_id);
        own.persona_name = account_name.clone();
        own.state = "invisible".into();
        Self {
            own_id,
            account_name,
            own,
            presence: "invisible".into(),
            friends: BTreeMap::new(),
            pending_personas: BTreeMap::new(),
        }
    }
    pub fn friends_event(&self, id: Option<&str>) -> Event {
        Event::new(
            id,
            "friends",
            json!({"friends":self.friends.values().collect::<Vec<_>>()}),
        )
    }
    pub fn profile_event(&self, id: Option<&str>) -> Event {
        Event::new(
            id,
            "profile",
            json!({"steamid":self.own.steamid,"persona_name":self.own.persona_name,"avatar_url":self.own.avatar_url,"state":self.presence,"profile_url":format!("https://steamcommunity.com/profiles/{}",self.own_id),"account_name":self.account_name}),
        )
    }
    pub fn persona_ids(&self) -> Vec<u64> {
        std::iter::once(self.own_id)
            .chain(self.friends.keys().copied())
            .collect()
    }
    pub fn apply_presence(&mut self, state: &str) {
        self.presence = state.to_owned();
    }
    pub fn handle(&mut self, event: FriendsEvent) -> Vec<Event> {
        match event {
            FriendsEvent::FriendsList {
                friends,
                incremental,
            } => {
                self.apply_friends(friends, incremental);
                vec![self.friends_event(None)]
            }
            FriendsEvent::PersonaStates(personas) => {
                let mut events = vec![];
                for persona in personas {
                    if persona.steamid == self.own_id {
                        self.own.update(persona);
                        events.push(self.profile_event(None));
                    } else if let Some(row) = self.friends.get_mut(&persona.steamid) {
                        row.update(persona);
                        events.push(Event::new(None, "friend_update", json!({"friend":row})));
                    } else if self.pending_personas.len() < 4096 {
                        self.pending_personas.insert(persona.steamid, persona);
                    }
                }
                events
            }
            FriendsEvent::SocialError {
                operation,
                steamid,
                error,
            } => {
                let mut event = Event::error(None, operation, &error, true);
                event.payload["steamid"] = json!(steamid.to_string());
                vec![event]
            }
            FriendsEvent::IncomingMessage(message) => {
                vec![message_event(None, self.own_id, message, None)]
            }
            FriendsEvent::TypingNotification { steamid } => vec![Event::new(
                None,
                "chat_typing",
                json!({"steamid":steamid.to_string()}),
            )],
            _ => vec![],
        }
    }
    fn apply_friends(&mut self, entries: Vec<Friend>, incremental: bool) {
        if !incremental {
            self.friends.retain(|id, _| {
                entries
                    .iter()
                    .any(|f| f.steamid == *id && f.relationship == 3)
            });
        }
        for friend in entries {
            if friend.relationship != 3 {
                self.friends.remove(&friend.steamid);
                self.pending_personas.remove(&friend.steamid);
                continue;
            }
            let row = self
                .friends
                .entry(friend.steamid)
                .or_insert_with(|| FriendRow::empty(friend.steamid));
            if let Some(persona) = self.pending_personas.remove(&friend.steamid) {
                row.update(persona);
            }
        }
    }
    /// Validate before scheduling network work; never send to an arbitrary/unverified recipient.
    pub fn validate(&self, request: &Request) -> Result<()> {
        match request.command.as_str() {
            "set_presence" => {
                parse_presence(request.payload["state"].as_str().unwrap_or_default())?;
            }
            "get_chat_history" | "send_message" => {
                let partner = parse_steamid(request)?;
                if !self.friends.contains_key(&partner) {
                    bail!("Choose a current Steam friend before opening this chat");
                }
                if request.command == "send_message" {
                    let text = request.payload["text"]
                        .as_str()
                        .context("Missing message text")?;
                    if text.trim().is_empty() || text.len() > 16_000 {
                        bail!("Message must contain 1–16000 UTF-8 bytes");
                    }
                    if request.payload["client_message_id"]
                        .as_str()
                        .is_some_and(|id| id.len() > 128)
                    {
                        bail!("client_message_id is too long");
                    }
                }
            }
            _ => bail!("Unsupported social command"),
        }
        Ok(())
    }
    pub fn persona_name(&self, steamid: u64) -> Option<String> {
        self.friends
            .get(&steamid)
            .map(|row| row.persona_name.clone())
            .or_else(|| {
                self.pending_personas
                    .get(&steamid)
                    .filter(|persona| persona.name_present)
                    .map(|persona| persona.name.clone())
            })
    }
    pub fn own_id(&self) -> u64 {
        self.own_id
    }
}

pub async fn execute(request: Request, connection: SharedConnection, own_id: u64) -> Event {
    let code = match request.command.as_str() {
        "send_message" => "chat_send_failed",
        "get_chat_history" => "chat_history_failed",
        _ => "presence_failed",
    };
    let result=tokio::time::timeout(Duration::from_secs(15),async {
        let conn=connection.lock().await;
        let state=conn.state_snapshot().await;
        match request.command.as_str() {
            "set_presence"=>{
                let desired=request.payload["state"].as_str().context("Missing presence state")?;
                let (header,body)=friends::build_change_status(&state,parse_presence(desired)?);
                conn.send_message(EMsg::ClientChangeStatus,&header,&body).await?;
                Ok(Event::new(Some(&request.id),"presence",json!({"state":desired})))
            }
            "get_chat_history"=>{
                let partner=parse_steamid(&request)?;
                let messages=chat::get_recent_messages(&conn,&state,partner).await?;
                Ok(Event::new(Some(&request.id),"chat_history",json!({"steamid":partner.to_string(),"messages":messages.into_iter().map(|m|message_json(own_id,m,None)).collect::<Vec<_>>()})))
            }
            "send_message"=>{
                let partner=parse_steamid(&request)?;
                let text=request.payload["text"].as_str().context("Missing message text")?.to_owned();
                let message=chat::send_message(&conn,&state,partner,text).await?;
                Ok(message_event(Some(&request.id),own_id,message,request.payload["client_message_id"].as_str()))
            }
            _=>bail!("Unsupported social command"),
        }
    }).await;
    match result {
        Ok(Ok(event)) => event,
        other => {
            let message = match other {
                Ok(Err(error)) => format!("{error:#}"),
                _ => {
                    "Steam did not respond before the timeout; sending is not retried automatically"
                        .into()
                }
            };
            let mut event = Event::error(Some(&request.id), code, &message, true);
            event.payload["steamid"] = request.payload["steamid"].clone();
            event.payload["client_message_id"] = request.payload["client_message_id"].clone();
            event
        }
    }
}
fn parse_steamid(request: &Request) -> Result<u64> {
    request.payload["steamid"]
        .as_str()
        .context("Missing SteamID")?
        .parse::<u64>()
        .ok()
        .filter(|id| *id > 0)
        .context("Invalid SteamID")
}
pub fn presence_raw(value: &str) -> Option<u32> {
    parse_presence(value).ok().map(PersonaState::to_raw)
}
fn parse_presence(value: &str) -> Result<PersonaState> {
    Ok(match value {
        "invisible" => PersonaState::Invisible,
        "online" => PersonaState::Online,
        "away" => PersonaState::Away,
        "busy" => PersonaState::Busy,
        "offline" => PersonaState::Offline,
        _ => bail!("Unsupported presence state"),
    })
}
pub fn presence_name(state: &PersonaState) -> &'static str {
    match state {
        PersonaState::Invisible => "invisible",
        PersonaState::Online => "online",
        PersonaState::Busy => "busy",
        PersonaState::Away => "away",
        PersonaState::Snooze => "snooze",
        PersonaState::LookingToTrade => "looking_to_trade",
        PersonaState::LookingToPlay => "looking_to_play",
        PersonaState::Offline => "offline",
    }
}
fn avatar_url(hash: Option<&[u8]>) -> String {
    let hash = hash
        .filter(|h| h.len() == 20 && h.iter().any(|b| *b != 0))
        .map(|h| h.iter().map(|b| format!("{b:02x}")).collect::<String>())
        .unwrap_or_else(|| "fef49e7fa7e1997310d705b2a6158ff8dc1cdfeb".into());
    format!("https://avatars.steamstatic.com/{hash}_full.jpg")
}
fn message_json(own_id: u64, message: ChatMessage, client_id: Option<&str>) -> serde_json::Value {
    let sender = if message.from_local {
        own_id
    } else {
        message.steamid
    };
    json!({"id":format!("{}:{}:{}:{}",message.steamid,sender,message.timestamp,message.ordinal),"sender_steamid":sender.to_string(),"text":message.message,"timestamp":message.timestamp,"outgoing":message.from_local,"client_message_id":client_id,"state":if message.from_local {"sent"} else {"received"}})
}
fn message_event(
    id: Option<&str>,
    own_id: u64,
    message: ChatMessage,
    client_id: Option<&str>,
) -> Event {
    Event::new(
        id,
        "chat_message",
        json!({"steamid":message.steamid.to_string(),"message":message_json(own_id,message,client_id)}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    fn persona(id: u64) -> Persona {
        Persona {
            steamid: id,
            name: "Friend".into(),
            name_present: true,
            state: PersonaState::Online,
            state_present: true,
            avatar_hash: None,
            avatar_present: false,
            game_app_id: Some(42),
            game_name: Some("Game".into()),
            game_fields_present: true,
        }
    }
    #[test]
    fn incremental_removal_preserves_other_friends_and_partial_presence() {
        let mut state = SocialState::new(1, "owner".into());
        state.handle(FriendsEvent::PersonaStates(vec![persona(2)]));
        state.apply_friends(
            vec![
                Friend {
                    steamid: 2,
                    relationship: 3,
                },
                Friend {
                    steamid: 3,
                    relationship: 3,
                },
            ],
            false,
        );
        let mut partial = persona(2);
        partial.name_present = false;
        partial.state_present = false;
        partial.game_fields_present = false;
        partial.state = PersonaState::Offline;
        partial.name.clear();
        state.handle(FriendsEvent::PersonaStates(vec![partial]));
        assert_eq!(state.friends[&2].state, "online");
        assert_eq!(state.friends[&2].persona_name, "Friend");
        assert_eq!(state.friends[&2].in_game_appid, Some(42));
        state.apply_friends(
            vec![Friend {
                steamid: 3,
                relationship: 0,
            }],
            true,
        );
        assert_eq!(state.friends.len(), 1);
        assert!(state.friends.contains_key(&2));
        assert_eq!(state.profile_event(None).payload["state"], "invisible");
    }
    #[test]
    fn cross_session_echo_and_send_confirmation_share_message_id() {
        let message = ChatMessage {
            steamid: 2,
            message: "hello".into(),
            timestamp: 123,
            ordinal: 4,
            from_local: true,
        };
        let a = message_json(1, message.clone(), Some("request"));
        let b = message_json(1, message, None);
        assert_eq!(a["id"], b["id"]);
        assert_eq!(a["sender_steamid"], "1");
        assert_eq!(a["state"], "sent");
    }
    #[test]
    fn refuses_unknown_recipient_and_empty_or_large_messages() {
        let mut state = SocialState::new(1, "owner".into());
        let mut request = Request {
            id: "a".into(),
            command: "send_message".into(),
            payload: json!({"steamid":"2","text":"hello"}),
        };
        assert!(state.validate(&request).is_err());
        state.apply_friends(
            vec![Friend {
                steamid: 2,
                relationship: 3,
            }],
            false,
        );
        assert!(state.validate(&request).is_ok());
        request.payload["text"] = json!(" ");
        assert!(state.validate(&request).is_err());
        request.payload["text"] = json!("a".repeat(16001));
        assert!(state.validate(&request).is_err());
        let new_session = SocialState::new(4, "other".into());
        assert!(new_session.friends.is_empty());
    }
}
