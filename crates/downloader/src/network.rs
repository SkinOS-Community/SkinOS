use crate::{DepotSelection, SharedConnection, content, proto::*};
use anyhow::{Context, Result, bail, ensure};
use futures_util::StreamExt;
use prost::Message;
use std::time::Duration;
use steam_cm_protocol::{
    emsg::EMsg,
    kv::{KVValue, parse_binary_kv},
    protobuf::*,
};

pub(crate) struct PreparedDepot {
    pub depot_id: u32,
    pub key: [u8; 32],
    pub servers: Vec<(String, Option<String>)>,
    pub files: Vec<FileMapping>,
    pub metadata: ManifestMetadata,
}

const MAX_PREPARED_SERVERS: usize = 6;

async fn request<Req: Message, Resp: Message + Default>(
    connection: &SharedConnection,
    emsg: EMsg,
    method: Option<&str>,
    body: &Req,
) -> Result<Resp> {
    let receiver = {
        let connection = connection.lock().await;
        let state = connection.state_snapshot().await;
        ensure!(
            state.steamid.is_some() && state.close_reason.is_none(),
            "Steam session disconnected"
        );
        connection
            .send_request(
                emsg,
                CMsgProtoBufHeader {
                    steamid: state.steamid,
                    client_sessionid: state.client_session_id,
                    target_job_name: method.map(str::to_owned),
                    ..Default::default()
                },
                body,
            )
            .await?
    };
    let packet = tokio::time::timeout(Duration::from_secs(40), receiver)
        .await
        .context("Steam request timed out")???;
    if let Some(result) = packet.header.eresult {
        ensure!(result == 1, "Steam rejected the request (result {result})");
    }
    Ok(packet.decode_body::<Resp>()?)
}

async fn service<Req: Message, Resp: Message + Default>(
    connection: &SharedConnection,
    name: &str,
    body: &Req,
) -> Result<Resp> {
    request(
        connection,
        EMsg::ServiceMethodCallFromClient,
        Some(name),
        body,
    )
    .await
}

pub(crate) async fn app_info(connection: &SharedConnection, appid: u32) -> Result<KVValue> {
    let tokens: CMsgClientPicsAccessTokenResponse = request(
        connection,
        EMsg::ClientPICSAccessTokenRequest,
        None,
        &CMsgClientPicsAccessTokenRequest {
            appids: vec![appid],
            packageids: vec![],
        },
    )
    .await?;
    ensure!(
        !tokens.app_denied_tokens.contains(&appid),
        "Steam denied access to app metadata"
    );
    let token = tokens
        .app_access_tokens
        .iter()
        .find(|t| t.appid == Some(appid))
        .and_then(|t| t.access_token)
        .unwrap_or(0);
    let response: CMsgClientPicsProductInfoResponse = request(
        connection,
        EMsg::ClientPICSProductInfoRequest,
        None,
        &CMsgClientPicsProductInfoRequest {
            apps: vec![c_msg_client_pics_product_info_request::AppInfo {
                appid: Some(appid),
                access_token: Some(token),
                only_public_obsolete: Some(false),
            }],
            single_response: Some(true),
            meta_data_only: Some(false),
            ..Default::default()
        },
    )
    .await?;
    ensure!(
        response.response_pending != Some(true),
        "Incomplete app metadata response"
    );
    let app = response
        .apps
        .into_iter()
        .find(|app| app.appid == Some(appid))
        .context("Steam did not return app metadata")?;
    ensure!(
        app.missing_token != Some(true),
        "App metadata requires a valid access token"
    );
    decode_app_info(&app.buffer.context("App metadata is empty")?)
}

fn servers_request(download_region_id: Option<u32>) -> Result<ServersRequest> {
    crate::validate_download_region(download_region_id)?;
    Ok(ServersRequest {
        cell_id: Some(download_region_id.unwrap_or(0)),
        // Steam may return nearby fallback cells first even for an explicit
        // preference. Ask for enough candidates to include the selected cell.
        max_servers: Some(if download_region_id.is_some() {
            100
        } else {
            20
        }),
    })
}

pub(crate) async fn servers(
    connection: &SharedConnection,
    appid: u32,
    download_region_id: Option<u32>,
) -> Result<Vec<String>> {
    let response: ServersResponse = service(
        connection,
        "ContentServerDirectory.GetServersForSteamPipe#1",
        &servers_request(download_region_id)?,
    )
    .await?;
    select_server_hosts(response, appid, download_region_id)
}

fn select_server_hosts(
    response: ServersResponse,
    appid: u32,
    download_region_id: Option<u32>,
) -> Result<Vec<String>> {
    let mut servers = response.servers;
    servers.sort_by(|a, b| {
        let matches_region = |server: &Server| {
            download_region_id.is_some_and(|selected| {
                server.cell_id.and_then(|cell| u32::try_from(cell).ok()) == Some(selected)
            })
        };
        matches_region(b).cmp(&matches_region(a)).then_with(|| {
            a.weighted_load
                .unwrap_or(0.0)
                .total_cmp(&b.weighted_load.unwrap_or(0.0))
        })
    });
    let mut hosts = Vec::new();
    let mut preferred_count = 0;
    for server in servers {
        if server.use_as_proxy == Some(true) {
            continue;
        }
        if !server.allowed_app_ids.is_empty() && !server.allowed_app_ids.contains(&appid) {
            continue;
        }
        if server.https_support.as_deref() == Some("none") {
            continue;
        }
        let preferred = download_region_id.is_some_and(|selected| {
            server.cell_id.and_then(|cell| u32::try_from(cell).ok()) == Some(selected)
        });
        let Some(host) = server.vhost.or(server.host) else {
            continue;
        };
        // Hosts become URL authorities. No paths, userinfo or custom ports.
        if host.is_empty()
            || !host
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'-')
        {
            continue;
        }
        if !hosts.contains(&host) {
            hosts.push(host);
            preferred_count += usize::from(preferred);
        }
    }
    ensure!(
        !hosts.is_empty(),
        "Steam returned no usable HTTPS content servers"
    );
    if download_region_id.is_some() {
        // Depot authorization and chunk failover use at most six hosts. Reserve
        // two fallback slots so a busy/unavailable selected cell does not use
        // the entire retry budget. Underfilled groups lend their spare slots.
        let fallback_count = hosts.len() - preferred_count;
        let preferred_slots = preferred_count.min(MAX_PREPARED_SERVERS - fallback_count.min(2));
        let fallback_slots = fallback_count.min(MAX_PREPARED_SERVERS - preferred_slots);
        let mut candidates = Vec::with_capacity(preferred_slots + fallback_slots);
        candidates.extend(hosts[..preferred_slots].iter().cloned());
        candidates.extend(
            hosts[preferred_count..preferred_count + fallback_slots]
                .iter()
                .cloned(),
        );
        return Ok(candidates);
    }
    Ok(hosts)
}

pub(crate) async fn prepare_depot(
    connection: &SharedConnection,
    client: &reqwest::Client,
    hosts: &[String],
    selection: &DepotSelection,
) -> Result<PreparedDepot> {
    let key_response: DepotKeyResponse = request(
        connection,
        EMsg::ClientGetDepotDecryptionKey,
        None,
        &DepotKeyRequest {
            depot_id: Some(selection.depot_id),
            app_id: Some(selection.source_appid),
        },
    )
    .await?;
    ensure!(
        key_response.eresult == Some(1) && key_response.depot_id == Some(selection.depot_id),
        "Account cannot download depot {} (Steam result {})",
        selection.depot_id,
        key_response.eresult.unwrap_or(2)
    );
    let key: [u8; 32] = key_response
        .depot_encryption_key
        .context("Missing depot key")?
        .try_into()
        .map_err(|_| anyhow::anyhow!("Invalid depot key length"))?;
    let code: ManifestCodeResponse = service(
        connection,
        "ContentServerDirectory.GetManifestRequestCode#1",
        &ManifestCodeRequest {
            app_id: Some(selection.source_appid),
            depot_id: Some(selection.depot_id),
            manifest_id: Some(selection.manifest_id),
            app_branch: Some("public".into()),
        },
    )
    .await?;
    let code = code
        .manifest_request_code
        .filter(|c| *c != 0)
        .context("Steam did not authorize the manifest download")?;
    // Token lookup is optional: a slow token endpoint must not multiply a
    // 40-second timeout by six hosts before the first manifest is downloaded.
    // Depot key and manifest code above remain mandatory license checks.
    let servers = futures_util::stream::iter(hosts.iter().take(MAX_PREPARED_SERVERS).cloned())
        .map(|host| async move {
            let token = tokio::time::timeout(
                Duration::from_secs(5),
                service::<_, CdnTokenResponse>(
                    connection,
                    "ContentServerDirectory.GetCDNAuthToken#1",
                    &CdnTokenRequest {
                        depot_id: Some(selection.depot_id),
                        host_name: Some(host.clone()),
                        app_id: Some(selection.source_appid),
                    },
                ),
            )
            .await
            .ok()
            .and_then(Result::ok)
            .and_then(|r| r.token);
            (host.clone(), token)
        })
        .buffered(3)
        .collect::<Vec<_>>()
        .await;
    let path = format!(
        "depot/{}/manifest/{}/5/{code}",
        selection.depot_id, selection.manifest_id
    );
    let data = fetch(client, &servers, &path, 64 * 1024 * 1024).await?;
    let depot_id = selection.depot_id;
    let manifest_id = selection.manifest_id;
    let (metadata, files) = tokio::task::spawn_blocking(move || {
        content::decode_manifest(&data, depot_id, manifest_id, &key)
    })
    .await??;
    Ok(PreparedDepot {
        depot_id: selection.depot_id,
        key,
        servers,
        files,
        metadata,
    })
}

/// Content tokens are a full query string, per SteamKit's CDN.Client.BuildCommand.
/// Never put token-bearing URLs or reqwest's full error string into user logs.
pub(crate) async fn fetch(
    client: &reqwest::Client,
    servers: &[(String, Option<String>)],
    path: &str,
    max_bytes: usize,
) -> Result<Vec<u8>> {
    fetch_with_progress(client, servers, path, max_bytes, &mut |_| {}).await
}

pub(crate) async fn fetch_with_progress(
    client: &reqwest::Client,
    servers: &[(String, Option<String>)],
    path: &str,
    max_bytes: usize,
    received: &mut (dyn FnMut(usize) + Send),
) -> Result<Vec<u8>> {
    let mut last = "content server unavailable".to_owned();
    for (host, token) in servers {
        let mut url = reqwest::Url::parse(&format!("https://{host}/{path}"))?;
        if let Some(token) = token.as_deref().filter(|v| !v.is_empty()) {
            url.set_query(Some(token.trim_start_matches('?')));
        }
        let response = match client.get(url).send().await {
            Ok(response) => response,
            Err(error) => {
                last = if error.is_timeout() {
                    "request timed out"
                } else {
                    "connection failed"
                }
                .into();
                continue;
            }
        };
        if !response.status().is_success() {
            last = format!("HTTP {}", response.status().as_u16());
            continue;
        }
        if response
            .content_length()
            .is_some_and(|len| len > max_bytes as u64)
        {
            bail!("Content response exceeds size limit")
        }
        match read_response(response, max_bytes, received).await {
            Ok(data) => return Ok(data),
            Err(_) => last = "content transfer interrupted".into(),
        }
    }
    bail!("Не удалось получить файлы игры: {last}")
}

async fn read_response(
    response: reqwest::Response,
    max_bytes: usize,
    received: &mut (dyn FnMut(usize) + Send),
) -> Result<Vec<u8>> {
    let mut stream = response.bytes_stream();
    let mut data = Vec::new();
    while let Some(item) = stream.next().await {
        let bytes = item.map_err(|_| anyhow::anyhow!("Content transfer interrupted"))?;
        ensure!(
            data.len().saturating_add(bytes.len()) <= max_bytes,
            "Content response exceeds size limit"
        );
        received(bytes.len());
        data.extend_from_slice(&bytes);
    }
    Ok(data)
}

/// PICS app buffers can be textual VDF or binary KV; normalize to app contents.
fn decode_app_info(buffer: &[u8]) -> Result<KVValue> {
    fn convert(value: keyvalues_parser::Value<'_>) -> KVValue {
        match value {
            keyvalues_parser::Value::Str(value) => KVValue::Str(value.into_owned()),
            keyvalues_parser::Value::Obj(values) => KVValue::Nested(
                values
                    .into_inner()
                    .into_iter()
                    .flat_map(|(key, values)| {
                        values
                            .into_iter()
                            .map(move |value| (key.to_string(), convert(value)))
                    })
                    .collect(),
            ),
        }
    }
    let root = if let Ok(text) = std::str::from_utf8(buffer) {
        let text = text.trim_end_matches('\0').trim();
        if text.starts_with('"') || text.starts_with("appinfo") {
            let parsed =
                keyvalues_parser::parse(text).context("Cannot decode text Steam app metadata")?;
            ensure!(
                parsed.bases.is_empty(),
                "External VDF includes are unsupported"
            );
            KVValue::Nested(vec![(parsed.key.into_owned(), convert(parsed.value))])
        } else {
            parse_binary_kv(buffer).context("Cannot decode binary Steam app metadata")?
        }
    } else {
        parse_binary_kv(buffer).context("Cannot decode binary Steam app metadata")?
    };
    let root = root.get("appinfo").cloned().unwrap_or(root);
    ensure!(
        root.get("common").is_some(),
        "App metadata has no common section"
    );
    Ok(root)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn server_directory_wire_request_uses_selected_region_and_preserves_automatic_mode() {
        // Belarus is cell 165 in Valve's client_cell_list; it deliberately uses
        // a two-byte protobuf varint, catching accidental truncation as well.
        let selected = servers_request(Some(165)).unwrap();
        assert_eq!(selected.encode_to_vec(), [0x08, 0xa5, 0x01, 0x10, 0x64]);
        let automatic = servers_request(None).unwrap();
        assert_eq!(automatic.encode_to_vec(), [0x08, 0x00, 0x10, 0x14]);
        assert!(servers_request(Some(0)).is_err());
        assert!(servers_request(Some(u32::MAX)).is_err());
    }

    fn content_server(host: &str, cell: i32, load: f32) -> Server {
        Server {
            host: Some(host.into()),
            cell_id: Some(cell),
            weighted_load: Some(load),
            https_support: Some("mandatory".into()),
            ..Default::default()
        }
    }

    #[test]
    fn explicit_cell_wins_over_lower_load_fallback_and_keeps_failover_hosts() {
        let servers = vec![
            content_server("warsaw.example", 38, 1.0),
            content_server("frankfurt-b.example", 5, 50.0),
            content_server("frankfurt-a.example", 5, 25.0),
        ];
        let response = ServersResponse { servers };
        assert_eq!(
            select_server_hosts(response.clone(), 570, Some(5)).unwrap(),
            [
                "frankfurt-a.example",
                "frankfurt-b.example",
                "warsaw.example"
            ]
        );
        // Automatic selection continues to use Valve's weighted load order.
        assert_eq!(
            select_server_hosts(response.clone(), 570, None).unwrap(),
            [
                "warsaw.example",
                "frankfurt-a.example",
                "frankfurt-b.example"
            ]
        );
        // A selected cell absent from the returned pool retains valid fallback.
        assert_eq!(
            select_server_hosts(response, 570, Some(165)).unwrap(),
            [
                "warsaw.example",
                "frankfurt-a.example",
                "frankfurt-b.example"
            ]
        );
    }

    #[test]
    fn six_host_retry_budget_reserves_real_fallback_after_filtering_and_deduplication() {
        let mut servers: Vec<_> = (0..8)
            .map(|i| content_server(&format!("preferred-{i}.example"), 5, 10.0 + i as f32))
            .collect();
        servers.push(content_server("preferred-0.example", 5, 0.0));
        servers.push(content_server("preferred-1.example", 38, 0.0));
        let mut rejected = content_server("bad-fallback.example", 38, 0.0);
        rejected.use_as_proxy = Some(true);
        servers.push(rejected);
        servers.extend(
            (0..3).map(|i| content_server(&format!("fallback-{i}.example"), 38, 1.0 + i as f32)),
        );
        let response = ServersResponse { servers };
        let selected = select_server_hosts(response.clone(), 570, Some(5)).unwrap();
        assert_eq!(
            selected
                .iter()
                .take(MAX_PREPARED_SERVERS)
                .map(String::as_str)
                .collect::<Vec<_>>(),
            [
                "preferred-0.example",
                "preferred-1.example",
                "preferred-2.example",
                "preferred-3.example",
                "fallback-0.example",
                "fallback-1.example"
            ]
        );
        assert_eq!(selected.len(), MAX_PREPARED_SERVERS);
        // Automatic keeps the full weighted-load pool and its original limit
        // is still applied by prepare_depot, not by region selection.
        let automatic = select_server_hosts(response, 570, None).unwrap();
        assert_eq!(automatic.len(), 11);
        assert_eq!(automatic[0], "preferred-0.example");
    }

    #[test]
    fn underfilled_region_groups_lend_retry_slots_to_the_other_group() {
        for (preferred_count, fallback_count, expected_preferred, expected_fallback) in
            [(8, 1, 5, 1), (1, 8, 1, 5), (8, 0, 6, 0), (0, 8, 0, 6)]
        {
            let servers = (0..preferred_count)
                .map(|i| content_server(&format!("preferred-{i}.example"), 5, i as f32))
                .chain(
                    (0..fallback_count)
                        .map(|i| content_server(&format!("fallback-{i}.example"), 38, i as f32)),
                )
                .collect();
            let selected = select_server_hosts(ServersResponse { servers }, 570, Some(5)).unwrap();
            assert_eq!(selected.len(), MAX_PREPARED_SERVERS);
            assert!(
                selected[..expected_preferred]
                    .iter()
                    .all(|host| host.starts_with("preferred-"))
            );
            assert_eq!(selected[expected_preferred..].len(), expected_fallback);
            assert!(
                selected[expected_preferred..]
                    .iter()
                    .all(|host| host.starts_with("fallback-"))
            );
        }
    }

    #[test]
    fn preference_does_not_bypass_content_server_safety_or_app_filters() {
        let mut proxy = content_server("proxy.example", 5, 0.0);
        proxy.use_as_proxy = Some(true);
        let mut other_app = content_server("other-app.example", 5, 0.0);
        other_app.allowed_app_ids = vec![10];
        let mut no_https = content_server("http.example", 5, 0.0);
        no_https.https_support = Some("none".into());
        let malformed = content_server("user@malformed.example/path", 5, 0.0);
        let mut selected = content_server("selected.example", 5, 100.0);
        selected.allowed_app_ids = vec![570];
        let fallback = content_server("fallback.example", 38, 1.0);
        let response = ServersResponse {
            servers: vec![proxy, other_app, no_https, malformed, fallback, selected],
        };
        assert_eq!(
            select_server_hosts(response, 570, Some(5)).unwrap(),
            ["selected.example", "fallback.example"]
        );
    }

    #[test]
    fn content_server_cell_decodes_valve_wire_tag() {
        let server = Server::decode([0x18, 0xa5, 0x01].as_slice()).unwrap();
        assert_eq!(server.cell_id, Some(165));
    }
    #[tokio::test]
    async fn pause_interrupts_an_actual_http_body_that_has_stalled() {
        use crate::{InstallControl, InstallInterrupted};
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let (finish, receiver) = std::sync::mpsc::channel();
        let server = std::thread::spawn(move || {
            let (mut socket, _) = listener.accept().unwrap();
            let mut request = [0; 1024];
            let _ = socket.read(&mut request);
            socket
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 1024\r\nConnection: close\r\n\r\nx")
                .unwrap();
            let _ = receiver.recv_timeout(Duration::from_secs(2));
        });
        let control = InstallControl::new();
        let pause = control.clone();
        let now = std::time::Instant::now();
        let result = tokio::time::timeout(
            Duration::from_secs(1),
            control.run(async {
                let response = reqwest::Client::new()
                    .get(format!("http://{address}/slow"))
                    .send()
                    .await?;
                read_response(response, 1024, &mut |_| {
                    pause.pause();
                })
                .await
            }),
        )
        .await
        .unwrap();
        finish.send(()).unwrap();
        server.join().unwrap();
        assert_eq!(
            result.unwrap_err().downcast_ref::<InstallInterrupted>(),
            Some(&InstallInterrupted::Paused)
        );
        assert!(now.elapsed() < Duration::from_secs(1));
    }
    #[test]
    fn accepts_text_pics_and_normalizes_appinfo_root() {
        let buffer = b"\"appinfo\" { \"common\" { \"oslist\" \"windows,macos\" } \"depots\" { \"1942282\" { \"manifests\" { \"public\" { \"gid\" \"12345678901234567\" } } } } }\0";
        let root = decode_app_info(buffer).unwrap();
        assert_eq!(
            root.get("common").unwrap().get("oslist").unwrap().as_str(),
            Some("windows,macos")
        );
        assert!(root.get("depots").unwrap().get("1942282").is_some());
    }
    #[test]
    fn accepts_binary_pics_root_contents() {
        let bytes = b"\0appinfo\0\0common\0\x01oslist\0macos\0\x08\x08";
        let root = decode_app_info(bytes).unwrap();
        assert_eq!(
            root.get("common").unwrap().get("oslist").unwrap().as_str(),
            Some("macos")
        );
    }
}
