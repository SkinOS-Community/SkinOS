//! Read-only Steam notification service and real CM pushes. No read/view/hide RPCs.
use crate::{
    connection::Connection,
    emsg::EMsg,
    error::{Error, Result},
    message::Packet,
    protobuf::{
        CMsgProtoBufHeader, CSteamNotificationGetSteamNotificationsRequest,
        CSteamNotificationGetSteamNotificationsResponse,
        CSteamNotificationNotificationsReceivedNotification, SteamNotificationData,
    },
};
use std::{sync::Arc, time::Duration};
use tokio::sync::Mutex;
const MAX_RESPONSE_BYTES: usize = 2 * 1024 * 1024;
pub const MAX_NOTIFICATIONS: usize = 200;
const PUSH_METHOD: &str = "SteamNotificationClient.NotificationsReceived#1";

#[derive(Clone)]
pub struct NotificationBatch {
    pub owner_steamid: Option<u64>,
    pub notifications: Vec<SteamNotificationData>,
    pub pending_gift_count: Option<u32>,
    pub pending_friend_count: Option<u32>,
    pub pending_family_invite_count: Option<u32>,
}
impl std::fmt::Debug for NotificationBatch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NotificationBatch")
            .field("count", &self.notifications.len())
            .finish_non_exhaustive()
    }
}
fn bounded(mut notifications: Vec<SteamNotificationData>) -> Vec<SteamNotificationData> {
    notifications.sort_by_key(|row| std::cmp::Reverse(row.timestamp.unwrap_or_default()));
    notifications.truncate(MAX_NOTIFICATIONS);
    notifications
}
pub fn decode_incoming(packet: &Packet) -> Option<NotificationBatch> {
    if !matches!(packet.emsg,value if value==EMsg::ServiceMethod.raw() || value==EMsg::ServiceMethodSendToClient.raw())
        || packet.target_job_name() != Some(PUSH_METHOD)
        || packet.body.len() > MAX_RESPONSE_BYTES
    {
        return None;
    }
    let body = packet
        .decode_body::<CSteamNotificationNotificationsReceivedNotification>()
        .ok()?;
    Some(NotificationBatch {
        owner_steamid: packet.header.steamid.filter(|id| *id != 0),
        notifications: bounded(body.notifications),
        pending_gift_count: body.pending_gift_count,
        pending_friend_count: body.pending_friend_count,
        pending_family_invite_count: body.pending_family_invite_count,
    })
}
/// No pagination field exists in this RPC. Bound time, encoded response size and
/// returned rows locally rather than inventing unsupported paging arguments.
pub async fn fetch(connection: Arc<Mutex<Connection>>, steamid: u64) -> Result<NotificationBatch> {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(12);
    let (job, mut response) = tokio::time::timeout_at(deadline, async {
        let connection = connection.lock().await;
        let state = connection.state_snapshot().await;
        if state.steamid != Some(steamid)
            || state.client_session_id.is_none()
            || state.close_reason.is_some()
        {
            return Err(Error::Authentication(
                "Notification session identity mismatch".into(),
            ));
        }
        connection
            .send_request_stream(
                EMsg::ServiceMethodCallFromClient,
                CMsgProtoBufHeader {
                    steamid: Some(steamid),
                    client_sessionid: state.client_session_id,
                    target_job_name: Some("SteamNotification.GetSteamNotifications#1".into()),
                    ..Default::default()
                },
                &CSteamNotificationGetSteamNotificationsRequest {
                    include_hidden: Some(false),
                    language: Some(8),
                    include_confirmation_count: Some(false),
                    include_pinned_counts: Some(true),
                    include_read: Some(true),
                    count_only: Some(false),
                },
            )
            .await
    })
    .await
    .map_err(|_| Error::Transport("Steam notification request timed out".into()))??;
    let result = tokio::time::timeout_at(deadline, response.recv()).await;
    // Release the CM mutex while waiting; history must not block friends/library.
    let _ = tokio::time::timeout(Duration::from_secs(1), async {
        connection.lock().await.end_stream(job).await;
    })
    .await;
    let packet = result
        .map_err(|_| Error::Transport("Steam notifications timed out".into()))?
        .ok_or(Error::Closed)??;
    if packet.header.eresult() != 1 {
        return Err(Error::Protocol(format!(
            "Steam notifications rejected (result {})",
            packet.header.eresult()
        )));
    }
    if packet.body.len() > MAX_RESPONSE_BYTES {
        return Err(Error::Protocol(
            "Steam notification response exceeds limit".into(),
        ));
    }
    if packet
        .header
        .steamid
        .is_some_and(|id| id != 0 && id != steamid)
    {
        return Err(Error::Authentication(
            "Notification response account mismatch".into(),
        ));
    }
    let body = packet.decode_body::<CSteamNotificationGetSteamNotificationsResponse>()?;
    Ok(NotificationBatch {
        owner_steamid: Some(steamid),
        notifications: bounded(body.notifications),
        pending_gift_count: body.pending_gift_count,
        pending_friend_count: body.pending_friend_count,
        pending_family_invite_count: body.pending_family_invite_count,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::message::{decode_frame, encode_message};
    use futures_util::{SinkExt, StreamExt};
    const SELF: u64 = 76561198000000001;
    #[test]
    fn push_decoder_checks_method_kind_and_limits_rows() {
        let body = CSteamNotificationNotificationsReceivedNotification {
            notifications: (1..250)
                .map(|id| SteamNotificationData {
                    notification_id: Some(id),
                    timestamp: Some(id as u32),
                    ..Default::default()
                })
                .collect(),
            ..Default::default()
        };
        let header = CMsgProtoBufHeader {
            steamid: Some(SELF),
            target_job_name: Some(PUSH_METHOD.into()),
            ..Default::default()
        };
        let bytes = encode_message(EMsg::ServiceMethod, &header, &body).unwrap();
        let mut packet = decode_frame(&bytes).unwrap().remove(0);
        let batch = decode_incoming(&packet).unwrap();
        assert_eq!(batch.owner_steamid, Some(SELF));
        assert_eq!(batch.notifications.len(), MAX_NOTIFICATIONS);
        assert_eq!(batch.notifications[0].notification_id, Some(249));
        packet.header.target_job_name = Some("Unknown.Push#1".into());
        assert!(decode_incoming(&packet).is_none());
    }
    #[tokio::test]
    async fn fetch_uses_only_readonly_rpc_and_does_not_mark_or_hide_anything() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut socket = tokio_tungstenite::accept_async(stream).await.unwrap();
            let packet = loop {
                let bytes = socket.next().await.unwrap().unwrap().into_data();
                let packet = decode_frame(&bytes).unwrap().remove(0);
                if packet.emsg == EMsg::ClientHeartBeat.raw() {
                    continue;
                }
                break packet;
            };
            assert_eq!(
                packet.target_job_name(),
                Some("SteamNotification.GetSteamNotifications#1")
            );
            assert_eq!(packet.emsg, EMsg::ServiceMethodCallFromClient.raw());
            let request = packet
                .decode_body::<CSteamNotificationGetSteamNotificationsRequest>()
                .unwrap();
            assert_eq!(request.include_hidden, Some(false));
            assert_eq!(request.count_only, Some(false));
            assert_eq!(request.include_confirmation_count, Some(false));
            let header = CMsgProtoBufHeader {
                steamid: Some(SELF),
                eresult: Some(1),
                jobid_target: packet.header.jobid_source,
                ..Default::default()
            };
            let response = CSteamNotificationGetSteamNotificationsResponse {
                pending_friend_count: Some(2),
                ..Default::default()
            };
            socket
                .send(tokio_tungstenite::tungstenite::Message::Binary(
                    encode_message(EMsg::ServiceMethodResponse, &header, &response).unwrap(),
                ))
                .await
                .unwrap();
            tokio::select! {_=stopped=>{},unexpected=socket.next()=>panic!("unexpected request after read-only fetch: {}",unexpected.is_some())}
        });
        let mut conn = Connection::connect(&format!("ws://{address}"))
            .await
            .unwrap();
        conn.set_logged_on(SELF, 7, 3600).await.unwrap();
        let result = fetch(Arc::new(Mutex::new(conn)), SELF).await.unwrap();
        assert_eq!(result.pending_friend_count, Some(2));
        let _ = stop.send(());
        server.await.unwrap();
    }
}
