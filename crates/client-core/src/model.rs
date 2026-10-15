use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Deserialize)]
pub struct Request {
    pub id: String,
    pub command: String,
    #[serde(default)]
    pub payload: Value,
}

#[derive(Serialize)]
pub struct Event {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    pub event: String,
    pub payload: Value,
}

impl Event {
    pub fn new(id: Option<&str>, event: &str, payload: Value) -> Self {
        Self {
            id: id.map(str::to_owned),
            event: event.into(),
            payload,
        }
    }
    pub fn error(id: Option<&str>, code: &str, message: &str, retryable: bool) -> Self {
        Self::new(
            id,
            "error",
            serde_json::json!({"code":code,"message":message,"retryable":retryable}),
        )
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Game {
    pub appid: u32,
    pub name: String,
    pub playtime_minutes: i32,
    pub last_played: u32,
    pub app_type: Option<String>,
    pub installdir: Option<String>,
    pub installed_dir: Option<String>,
    pub macos_supported: bool,
    #[serde(default)]
    pub windows_supported: bool,
    #[serde(default)]
    pub linux_supported: bool,
    #[serde(default)]
    pub update_available: bool,
    #[serde(default)]
    pub installed_build_id: Option<String>,
    #[serde(default)]
    pub available_build_id: Option<String>,
    #[serde(default)]
    pub installed_platform: Option<String>,
    pub header_url: String,
    pub launch: Vec<LaunchEntry>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LaunchEntry {
    pub executable: String,
    pub arguments: Option<String>,
    pub workingdir: Option<String>,
    pub launch_type: Option<String>,
    pub oslist: Option<String>,
    pub osarch: Option<String>,
    pub betakey: Option<String>,
}

impl From<steam_cm_protocol::ProtocolGame> for Game {
    fn from(game: steam_cm_protocol::ProtocolGame) -> Self {
        let public_default = |entry: &&steam_cm_protocol::LaunchEntry| {
            entry.betakey.as_deref().is_none_or(str::is_empty)
                && entry
                    .launch_type
                    .as_deref()
                    .is_none_or(|v| v.is_empty() || v == "default")
        };
        let macos_supported = game.launch.iter().filter(public_default).any(|entry| {
            entry.osarch.as_deref() != Some("32")
                && entry
                    .oslist
                    .as_deref()
                    .is_some_and(|oses| oses.split(',').any(|os| os.trim() == "macos"))
        });
        Self {
            linux_supported: game.launch.iter().filter(public_default).any(|entry| {
                entry
                    .oslist
                    .as_deref()
                    .is_some_and(|v| v.split(',').any(|os| os.trim() == "linux"))
            }),
            windows_supported: game.launch.iter().filter(public_default).any(|entry| {
                entry.oslist.as_deref().is_none_or(|oses| {
                    oses.is_empty() || oses.split(',').any(|os| os.trim() == "windows")
                })
            }),
            update_available: false,
            installed_build_id: None,
            available_build_id: None,
            installed_platform: None,
            appid: game.appid,
            name: game.name,
            playtime_minutes: game.playtime_forever,
            last_played: game.rtime_last_played,
            app_type: game.app_type,
            installdir: game.installdir,
            installed_dir: None,
            macos_supported,
            header_url: format!(
                "https://cdn.akamai.steamstatic.com/steam/apps/{}/header.jpg",
                game.appid
            ),
            launch: game
                .launch
                .into_iter()
                .map(|entry| LaunchEntry {
                    executable: entry.executable,
                    arguments: entry.arguments,
                    workingdir: entry.workingdir,
                    launch_type: entry.launch_type,
                    oslist: entry.oslist,
                    osarch: entry.osarch,
                    betakey: entry.betakey,
                })
                .collect(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn request_password_is_never_in_response_schema() {
        let req: Request = serde_json::from_str(
            r#"{"id":"1","command":"login_password","payload":{"password":"secret"}}"#,
        )
        .unwrap();
        let event = Event::error(Some(&req.id), "invalid_request", "Missing account", false);
        assert!(!serde_json::to_string(&event).unwrap().contains("secret"));
    }
    #[test]
    fn platform_detection_requires_mac_metadata() {
        let g = steam_cm_protocol::ProtocolGame {
            appid: 42,
            name: "A".into(),
            playtime_forever: 0,
            rtime_last_played: 0,
            img_icon_url: None,
            app_type: None,
            installdir: None,
            launch: vec![steam_cm_protocol::LaunchEntry {
                oslist: Some("windows, macos".into()),
                ..Default::default()
            }],
        };
        assert!(Game::from(g).macos_supported);
    }
}
