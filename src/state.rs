use std::{collections::{HashMap, VecDeque}, path::PathBuf};
use std::sync::Arc;

use anyhow::Result;
use chrono::{DateTime, Utc};
use serde::Serialize;
use tokio::sync::{broadcast, RwLock};

use crate::{config::ConfigStore, device, outputs::{OutputManager, OutputStatus}, recording::RecordingManager};

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PipelineStatus { Idle, Running, DeviceLost, Error }

#[derive(Debug, Clone, Serialize)]
pub struct RuntimeStatus {
    pub status: PipelineStatus,
    pub recording: bool,
    pub recording_control: crate::recording::RecordingControl,
    pub selected_video: Option<String>,
    pub signal_present: Option<bool>,
    pub selected_audio: Option<String>,
    pub audio_present: Option<bool>,
    pub last_error: Option<String>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize)]
pub struct LogRecord {
    pub timestamp: DateTime<Utc>,
    pub level: String,
    pub message: String,
}

#[derive(Clone)]
pub struct ChannelRuntime {
    pub name: String,
    pub config: ConfigStore,
    pub recordings: Arc<RecordingManager>,
    pub outputs: Arc<OutputManager>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ChannelStatus {
    pub id: String,
    pub name: String,
    pub enabled: bool,
    pub video_device: Option<String>,
    pub recording: bool,
    pub outputs: OutputStatus,
}

impl Default for RuntimeStatus {
    fn default() -> Self { Self { status: PipelineStatus::Idle, recording: false, recording_control: Default::default(), selected_video: None, signal_present: None, selected_audio: None, audio_present: None, last_error: None, updated_at: Utc::now() } }
}

pub struct AppState {
    pub data_dir: PathBuf,
    pub config: ConfigStore,
    pub runtime: RwLock<RuntimeStatus>,
    pub events: broadcast::Sender<RuntimeStatus>,
    pub logs: Arc<RwLock<VecDeque<LogRecord>>>,
    pub channels: Arc<RwLock<HashMap<String, ChannelRuntime>>>,
    pub recordings: Arc<RecordingManager>,
    pub outputs: Arc<OutputManager>,
}

impl AppState {
    pub async fn new(config: ConfigStore, data_dir: PathBuf) -> Result<Self> {
        let (events, _) = broadcast::channel(32);
        let app_config = config.get().await;
        let mut channels = HashMap::new();
        for channel in &app_config.channels {
            if channel.enabled { channels.insert(channel.id.clone(), ChannelRuntime { name: channel.name.clone(), config: ConfigStore::in_memory(channel.as_app_config()), recordings: Arc::new(RecordingManager::default()), outputs: Arc::new(OutputManager::default()) }); }
        }
        let _ = data_dir;
        Ok(Self { data_dir, config, runtime: RwLock::new(RuntimeStatus::default()), events, logs: Arc::new(RwLock::new(VecDeque::with_capacity(256))), channels: Arc::new(RwLock::new(channels)), recordings: Arc::new(RecordingManager::default()), outputs: Arc::new(OutputManager::default()) })
    }

    pub fn start_background_tasks(self: &Arc<Self>) {
        self.recordings.clone().spawn_watchdog(self.config.clone());
        self.outputs.clone().spawn_watchdog();
        let channel_runtimes = self.channels.clone();
        tokio::spawn(async move {
            let runtimes = channel_runtimes.read().await.values().cloned().collect::<Vec<_>>();
            for channel in runtimes {
                start_channel_background(channel);
            }
        });
        let state = self.clone();
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                let config = state.config.get().await;
                let selected = config.video.device.clone().or_else(|| device::video_devices().into_iter().find(|item| item.likely_capture).map(|item| item.path.to_string_lossy().into_owned()));
                let signal = selected.as_deref().and_then(|path| device::signal_present(std::path::Path::new(path)));
                let audio = config.audio.device.clone();
                let audio_signal = audio.as_deref().map(|name| device::audio_devices().iter().any(|item| item.name == name && item.available));
                state.set_runtime(|runtime| {
                    runtime.selected_video = selected.clone();
                    runtime.signal_present = signal;
                    runtime.selected_audio = audio.clone();
                    runtime.audio_present = audio_signal;
                    if signal == Some(false) && runtime.recording { runtime.status = PipelineStatus::DeviceLost; }
                    if audio_signal == Some(false) && runtime.recording { runtime.status = PipelineStatus::DeviceLost; runtime.last_error = Some("音频设备不可用".into()); }
                    if signal == Some(true) && runtime.recording && matches!(runtime.status, PipelineStatus::DeviceLost) { runtime.status = PipelineStatus::Running; }
                }).await;
                let running = state.recordings.is_running().await;
                state.set_runtime(|runtime| {
                    runtime.recording = running;
                    if running { runtime.status = PipelineStatus::Running; }
                    else if matches!(runtime.status, PipelineStatus::Running) { runtime.status = PipelineStatus::Idle; }
                }).await;
                let _ = state.events.send(state.snapshot().await);
            }
        });
    }

    pub async fn sync_channels(self: &Arc<Self>) {
        let configured = self.config.get().await.channels;
        let mut added = Vec::new();
        let mut removed = Vec::new();
        {
            let mut channels = self.channels.write().await;
            let configured_ids = configured.iter().filter(|channel| channel.enabled).map(|channel| channel.id.clone()).collect::<std::collections::HashSet<_>>();
            let existing_ids = channels.keys().cloned().collect::<Vec<_>>();
            for id in existing_ids {
                if !configured_ids.contains(&id) {
                    if let Some(channel) = channels.remove(&id) { removed.push(channel); }
                }
            }
            for channel in configured.into_iter().filter(|channel| channel.enabled) {
                let app_config = channel.as_app_config();
                if let Some(runtime) = channels.get_mut(&channel.id) {
                    runtime.name = channel.name;
                    runtime.config.update_memory(app_config).await;
                } else {
                    let runtime = ChannelRuntime { name: channel.name, config: ConfigStore::in_memory(app_config), recordings: Arc::new(RecordingManager::default()), outputs: Arc::new(OutputManager::default()) };
                    added.push(runtime.clone());
                    channels.insert(channel.id, runtime);
                }
            }
        }
        for channel in removed {
            channel.recordings.stop().await;
            channel.outputs.stop().await;
        }
        for channel in added { start_channel_background(channel); }
    }

    pub async fn snapshot(&self) -> RuntimeStatus {
        let mut status = self.runtime.read().await.clone();
        status.recording = self.recordings.is_running().await;
        status.recording_control = self.recordings.control().await;
        status.updated_at = Utc::now();
        status
    }

    pub async fn log(&self, level: impl Into<String>, message: impl Into<String>) {
        let mut logs = self.logs.write().await;
        if logs.len() >= 256 { logs.pop_front(); }
        logs.push_back(LogRecord { timestamp: Utc::now(), level: level.into(), message: message.into() });
    }

    pub async fn log_snapshot(&self) -> Vec<LogRecord> { self.logs.read().await.iter().cloned().collect() }

    pub async fn channel_statuses(&self) -> Vec<ChannelStatus> {
        let entries = self.channels.read().await.iter().map(|(id, channel)| (id.clone(), channel.clone())).collect::<Vec<_>>();
        let mut statuses = Vec::with_capacity(entries.len());
        for (id, channel) in entries {
            let config = channel.config.get().await;
            statuses.push(ChannelStatus { id, name: channel.name, enabled: true, video_device: config.video.device, recording: channel.recordings.is_running().await, outputs: channel.outputs.status().await });
        }
        statuses
    }

    pub async fn channel(&self, id: &str) -> Option<ChannelRuntime> { self.channels.read().await.get(id).cloned() }

    pub async fn set_runtime(&self, update: impl FnOnce(&mut RuntimeStatus)) {
        let control = self.recordings.control().await;
        let mut status = self.runtime.write().await;
        update(&mut status);
        status.recording_control = control;
        status.updated_at = Utc::now();
        let _ = self.events.send(status.clone());
    }

    pub async fn shutdown(&self) { self.recordings.stop().await; self.outputs.stop().await; }
}

fn start_channel_background(channel: ChannelRuntime) {
    channel.recordings.clone().spawn_watchdog(channel.config.clone());
    channel.outputs.clone().spawn_watchdog();
}

pub type SharedState = Arc<AppState>;
