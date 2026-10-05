use std::{sync::Arc, time::{Duration, Instant}};

use anyhow::Result;
use chrono::{DateTime, Local, Utc};
use serde::Serialize;
use tokio::{sync::Mutex, time};


use crate::{config::ConfigStore, media};

#[derive(Debug, Clone, Serialize)]
pub struct RecordingFile { pub name: String, pub path: String, pub bytes: u64, pub modified: Option<DateTime<Utc>> }

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RecordingControl {
    #[default]
    Policy,
    ManualStart,
    ManualStop,
}

#[derive(Default)]
struct RecordingState {
    native: Option<media::NativeRuntime>,
    current: Option<String>,
    started: Option<Instant>,
    control: RecordingControl,
    retry_after: Option<Instant>,
}

#[derive(Default)]
pub struct RecordingManager { state: Mutex<RecordingState> }

impl RecordingManager {
    async fn start_locked(&self, state: &mut RecordingState, config: &ConfigStore) -> Result<()> {
        if state.native.as_ref().is_some_and(|runtime| !runtime.is_finished()) { return Ok(()) }
        stop_locked(state).await;
        let cfg = config.get().await;
        let directory = if cfg.recording.directory.is_absolute() { cfg.recording.directory.clone() } else { std::env::current_dir()?.join(&cfg.recording.directory) };
        tokio::fs::create_dir_all(&directory).await?;
        self.cleanup_storage(&directory, &cfg.recording).await?;
        let extension = if cfg.recording.container.eq_ignore_ascii_case("mp4") { "mp4" } else { "ts" };
        anyhow::ensure!(media::native_video_supported(&cfg), "当前录像配置不受内置媒体链路支持；请使用 MJPEG/YUYV 输入和硬件编码");
        // Keep filenames readable and unique even when two starts occur within one millisecond.
        let mut timestamp = Local::now();
        let file = loop {
            let file = directory.join(format!("{}.{}", timestamp.format("%Y%m%d-%H%M%S-%3f%z"), extension));
            if !file.exists() { break file; }
            timestamp += chrono::Duration::milliseconds(1);
        };
        let native = media::NativeRuntime::spawn(cfg, file.clone()).await?;
        state.current = Some(file.to_string_lossy().into_owned());
        state.started = Some(Instant::now());
        state.native = Some(native);
        state.retry_after = None;
        Ok(())
    }

    pub async fn update_config(&self, store: &ConfigStore, config: crate::config::AppConfig) -> Result<()> {
        let mut state = self.state.lock().await;
        let previous = store.get().await;
        let media_changed = serde_json::to_value(&previous.video)? != serde_json::to_value(&config.video)?
            || serde_json::to_value(&previous.audio)? != serde_json::to_value(&config.audio)?
            || previous.recording.directory != config.recording.directory
            || previous.recording.container != config.recording.container;
        // Persist first so a failed write leaves the current recording untouched.
        store.set(config).await?;
        if media_changed { stop_locked(&mut state).await; }
        self.reconcile_locked(&mut state, store).await;
        Ok(())
    }

    pub async fn manual_start(&self, config: &ConfigStore) -> Result<()> {
        let mut state = self.state.lock().await;
        self.start_locked(&mut state, config).await?;
        state.control = RecordingControl::ManualStart;
        Ok(())
    }

    pub async fn manual_stop(&self) {
        let mut state = self.state.lock().await;
        state.control = RecordingControl::ManualStop;
        stop_locked(&mut state).await;
    }

    pub async fn resume_policy(&self, config: &ConfigStore) {
        let mut state = self.state.lock().await;
        state.control = RecordingControl::Policy;
        state.retry_after = None;
        self.reconcile_locked(&mut state, config).await;
    }

    pub async fn stop(&self) { self.manual_stop().await; }

    pub async fn control(&self) -> RecordingControl { self.state.lock().await.control }

    pub async fn is_running(&self) -> bool {
        self.state.lock().await.native.as_ref().is_some_and(|runtime| !runtime.is_finished())
    }

    pub async fn is_current(&self, path: &str) -> bool {
        self.state.lock().await.current.as_deref() == Some(path)
    }

    async fn reconcile_locked(&self, state: &mut RecordingState, config: &ConfigStore) {
        let cfg = config.get().await;
        let desired = recording_desired(state.control, &cfg.recording, &Local::now().format("%H:%M").to_string());
        if !desired { stop_locked(state).await; return; }
        if state.native.as_ref().is_some_and(|runtime| runtime.is_finished()) {
            stop_locked(state).await;
            state.retry_after = Some(Instant::now() + Duration::from_secs(10));
        }
        let signal = cfg.video.device.as_deref().and_then(|path| crate::device::signal_present(std::path::Path::new(path)));
        let audio_missing = cfg.audio.enabled && cfg.audio.device.as_deref().is_some_and(|name| !crate::device::audio_devices().iter().any(|item| item.name == name && item.available));
        if signal == Some(false) || audio_missing { stop_locked(state).await; return; }
        let size_expired = cfg.recording.max_segment_bytes > 0 && state.current.as_ref().and_then(|path| std::fs::metadata(path).ok()).is_some_and(|metadata| metadata.len() >= cfg.recording.max_segment_bytes);
        let time_expired = state.started.is_some_and(|started| cfg.recording.segment_seconds > 0 && started.elapsed() >= Duration::from_secs(cfg.recording.segment_seconds));
        if size_expired || time_expired { stop_locked(state).await; }
        if state.native.is_none() && state.retry_after.is_none_or(|retry| Instant::now() >= retry) {
            if let Err(error) = self.start_locked(state, config).await {
                tracing::error!(%error, "recording start failed; retrying in 10 seconds");
                state.retry_after = Some(Instant::now() + Duration::from_secs(10));
            }
        }
    }

    pub async fn files(&self, config: &ConfigStore) -> Result<Vec<RecordingFile>> {
        let cfg = config.get().await;
        let directory = if cfg.recording.directory.is_absolute() { cfg.recording.directory } else { std::env::current_dir()?.join(cfg.recording.directory) };
        let mut result = Vec::new();
        let Ok(mut entries) = tokio::fs::read_dir(directory).await else { return Ok(result) };
        while let Some(entry) = entries.next_entry().await? { let metadata = entry.metadata().await?; if metadata.is_file() && is_recording_path(&entry.path()) { result.push(RecordingFile { name: entry.file_name().to_string_lossy().into_owned(), path: entry.path().to_string_lossy().into_owned(), bytes: metadata.len(), modified: metadata.modified().ok().map(DateTime::<Utc>::from) }); } }
        result.sort_by(|a, b| b.modified.cmp(&a.modified));
        Ok(result)
    }

    pub fn spawn_watchdog(self: Arc<Self>, config: ConfigStore) {
        tokio::spawn(async move {
            loop {
                self.reconcile_locked(&mut *self.state.lock().await, &config).await;
                time::sleep(Duration::from_secs(2)).await;
            }
        });
    }

    async fn cleanup_storage(&self, directory: &std::path::Path, config: &crate::config::RecordingConfig) -> Result<()> {
        if !config.loop_recording || config.min_free_bytes == 0 { return Ok(()) }
        while available_bytes(directory).is_some_and(|free| free < config.min_free_bytes) {
            let files = self.files_from_directory(directory).await?;
            let Some(file) = files.into_iter().next() else { break };
            tokio::fs::remove_file(file.path).await?;
        }
        Ok(())
    }

    async fn files_from_directory(&self, directory: &std::path::Path) -> Result<Vec<RecordingFile>> {
        let mut result = Vec::new();
        let Ok(mut entries) = tokio::fs::read_dir(directory).await else { return Ok(result) };
        while let Some(entry) = entries.next_entry().await? { let metadata = entry.metadata().await?; if metadata.is_file() && is_recording_path(&entry.path()) { result.push(RecordingFile { name: entry.file_name().to_string_lossy().into_owned(), path: entry.path().to_string_lossy().into_owned(), bytes: metadata.len(), modified: metadata.modified().ok().map(DateTime::<Utc>::from) }); } }
        result.sort_by(|a, b| a.modified.cmp(&b.modified));
        Ok(result)
    }
}

async fn stop_locked(state: &mut RecordingState) {
    if let Some(native) = state.native.take() { native.stop().await; }
    state.current = None;
    state.started = None;
}

fn recording_desired(control: RecordingControl, config: &crate::config::RecordingConfig, current: &str) -> bool {
    match control {
        RecordingControl::ManualStart => true,
        RecordingControl::ManualStop => false,
        RecordingControl::Policy if config.schedule_enabled => schedule_active(current, &config.schedule_start, &config.schedule_stop),
        RecordingControl::Policy => config.auto_start,
    }
}

fn schedule_active(current: &str, start: &str, stop: &str) -> bool {
    if start == stop { return false; }
    if start < stop { current >= start && current < stop } else { current >= start || current < stop }
}

fn is_recording_path(path: &std::path::Path) -> bool { matches!(path.extension().and_then(|extension| extension.to_str()), Some("ts" | "mp4")) }

fn available_bytes(path: &std::path::Path) -> Option<u64> {
    let path = std::ffi::CString::new(path.to_string_lossy().as_bytes()).ok()?;
    let mut stats = std::mem::MaybeUninit::<libc::statvfs>::uninit();
    let result = unsafe { libc::statvfs(path.as_ptr(), stats.as_mut_ptr()) };
    if result != 0 { return None; }
    let stats = unsafe { stats.assume_init() };
    Some(stats.f_bavail as u64 * stats.f_frsize as u64)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::RecordingConfig;

    #[test]
    fn schedule_takes_precedence_over_auto_start() {
        let cfg = RecordingConfig { auto_start: true, schedule_enabled: true, schedule_start: "08:00".into(), schedule_stop: "18:00".into(), ..Default::default() };
        assert!(!recording_desired(RecordingControl::Policy, &cfg, "20:00"));
        assert!(recording_desired(RecordingControl::Policy, &cfg, "08:00"));
        assert!(!recording_desired(RecordingControl::Policy, &cfg, "18:00"));
        assert!(recording_desired(RecordingControl::ManualStart, &cfg, "20:00"));
        assert!(!recording_desired(RecordingControl::ManualStop, &cfg, "12:00"));
    }

    #[test]
    fn schedule_handles_midnight_and_equal_endpoints() {
        assert!(schedule_active("23:00", "22:00", "06:00"));
        assert!(schedule_active("02:00", "22:00", "06:00"));
        assert!(!schedule_active("06:00", "22:00", "06:00"));
        assert!(!schedule_active("12:00", "08:00", "08:00"));
    }

    #[test]
    fn manual_stop_survives_watchdog_with_auto_start() {
        let cfg = RecordingConfig { auto_start: true, ..Default::default() };
        for _ in 0..30 { assert!(!recording_desired(RecordingControl::ManualStop, &cfg, "12:00")); }
        assert!(recording_desired(RecordingControl::Policy, &cfg, "12:00"));
    }
}
