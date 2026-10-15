use crate::{InstallProgress, ProgressCallback};
use std::{
    collections::VecDeque,
    time::{Duration, Instant},
};

/// Counts response-body bytes, never reused or uncompressed bytes. Denominator
/// starts from chunk metadata and is corrected against each actual response.
pub(crate) struct TransferProgress {
    pub downloaded: u64,
    pub total: u64,
    pub reused: u64,
    pub processed: u64,
    pub install_total: u64,
    pub files_completed: u64,
    pub total_files: u64,
    pub chunks_completed: u64,
    pub total_chunks: u64,
    appid: u32,
    callback: ProgressCallback,
    samples: VecDeque<(Instant, u64)>,
    last_emit: Instant,
}
impl TransferProgress {
    pub fn new(
        appid: u32,
        total: u64,
        reused: u64,
        install_total: u64,
        callback: ProgressCallback,
    ) -> Self {
        let now = Instant::now();
        Self {
            downloaded: 0,
            total,
            reused,
            processed: 0,
            install_total,
            files_completed: 0,
            total_files: 0,
            chunks_completed: 0,
            total_chunks: 0,
            appid,
            callback,
            samples: VecDeque::from([(now, 0)]),
            last_emit: now,
        }
    }
    pub fn receive(&mut self, bytes: usize, message: &str) {
        self.downloaded = self.downloaded.saturating_add(bytes as u64);
        let now = Instant::now();
        self.samples.push_back((now, self.downloaded));
        while self.samples.len() > 2
            && self
                .samples
                .front()
                .is_some_and(|(t, _)| now.duration_since(*t) > Duration::from_secs(5))
        {
            self.samples.pop_front();
        }
        if now.duration_since(self.last_emit) >= Duration::from_millis(150) {
            self.emit("downloading", message, false);
        }
    }
    pub fn complete_chunk(&mut self, expected_wire: u64, actual: u64) {
        self.total = self
            .total
            .saturating_sub(expected_wire)
            .saturating_add(actual)
            .max(self.downloaded);
    }
    pub fn emit_verification(&mut self, processed: u64, total: u64, message: &str) {
        let original = (self.processed, self.install_total);
        self.processed = processed;
        self.install_total = total;
        self.emit("verifying", message, false);
        (self.processed, self.install_total) = original;
    }
    pub fn emit(&mut self, phase: &'static str, message: &str, complete: bool) {
        let now = Instant::now();
        let rate = self
            .samples
            .front()
            .map(|(t, n)| {
                let elapsed = now.duration_since(*t).as_secs_f64();
                if elapsed >= 0.1 {
                    (self.downloaded.saturating_sub(*n)) as f64 / elapsed
                } else {
                    0.0
                }
            })
            .unwrap_or(0.0);
        let total = self.total.max(self.downloaded);
        let remaining = total.saturating_sub(self.downloaded);
        let progress = if complete {
            Some(1.0)
        } else if phase == "verifying" && self.install_total > 0 {
            Some((self.processed as f64 / self.install_total as f64).min(1.0))
        } else if total > 0 {
            Some((self.downloaded as f64 / total as f64).min(1.0))
        } else {
            None
        };
        (self.callback)(InstallProgress {
            appid: self.appid,
            phase,
            downloaded_bytes: self.downloaded,
            total_bytes: total,
            progress,
            bytes_per_second: if complete || phase != "downloading" {
                0.0
            } else {
                rate
            },
            eta_seconds: if complete {
                Some(0)
            } else if phase == "downloading" && rate > 0.0 {
                Some((remaining as f64 / rate).ceil() as u64)
            } else {
                None
            },
            reused_bytes: self.reused,
            processed_bytes: self.processed,
            install_total_bytes: self.install_total,
            files_completed: self.files_completed,
            total_files: self.total_files,
            chunks_completed: self.chunks_completed,
            total_chunks: self.total_chunks,
            message: message.into(),
        });
        self.last_emit = now;
    }
}

/// Reports checked manifest coverage independently of network byte counters.
/// New/sparse files can be classified missing without hashing their zero holes.
pub(crate) struct VerificationProgress {
    appid: u32,
    callback: ProgressCallback,
    last_emit: Instant,
    pub processed: u64,
    pub total: u64,
    pub files: u64,
    pub total_files: u64,
    pub chunks: u64,
    pub total_chunks: u64,
    pub reused: u64,
}
impl VerificationProgress {
    pub fn new(
        appid: u32,
        total: u64,
        total_files: u64,
        total_chunks: u64,
        callback: ProgressCallback,
    ) -> Self {
        Self {
            appid,
            callback,
            last_emit: Instant::now(),
            processed: 0,
            total,
            files: 0,
            total_files,
            chunks: 0,
            total_chunks,
            reused: 0,
        }
    }
    pub fn emit(&mut self, message: &str, force: bool) {
        if !force && self.last_emit.elapsed() < Duration::from_millis(150) {
            return;
        }
        (self.callback)(InstallProgress {
            appid: self.appid,
            phase: "verifying",
            downloaded_bytes: 0,
            total_bytes: 0,
            progress: if self.total > 0 {
                Some((self.processed as f64 / self.total as f64).min(1.0))
            } else {
                None
            },
            message: message.into(),
            bytes_per_second: 0.0,
            eta_seconds: None,
            reused_bytes: self.reused,
            processed_bytes: self.processed,
            install_total_bytes: self.total,
            files_completed: self.files,
            total_files: self.total_files,
            chunks_completed: self.chunks,
            total_chunks: self.total_chunks,
        });
        self.last_emit = Instant::now();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn checksum_progress_reports_checked_bytes_instead_of_finished_network_bytes() {
        let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let sink = seen.clone();
        let mut meter = TransferProgress::new(
            42,
            100,
            0,
            1000,
            std::sync::Arc::new(move |p| sink.lock().unwrap().push(p)),
        );
        meter.downloaded = 100;
        meter.processed = 1000;
        meter.emit_verification(25, 100, "Проверка SHA1");
        let events = seen.lock().unwrap();
        let event = events.last().unwrap();
        assert_eq!(
            (
                event.progress,
                event.processed_bytes,
                event.install_total_bytes
            ),
            (Some(0.25), 25, 100)
        );
        assert_eq!((event.bytes_per_second, event.eta_seconds), (0.0, None));
        assert_eq!((meter.processed, meter.install_total), (1000, 1000));
    }
    #[test]
    fn counts_wire_bytes_and_corrects_retries_without_counting_reuse() {
        let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let sink = seen.clone();
        let mut meter = TransferProgress::new(
            42,
            100,
            900,
            1000,
            std::sync::Arc::new(move |p| sink.lock().unwrap().push(p)),
        );
        meter.receive(30, "download"); // interrupted first attempt
        meter.receive(100, "download"); // complete retry
        meter.complete_chunk(100, 130);
        meter.emit("complete", "done", true);
        let events = seen.lock().unwrap();
        let p = events.last().unwrap();
        assert_eq!(
            (p.downloaded_bytes, p.total_bytes, p.reused_bytes),
            (130, 130, 900)
        );
        assert_eq!((p.progress, p.eta_seconds), (Some(1.0), Some(0)));
    }
}
