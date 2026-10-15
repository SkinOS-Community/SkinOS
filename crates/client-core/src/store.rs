//! Public store metadata. HTML from Steam is reduced to bounded plain text;
//! neither the Rust core nor native game cards execute publisher-supplied HTML.
use anyhow::{Context, Result, bail, ensure};
use reqwest::Client;
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    sync::{Mutex, OnceLock},
    time::{Duration, Instant},
};

const MAX_RESPONSE_BYTES: usize = 4 * 1024 * 1024;
const CACHE_TTL: Duration = Duration::from_secs(3600);
const MAX_CACHE_ENTRIES: usize = 128;
type DetailsCache = Mutex<HashMap<u32, (Instant, Value)>>;
static CACHE: OnceLock<DetailsCache> = OnceLock::new();

pub(crate) fn http_client() -> Result<Client> {
    http_client_builder()
        .build()
        .context("Cannot initialize the Steam web connection")
}

pub(crate) fn http_client_builder() -> reqwest::ClientBuilder {
    Client::builder()
        .https_only(true)
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(25))
        .user_agent("SkinOS/0.1 (macOS; Steam store browser)")
}

pub(crate) async fn bounded_body(mut response: reqwest::Response, limit: usize) -> Result<Vec<u8>> {
    ensure!(
        response.status().is_success(),
        "Steam web request failed (HTTP {})",
        response.status().as_u16()
    );
    ensure!(
        response
            .content_length()
            .is_none_or(|size| size <= limit as u64),
        "Steam web response exceeds the size limit"
    );
    let mut bytes = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .context("Cannot read the Steam web response")?
    {
        ensure!(
            bytes.len().saturating_add(chunk.len()) <= limit,
            "Steam web response exceeds the size limit"
        );
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}

pub async fn load_game_details(appid: u32) -> Result<Value> {
    ensure!(appid != 0, "Invalid AppID");
    let cache = CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    if let Some((created, details)) = cache
        .lock()
        .map_err(|_| anyhow::anyhow!("Store cache unavailable"))?
        .get(&appid)
        && created.elapsed() < CACHE_TTL
    {
        return Ok(details.clone());
    }
    let response = http_client()?
        .get("https://store.steampowered.com/api/appdetails")
        .query(&[("appids", appid.to_string()), ("l", "russian".into())])
        .send()
        .await
        .context("Cannot reach the Steam store")?;
    let bytes = bounded_body(response, MAX_RESPONSE_BYTES).await?;
    let source: Value = serde_json::from_slice(&bytes).context("Invalid Steam store response")?;
    let details = parse_details(appid, &source)?;
    let mut cache = cache
        .lock()
        .map_err(|_| anyhow::anyhow!("Store cache unavailable"))?;
    cache.retain(|_, (created, _)| created.elapsed() < CACHE_TTL);
    if cache.len() >= MAX_CACHE_ENTRIES
        && let Some(oldest) = cache
            .iter()
            .min_by_key(|(_, (created, _))| *created)
            .map(|(id, _)| *id)
    {
        cache.remove(&oldest);
    }
    cache.insert(appid, (Instant::now(), details.clone()));
    Ok(details)
}

fn parse_details(appid: u32, response: &Value) -> Result<Value> {
    let result = &response[appid.to_string()];
    ensure!(
        result["success"] == true,
        "The Steam store has no public details for this game"
    );
    let data = &result["data"];
    ensure!(data.is_object(), "Invalid Steam store game data");
    if data["steam_appid"]
        .as_u64()
        .is_some_and(|id| id != u64::from(appid))
    {
        bail!("Steam store returned a different AppID");
    }
    let name = data["name"]
        .as_str()
        .context("Steam store game has no name")?;
    let requirements = |key: &str| {
        // Steam returns [] instead of {} when a platform has no requirements.
        json!({
            "minimum": plain_text(data[key]["minimum"].as_str().unwrap_or(""), 24_000),
            "recommended": plain_text(data[key]["recommended"].as_str().unwrap_or(""), 24_000)
        })
    };
    Ok(json!({
        "appid": appid,
        "name": plain_text(name, 500),
        "description": plain_text(data["short_description"].as_str().unwrap_or(""), 8000),
        "platforms": {
            "windows": data["platforms"]["windows"].as_bool().unwrap_or(false),
            "mac": data["platforms"]["mac"].as_bool().unwrap_or(false),
            "linux": data["platforms"]["linux"].as_bool().unwrap_or(false)
        },
        "requirements": {
            "windows": requirements("pc_requirements"),
            "mac": requirements("mac_requirements"),
            "linux": requirements("linux_requirements")
        },
        "store_url": format!("https://store.steampowered.com/app/{appid}/"),
        "header_url": safe_header_url(data["header_image"].as_str().unwrap_or("")),
    }))
}

fn safe_header_url(value: &str) -> String {
    reqwest::Url::parse(value)
        .ok()
        .filter(|url| {
            url.scheme() == "https"
                && url.username().is_empty()
                && url.password().is_none()
                && url.port().is_none()
                && url.host_str().is_some_and(|host| {
                    host.ends_with(".steamstatic.com") || host.ends_with(".steamusercontent.com")
                })
        })
        .map(|url| url.to_string())
        .unwrap_or_default()
}

/// Conservative display-only tokenizer. Even malformed markup is never loaded
/// as HTML; script/style bodies and tag attributes are not exposed as content.
pub(crate) fn plain_text(html: &str, max_chars: usize) -> String {
    let mut text = String::new();
    let mut remaining = html;
    while !remaining.is_empty() && text.len() < max_chars.saturating_mul(4) {
        let Some(start) = remaining.find('<') else {
            text.push_str(remaining);
            break;
        };
        text.push_str(&remaining[..start]);
        remaining = &remaining[start..];
        if remaining.starts_with("<!--") {
            let Some(end) = remaining.find("-->") else {
                break;
            };
            remaining = &remaining[end + 3..];
            continue;
        }
        let Some(end) = tag_end(remaining) else { break };
        let tag = remaining[1..end].trim().trim_start_matches('/').trim();
        let name = tag
            .split(|ch: char| ch.is_whitespace() || ch == '/')
            .next()
            .unwrap_or("")
            .to_ascii_lowercase();
        let closing = remaining.starts_with("</");
        remaining = &remaining[end + 1..];
        if !closing && matches!(name.as_str(), "script" | "style" | "iframe" | "object") {
            let lower = remaining.to_ascii_lowercase();
            let Some(end) = lower.find(&format!("</{name}")) else {
                break;
            };
            remaining = &remaining[end..];
            continue;
        }
        if matches!(
            name.as_str(),
            "br" | "p" | "div" | "li" | "ul" | "ol" | "h1" | "h2" | "h3" | "tr"
        ) {
            text.push('\n');
        }
    }
    let decoded = html_escape::decode_html_entities(&text);
    let mut out = String::new();
    for line in decoded.lines() {
        let compact = line.split_whitespace().collect::<Vec<_>>().join(" ");
        if !compact.is_empty() {
            if !out.is_empty() {
                out.push('\n');
            }
            out.push_str(&compact);
        }
    }
    out.chars()
        .filter(|ch| *ch == '\n' || !ch.is_control())
        .take(max_chars)
        .collect()
}

fn tag_end(tag: &str) -> Option<usize> {
    let mut quote = None;
    for (index, ch) in tag.char_indices().skip(1) {
        if Some(ch) == quote {
            quote = None;
        } else if quote.is_none() && matches!(ch, '\'' | '"') {
            quote = Some(ch);
        } else if ch == '>' && quote.is_none() {
            return Some(index);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_platforms_and_missing_array_requirements() {
        let parsed = parse_details(42, &json!({"42":{"success":true,"data":{
            "steam_appid":42,"name":"Game", "platforms":{"windows":true,"mac":true,"linux":true},
            "pc_requirements":{"minimum":"<b>CPU:</b> 2 GHz<br>RAM: 4 GB"},
            "mac_requirements":{"minimum":"macOS 14", "recommended":"macOS 15"}, "linux_requirements":[]
        }}})).unwrap();
        assert_eq!(
            parsed["requirements"]["windows"]["minimum"],
            "CPU: 2 GHz\nRAM: 4 GB"
        );
        assert_eq!(parsed["requirements"]["mac"]["recommended"], "macOS 15");
        assert_eq!(parsed["requirements"]["linux"]["minimum"], "");
        assert_eq!(parsed["platforms"]["linux"], true);
        assert!(parse_details(43, &json!({"43":{"success":false}})).is_err());
    }

    #[test]
    fn publisher_html_stays_display_only_and_bounded() {
        let text = plain_text(
            "<ul><li>CPU &amp; RAM</li><li>中文 &#x2713;</li></ul><script>alert('bad')</script><style>bad{}</style><a title='>' href='file:///etc/passwd'>safe</a><!--hidden-->",
            200,
        );
        assert_eq!(text, "CPU & RAM\n中文 ✓\nsafe");
        assert_eq!(plain_text("中文中文", 3), "中文中");
        assert_eq!(plain_text("okay<script>unclosed", 100), "okay");
        assert_eq!(
            safe_header_url("https://cdn.steamstatic.com/header.jpg"),
            "https://cdn.steamstatic.com/header.jpg"
        );
        assert_eq!(
            safe_header_url("https://cdn.steamstatic.com.evil.invalid/header.jpg"),
            ""
        );
    }
}
