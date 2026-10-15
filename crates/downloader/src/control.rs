use anyhow::Result;
use std::{
    fmt,
    future::Future,
    sync::{Arc, Mutex},
};
use tokio::sync::Notify;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InstallInterrupted {
    Paused,
    Cancelled,
}
impl fmt::Display for InstallInterrupted {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Paused => "Installation paused",
            Self::Cancelled => "Installation cancelled",
        })
    }
}
impl std::error::Error for InstallInterrupted {}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    Running,
    Interrupted(InstallInterrupted),
    Committing,
    Finished,
}
struct Inner {
    state: Mutex<State>,
    changed: Notify,
}
/// One install attempt. A paused attempt exits, preserving verified staging
/// chunks; resuming creates a new control and revalidates those bytes.
#[derive(Clone)]
pub struct InstallControl {
    inner: Arc<Inner>,
}
impl Default for InstallControl {
    fn default() -> Self {
        Self::new()
    }
}
impl InstallControl {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Inner {
                state: Mutex::new(State::Running),
                changed: Notify::new(),
            }),
        }
    }
    fn interrupt(&self, reason: InstallInterrupted) -> bool {
        let mut state = self.inner.state.lock().unwrap();
        match *state {
            State::Running | State::Interrupted(InstallInterrupted::Paused) => {
                *state = State::Interrupted(reason);
                self.inner.changed.notify_waiters();
                true
            }
            State::Interrupted(InstallInterrupted::Cancelled) => {
                reason == InstallInterrupted::Cancelled
            }
            State::Committing | State::Finished => false,
        }
    }
    /// False means the atomic commit has already begun and must finish.
    pub fn pause(&self) -> bool {
        self.interrupt(InstallInterrupted::Paused)
    }
    /// Cancellation retains resumable staging and never deletes user files.
    pub fn cancel(&self) -> bool {
        self.interrupt(InstallInterrupted::Cancelled)
    }
    pub fn checkpoint(&self) -> std::result::Result<(), InstallInterrupted> {
        match *self.inner.state.lock().unwrap() {
            State::Interrupted(reason) => Err(reason),
            _ => Ok(()),
        }
    }
    async fn interrupted(&self) -> InstallInterrupted {
        loop {
            let changed = self.inner.changed.notified();
            tokio::pin!(changed);
            changed.as_mut().enable();
            if let Err(reason) = self.checkpoint() {
                return reason;
            }
            changed.await;
        }
    }
    /// Interrupt only operations whose dropped future cannot keep writing files
    /// (HTTP/CM waits). Disk work instead uses `blocking`, which is always joined.
    pub(crate) async fn run<T>(&self, future: impl Future<Output = Result<T>>) -> Result<T> {
        self.checkpoint()?;
        tokio::select! {
            biased;
            reason = self.interrupted() => Err(reason.into()),
            result = future => { self.checkpoint()?; result }
        }
    }
    pub(crate) async fn blocking<T: Send + 'static>(
        &self,
        work: impl FnOnce() -> Result<T> + Send + 'static,
    ) -> Result<T> {
        self.checkpoint()?;
        let result = tokio::task::spawn_blocking(work).await?;
        self.checkpoint()?;
        result
    }
    /// Cancellation and commit share one gate. Once admitted, this short
    /// synchronous transaction is never interrupted halfway through renames.
    pub(crate) fn commit<T>(&self, work: impl FnOnce() -> Result<T>) -> Result<T> {
        {
            let mut state = self.inner.state.lock().unwrap();
            match *state {
                State::Interrupted(reason) => return Err(reason.into()),
                State::Running => *state = State::Committing,
                _ => anyhow::bail!("Installation already committed"),
            }
        }
        let result = work();
        *self.inner.state.lock().unwrap() = State::Finished;
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn pause_interrupts_a_stalled_network_wait() {
        let control = InstallControl::new();
        let child = control.clone();
        let task =
            tokio::spawn(async move { child.run(std::future::pending::<Result<()>>()).await });
        tokio::task::yield_now().await;
        assert!(control.pause());
        let error = tokio::time::timeout(std::time::Duration::from_millis(100), task)
            .await
            .unwrap()
            .unwrap()
            .unwrap_err();
        assert_eq!(
            error.downcast_ref::<InstallInterrupted>(),
            Some(&InstallInterrupted::Paused)
        );
    }
    #[test]
    fn accepted_cancel_cannot_enter_commit() {
        let control = InstallControl::new();
        assert!(control.cancel());
        let error = control
            .commit(|| -> Result<()> { panic!("cancelled install committed") })
            .unwrap_err();
        assert_eq!(
            error.downcast_ref::<InstallInterrupted>(),
            Some(&InstallInterrupted::Cancelled)
        );
    }
    #[test]
    fn commit_wins_race_without_claiming_cancel_succeeded() {
        let control = InstallControl::new();
        let other = control.clone();
        control
            .commit(|| {
                assert!(!other.cancel());
                assert!(!other.pause());
                Ok(())
            })
            .unwrap();
        assert!(!control.cancel());
    }
    #[tokio::test]
    async fn interrupted_blocking_write_is_joined_before_slot_is_released() {
        let control = InstallControl::new();
        let worker_control = control.clone();
        let finished = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let worker_finished = finished.clone();
        let task = tokio::spawn(async move {
            worker_control
                .blocking(move || {
                    std::thread::sleep(std::time::Duration::from_millis(25));
                    worker_finished.store(true, std::sync::atomic::Ordering::Release);
                    Ok(())
                })
                .await
        });
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        control.cancel();
        assert!(task.await.unwrap().is_err());
        assert!(finished.load(std::sync::atomic::Ordering::Acquire));
    }
}
