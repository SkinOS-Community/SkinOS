//! Account-scoped Steam website credentials obtained from the authenticated CM.
//!
//! Secrets travel only through the private core/UI pipe into an ephemeral
//! WKWebsiteDataStore. Do not log this module's return value or persist cookies.
use crate::{daemon::SharedConnection, session, store};
use anyhow::{Context, Result, bail, ensure};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use reqwest::{
    Url,
    cookie::Jar,
    header::{HeaderValue, LOCATION},
};
use serde_json::{Value, json};
use std::{
    io::Read,
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use steam_cm_protocol::{
    emsg::EMsg,
    protobuf::{
        CAuthenticationAccessTokenGenerateForAppRequest,
        CAuthenticationAccessTokenGenerateForAppResponse, CMsgProtoBufHeader,
    },
};
use zeroize::{Zeroize, Zeroizing};

const COOKIE_HOSTS: [&str; 3] = [
    "store.steampowered.com",
    "steamcommunity.com",
    "checkout.steampowered.com",
];
const MAX_WEB_BYTES: usize = 8 * 1024 * 1024;

#[derive(Clone, Copy)]
enum Target {
    Store,
    Community,
}
impl Target {
    fn parse(value: &str) -> Result<Self> {
        match value {
            "store" => Ok(Self::Store),
            "community" => Ok(Self::Community),
            _ => bail!("Unknown Steam website target"),
        }
    }
    fn host(self) -> &'static str {
        match self {
            Self::Store => COOKIE_HOSTS[0],
            Self::Community => COOKIE_HOSTS[1],
        }
    }
    fn destination(self, steamid: &str) -> String {
        match self {
            Self::Store => "https://store.steampowered.com/".to_owned(),
            Self::Community => format!("https://steamcommunity.com/profiles/{steamid}/"),
        }
    }
}

/// Mint a web access token through the *existing* authenticated connection.
/// A standalone HTTP refresh is rejected for SteamClient tokens by Steam.
pub async fn create_web_session(
    connection: SharedConnection,
    steamid: &str,
    target: &str,
) -> Result<Value> {
    let website = Target::parse(target)?;
    let expected_id = validate_steamid(steamid)?;
    let token = session::load_refresh_token(steamid)?;
    let mut request = CAuthenticationAccessTokenGenerateForAppRequest {
        refresh_token: Some(token.expose_secret().to_owned()),
        steamid: Some(expected_id),
        // Never rotate/revoke the user's long-lived login behind the runtime.
        renewal_type: Some(0),
    };
    let receiver = {
        let connection = connection.lock().await;
        let state = connection.state_snapshot().await;
        ensure!(
            state.steamid == Some(expected_id)
                && state.client_session_id.is_some()
                && state.close_reason.is_none(),
            "Steam connection does not belong to the current account"
        );
        let sent = connection
            .send_request(
                EMsg::ServiceMethodCallFromClient,
                CMsgProtoBufHeader {
                    steamid: Some(expected_id),
                    client_sessionid: state.client_session_id,
                    target_job_name: Some("Authentication.GenerateAccessTokenForApp#1".to_owned()),
                    ..Default::default()
                },
                &request,
            )
            .await;
        if let Some(secret) = request.refresh_token.as_mut() {
            secret.zeroize();
        }
        sent.context("Cannot request the Steam website session")?
    };
    let packet = tokio::time::timeout(Duration::from_secs(20), receiver)
        .await
        .context("Steam website authentication timed out")?
        .context("Steam website authentication was interrupted")?
        .context("Steam website authentication failed")?;
    let result = packet.header.eresult();
    ensure!(
        result == 1,
        "Steam website authentication failed (Steam result {result})"
    );
    let mut response: CAuthenticationAccessTokenGenerateForAppResponse = packet
        .decode_body()
        .context("Invalid Steam website authentication response")?;
    let access = Zeroizing::new(
        response
            .access_token
            .take()
            .context("Steam returned no website access token")?,
    );
    if let Some(secret) = response.refresh_token.as_mut() {
        secret.zeroize();
    }
    let now = unix_time();
    // JWT claims are used only for expiry/account consistency. The signature is
    // not trusted locally: a real Steam HTTPS page must accept the cookie below.
    let expiry = validate_access_token(&access, steamid, now)?;
    let session_id = random_hex(12)?;
    let login_cookie = Zeroizing::new(format!("{steamid}%7C%7C{}", access.as_str()));
    let cookies = session_cookies(&login_cookie, &session_id, expiry);
    let header = cookie_header(&cookies, website.host())?;
    tokio::time::timeout(
        Duration::from_secs(45),
        verify_identity(website, header, expected_id),
    )
    .await
    .context("Steam website identity verification timed out")??;
    Ok(json!({
        "steamid": steamid,
        "target": target,
        "url": website.destination(steamid),
        "expires_at": expiry,
        "cookies": cookies,
    }))
}

fn validate_steamid(value: &str) -> Result<u64> {
    ensure!(
        value.len() == 17 && value.bytes().all(|ch| ch.is_ascii_digit()),
        "Invalid SteamID"
    );
    let id: u64 = value.parse().context("Invalid SteamID")?;
    ensure!(
        id >> 56 == 1 && (id >> 52) & 0xf == 1,
        "Steam website sessions require an individual account"
    );
    Ok(id)
}

fn validate_access_token(token: &str, steamid: &str, now: u64) -> Result<u64> {
    ensure!(
        !token.is_empty()
            && token.len() <= 32_768
            && token
                .bytes()
                .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, b'-' | b'_' | b'.')),
        "Invalid Steam website token format"
    );
    let parts: Vec<_> = token.split('.').collect();
    ensure!(
        parts.len() == 3 && parts.iter().all(|part| !part.is_empty()),
        "Invalid Steam website token format"
    );
    let payload = Zeroizing::new(
        URL_SAFE_NO_PAD
            .decode(parts[1])
            .context("Invalid Steam website token encoding")?,
    );
    let claims: Value =
        serde_json::from_slice(&payload).context("Invalid Steam website token claims")?;
    ensure!(
        claims["sub"].as_str() == Some(steamid),
        "Steam website token belongs to another account"
    );
    let audience = claims["aud"]
        .as_array()
        .context("Steam website token has no audience")?;
    ensure!(
        audience.iter().any(|value| value == "web")
            && !audience.iter().any(|value| value == "renew"),
        "Steam returned a token that cannot be used for websites"
    );
    let expiry = claims["exp"]
        .as_u64()
        .context("Steam website token has no expiry")?;
    ensure!(
        expiry > now.saturating_add(60),
        "Steam website token has expired"
    );
    Ok(expiry.min(now.saturating_add(6 * 3600)))
}

fn random_hex(bytes: usize) -> Result<String> {
    let mut random = vec![0_u8; bytes];
    std::fs::File::open("/dev/urandom")
        .context("Cannot initialize secure session randomness")?
        .read_exact(&mut random)
        .context("Cannot generate the website session ID")?;
    Ok(random.iter().map(|byte| format!("{byte:02x}")).collect())
}

fn session_cookies(login: &str, session_id: &str, expiry: u64) -> Vec<Value> {
    COOKIE_HOSTS.iter().flat_map(|host| [
        json!({"name":"steamLoginSecure","value":login,"domain":host,"path":"/","secure":true,"http_only":true,"expires_at":expiry}),
        json!({"name":"sessionid","value":session_id,"domain":host,"path":"/","secure":true,"http_only":false,"expires_at":expiry}),
    ]).collect()
}

fn cookie_header(cookies: &[Value], host: &str) -> Result<HeaderValue> {
    ensure!(
        COOKIE_HOSTS.contains(&host),
        "Steam cookies cannot be sent to this host"
    );
    let pairs: Vec<String> = cookies
        .iter()
        .filter(|cookie| cookie["domain"].as_str() == Some(host))
        .map(|cookie| {
            let name = cookie["name"]
                .as_str()
                .context("Invalid Steam cookie name")?;
            let value = cookie["value"]
                .as_str()
                .context("Invalid Steam cookie value")?;
            ensure!(
                matches!(name, "steamLoginSecure" | "sessionid"),
                "Unexpected Steam cookie"
            );
            ensure!(
                cookie["secure"] == true
                    && cookie["path"] == "/"
                    && !value
                        .bytes()
                        .any(|ch| ch <= 0x20 || ch >= 0x7f || matches!(ch, b';' | b',')),
                "Invalid Steam cookie attributes"
            );
            Ok(format!("{name}={value}"))
        })
        .collect::<Result<_>>()?;
    ensure!(pairs.len() == 2, "Incomplete Steam website cookies");
    let serialized = Zeroizing::new(pairs.join("; "));
    let mut header = HeaderValue::from_str(&serialized)
        .map_err(|_| anyhow::anyhow!("Invalid Steam website cookie header"))?;
    header.set_sensitive(true);
    Ok(header)
}

fn allow_identity_url(url: &Url, expected_host: &str) -> bool {
    COOKIE_HOSTS.contains(&expected_host)
        && url.scheme() == "https"
        && url.host_str() == Some(expected_host)
        && url.username().is_empty()
        && url.password().is_none()
        && url.port().is_none()
}

async fn verify_identity(target: Target, cookies: HeaderValue, steamid: u64) -> Result<()> {
    let mut url = Url::parse(&format!("https://{}/", target.host()))?;
    let jar = Arc::new(Jar::default());
    for pair in cookies
        .to_str()
        .context("Invalid Steam cookie encoding")?
        .split("; ")
    {
        jar.add_cookie_str(&format!("{pair}; Path=/; Secure"), &url);
    }
    // Store redirects can set country/language/browser cookies; retaining those
    // within this disposable jar avoids a redirect loop. The client only sends
    // requests to the exact target host and is dropped after identity proof.
    let client = store::http_client_builder()
        .cookie_provider(jar)
        .build()
        .context("Cannot initialize the Steam web connection")?;
    for _ in 0..4 {
        ensure!(
            allow_identity_url(&url, target.host()),
            "Steam website redirected outside its account domain"
        );
        let response = client
            .get(url.clone())
            .send()
            .await
            .map_err(|_| anyhow::anyhow!("Cannot verify the Steam website login"))?;
        if response.status().is_redirection() {
            let location = response
                .headers()
                .get(LOCATION)
                .and_then(|value| value.to_str().ok())
                .context("Steam website returned an invalid redirect")?;
            url = url
                .join(location)
                .context("Steam website returned an invalid redirect")?;
            continue;
        }
        let bytes = store::bounded_body(response, MAX_WEB_BYTES).await?;
        let html =
            std::str::from_utf8(&bytes).context("Invalid Steam website response encoding")?;
        ensure!(
            page_identity_matches(html, steamid),
            "Steam website did not confirm the current account; sign in again or retry"
        );
        return Ok(());
    }
    bail!("Steam website returned too many redirects")
}

fn page_identity_matches(html: &str, steamid: u64) -> bool {
    if numeric_assignment(html, "g_steamID") == Some(steamid) {
        return true;
    }
    if numeric_assignment(html, "g_AccountID") == Some(steamid & 0xffff_ffff) {
        return true;
    }
    // The store can serve its React bootstrap instead of the legacy globals.
    if let Some(rest) = html.split("data-userinfo=\"").nth(1)
        && let Some(encoded) = rest.split('"').next()
        && let Ok(data) = serde_json::from_str::<Value>(&html_escape::decode_html_entities(encoded))
    {
        return data["logged_in"] == true && data["steamid"].as_str() == Some(&steamid.to_string());
    }
    false
}

fn numeric_assignment(html: &str, name: &str) -> Option<u64> {
    let (_, rest) = html.split_once(&format!("var {name}"))?;
    let rest = rest
        .trim_start()
        .strip_prefix('=')?
        .trim_start()
        .trim_start_matches(['\'', '"']);
    let number: String = rest.chars().take_while(char::is_ascii_digit).collect();
    number.parse().ok()
}

fn unix_time() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

#[cfg(test)]
mod tests {
    use super::*;
    const ID: &str = "76561198012345678";

    #[test]
    fn tokens_are_account_bound_and_must_be_web_access_tokens() {
        let token = |sub: &str, aud: Value, exp: u64| {
            format!(
                "e30.{}.signature",
                URL_SAFE_NO_PAD
                    .encode(serde_json::to_vec(&json!({"sub":sub,"aud":aud,"exp":exp})).unwrap())
            )
        };
        assert_eq!(
            validate_access_token(&token(ID, json!(["web", "client"]), 2000), ID, 1000).unwrap(),
            2000
        );
        assert!(
            validate_access_token(&token("76561198012345679", json!(["web"]), 2000), ID, 1000)
                .is_err()
        );
        assert!(
            validate_access_token(&token(ID, json!(["web", "renew"]), 2000), ID, 1000).is_err()
        );
        assert!(validate_access_token(&token(ID, json!(["web"]), 999), ID, 1000).is_err());
        assert!(validate_access_token("x.y.z; injected=yes", ID, 0).is_err());
        assert_eq!(validate_steamid(ID).unwrap(), 76_561_198_012_345_678);
    }

    #[test]
    fn cookie_scope_and_redirects_are_exact_host_only() {
        let cookies = session_cookies("fake-login", "random", 2000);
        let header = cookie_header(&cookies, "store.steampowered.com").unwrap();
        assert!(header.is_sensitive());
        assert_eq!(
            header.to_str().unwrap(),
            "steamLoginSecure=fake-login; sessionid=random"
        );
        for host in [
            "steampowered.com",
            "store.steampowered.com.evil.test",
            "localhost",
            "example.com",
        ] {
            assert!(cookie_header(&cookies, host).is_err());
        }
        for value in [
            "http://store.steampowered.com/",
            "https://steamcommunity.com/",
            "https://store.steampowered.com.evil.test/",
            "https://evil@store.steampowered.com/",
            "https://store.steampowered.com:8443/",
        ] {
            assert!(!allow_identity_url(
                &Url::parse(value).unwrap(),
                "store.steampowered.com"
            ));
        }
        assert!(allow_identity_url(
            &Url::parse("https://store.steampowered.com/account/").unwrap(),
            "store.steampowered.com"
        ));
    }

    #[test]
    fn website_requires_logged_in_identity_not_profile_text() {
        let id = ID.parse().unwrap();
        assert!(page_identity_matches(
            &format!("var g_steamID = \"{ID}\";"),
            id
        ));
        assert!(page_identity_matches(
            &format!("var g_AccountID = {};", id & 0xffff_ffff),
            id
        ));
        assert!(!page_identity_matches(
            &format!("/profiles/{ID}/ var g_AccountID = 0;"),
            id
        ));
        assert!(!page_identity_matches("var g_steamID = false;", id));
        assert!(!page_identity_matches(
            "var g_steamID = \"76561198012345679\";",
            id
        ));
    }
}
