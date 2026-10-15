//! Transport recovery is separate from the lifetime of an authenticated account's games.
use crate::session::Account;
use std::time::Duration;
use steam_cm_protocol::{AuthMethod, error::Error};
use tokio::time::Instant;
use zeroize::Zeroizing;

pub const RETRY_DELAY: Duration = Duration::from_secs(10);

pub struct SessionFailure {
    pub message: String,
    pub retryable: bool,
}
impl SessionFailure {
    pub fn transport(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            retryable: true,
        }
    }
    pub fn from_protocol(error: Error) -> Self {
        let retryable = match &error {
            Error::Closed | Error::Io(_) | Error::Transport(_) | Error::WebSocket(_) => true,
            Error::Http(error) => error
                .status()
                .is_none_or(|status| status.is_server_error() || status.as_u16() == 429),
            // These are server availability responses, not rejected credentials.
            Error::Authentication(message) => [3, 20, 48, 84].iter().any(|code| {
                message.ends_with(&format!("eresult {code}"))
                    || message.ends_with(&format!("result {code})"))
            }),
            _ => false,
        };
        Self {
            message: error.to_string(),
            retryable,
        }
    }
}

#[derive(Default)]
pub struct SessionEpochs {
    pub account: u64,
    pub transport: u64,
}
impl SessionEpochs {
    pub fn new_account(&mut self) {
        self.account += 1;
        self.transport += 1;
    }
    pub fn new_transport(&mut self) {
        self.transport += 1;
    }
    pub fn accepts(&self, epoch: u64, operation: bool) -> bool {
        epoch
            == if operation {
                self.account
            } else {
                self.transport
            }
    }
}

#[derive(Default)]
pub struct Reconnect {
    account: Option<Account>,
    token: Option<Zeroizing<String>>,
    pub deadline: Option<Instant>,
}
impl Reconnect {
    pub fn remember(&mut self, account: Account, token: &str) {
        self.account = Some(account);
        self.token = Some(Zeroizing::new(token.to_owned()));
        self.deadline = None;
    }
    pub fn account(&self) -> Option<&Account> {
        self.account.as_ref()
    }
    pub fn cancel(&mut self) {
        self.deadline = None;
        self.token = None;
        self.account = None;
    }
    pub fn schedule(&mut self, failure: &SessionFailure) -> bool {
        self.deadline =
            (failure.retryable && self.token.is_some()).then(|| Instant::now() + RETRY_DELAY);
        self.deadline.is_some()
    }
    pub fn retry_now(&mut self) -> bool {
        self.deadline = self.token.as_ref().map(|_| Instant::now());
        self.deadline.is_some()
    }
    pub fn begin_attempt(&mut self) -> Option<(AuthMethod, String)> {
        self.deadline = None;
        Some((
            AuthMethod::RefreshToken(self.token.as_ref()?.to_string()),
            self.account.as_ref()?.account_name.clone(),
        ))
    }
    pub fn remaining_seconds(&self) -> Option<u64> {
        self.deadline.map(|deadline| {
            deadline
                .saturating_duration_since(Instant::now())
                .as_secs_f64()
                .ceil() as u64
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn target() -> Account {
        Account {
            steamid: "76561198000000000".into(),
            account_name: "fixture".into(),
            last_used: 1,
        }
    }
    #[tokio::test(start_paused = true)]
    async fn unexpected_eof_retries_after_ten_seconds_and_not_earlier() {
        let mut retry = Reconnect::default();
        retry.remember(target(), "test-only-token");
        assert!(retry.schedule(&SessionFailure::from_protocol(Error::Io(
            std::io::ErrorKind::UnexpectedEof.into()
        ))));
        assert_eq!(retry.remaining_seconds(), Some(10));
        tokio::time::advance(Duration::from_secs(9)).await;
        assert_eq!(retry.remaining_seconds(), Some(1));
        tokio::time::advance(Duration::from_secs(1)).await;
        assert_eq!(retry.remaining_seconds(), Some(0));
        assert!(retry.begin_attempt().is_some());
        assert!(retry.deadline.is_none());
        retry.schedule(&SessionFailure::transport("next connection failed"));
        assert_eq!(retry.remaining_seconds(), Some(10));
    }
    #[tokio::test(start_paused = true)]
    async fn cancellation_and_rejected_credentials_never_retry() {
        let mut retry = Reconnect::default();
        retry.remember(target(), "test-only-token");
        assert!(
            !retry.schedule(&SessionFailure::from_protocol(Error::Authentication(
                "ClientLogOn failed with eresult 5".into()
            )))
        );
        retry.cancel();
        assert!(!retry.retry_now());
        assert!(!retry.schedule(&SessionFailure::transport("late failure")));
        tokio::time::advance(Duration::from_secs(100)).await;
        assert!(retry.begin_attempt().is_none());
    }
    #[test]
    fn connection_epoch_changes_preserve_game_events_but_reject_stale_workers() {
        let mut epochs = SessionEpochs::default();
        epochs.new_account();
        let game = epochs.account;
        let worker = epochs.transport;
        epochs.new_transport();
        assert!(epochs.accepts(game, true));
        assert!(!epochs.accepts(worker, false));
        epochs.new_account();
        assert!(!epochs.accepts(game, true));
    }
}
