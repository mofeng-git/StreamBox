use std::sync::Arc;

use anyhow::Result;
use serde::Serialize;
use tokio::sync::{broadcast, Mutex};

use crate::{config::ConfigStore, media, onvif, rtsp};

#[derive(Debug, Clone, Serialize, Default)]
pub struct OutputStatus {
    pub rtsp: bool,
    pub rtmp: bool,
    pub rtmp_connected: bool,
    pub onvif: bool,
    pub last_error: Option<String>,
}

#[derive(Default)]
pub struct OutputManager { native_video: Mutex<Option<media::NativeRuntime>>, native_audio: Mutex<Option<media::NativeAudioRuntime>>, tasks: Mutex<Vec<tokio::task::JoinHandle<()>>>, status: Arc<Mutex<OutputStatus>>, config: Mutex<Option<ConfigStore>> }

impl OutputManager {
    pub async fn start(&self, config: &ConfigStore) -> Result<OutputStatus> {
        self.stop().await;
        *self.config.lock().await = Some(config.clone());
        let cfg = config.get().await;
        let mut native_video = None;
        let mut native_audio = None;
        let mut tasks = Vec::new();
        let mut status = OutputStatus::default();
        if cfg.streaming.rtsp.enabled && media::native_video_supported(&cfg) {
            let audio_receiver = if cfg.audio.enabled && cfg.audio.device.is_some() {
                let (audio_sender, audio_receiver) = broadcast::channel(64);
                match media::NativeAudioRuntime::spawn(cfg.clone(), audio_sender) {
                    Ok(runtime) => { native_audio = Some(runtime); Some(audio_receiver) }
                    Err(error) => { status.last_error = Some(format!("RTSP audio: {error}")); None }
                }
            } else { None };
            let has_audio = audio_receiver.is_some();
            let (audio_rate, audio_channels) = if has_audio { cfg.audio.output_params() } else { (0, 0) };
            match rtsp::start_native(cfg.streaming.rtsp.clone(), cfg.clone(), audio_receiver, cfg.audio.codec.clone(), audio_rate, audio_channels).await {
                Ok((_hub, runtime, mut handles)) => {
                    native_video = Some(runtime);
                    tasks.append(&mut handles);
                    status.rtsp = true;
                }
                Err(error) => {
                    status.last_error = Some(format!("RTSP native: {error}"));
                }
            }
        } else if cfg.streaming.rtsp.enabled {
            status.last_error = Some("RTSP requires the native video path; current input configuration is unsupported".into());
        }
        if cfg.streaming.rtmp.enabled {
            if let Some(target) = cfg.streaming.rtmp.url.as_deref() {
                let config = cfg.clone();
                let target = target.to_owned();
                let delay = cfg.streaming.rtmp.reconnect_seconds.max(1);
                let status_handle = self.status.clone();
                tasks.push(tokio::spawn(async move { rtmp_loop(config, target, delay, status_handle).await; }));
                status.rtmp = true;
            } else {
                status.last_error = Some("RTMP URL is not configured".into());
            }
        }
        status.onvif = cfg.streaming.onvif.enabled;
        if cfg.streaming.onvif.enabled {
            let rtsp_uri = format!("rtsp://{}:{}{}", cfg.streaming.rtsp.bind, cfg.streaming.rtsp.port, cfg.streaming.rtsp.path);
            match onvif::start(&cfg.streaming.onvif, rtsp_uri).await {
                Ok(mut handles) => tasks.append(&mut handles),
                Err(error) => { status.onvif = false; status.last_error = Some(format!("ONVIF: {error}")); }
            }
        }
        *self.native_video.lock().await = native_video;
        *self.native_audio.lock().await = native_audio;
        *self.tasks.lock().await = tasks;
        *self.status.lock().await = status.clone();
        Ok(status)
    }

    pub async fn stop(&self) {
        let tasks = std::mem::take(&mut *self.tasks.lock().await);
        for task in tasks { task.abort(); }
        if let Some(native) = self.native_video.lock().await.take() { native.stop().await; }
        if let Some(native) = self.native_audio.lock().await.take() { native.stop().await; }
        *self.status.lock().await = OutputStatus::default();
    }

    pub async fn status(&self) -> OutputStatus { self.status.lock().await.clone() }

    pub fn spawn_watchdog(self: Arc<Self>) {
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(std::time::Duration::from_secs(2)).await;
                let mut changed = false;
                let native_finished = self.native_video.lock().await.as_ref().is_some_and(|runtime| runtime.is_finished());
                if native_finished { *self.native_video.lock().await = None; changed = true; }
                let audio_finished = self.native_audio.lock().await.as_ref().is_some_and(|runtime| runtime.is_finished());
                if audio_finished { *self.native_audio.lock().await = None; changed = true; }
                let should_restart = changed && self.status.lock().await.rtsp;
                if changed { *self.status.lock().await = OutputStatus::default(); }
                if should_restart {
                    if let Some(config) = self.config.lock().await.clone() {
                        if let Err(error) = self.start(&config).await { tracing::warn!(error = %error, "RTSP output restart failed"); }
                    }
                }
            }
        });
    }
}

async fn rtmp_loop(config: crate::config::AppConfig, target: String, delay_seconds: u64, status: Arc<Mutex<OutputStatus>>) {
    loop {
        if !media::native_video_supported(&config) {
            status.lock().await.last_error = Some("RTMP requires the native video path; current input configuration is unsupported".into());
            tokio::time::sleep(std::time::Duration::from_secs(delay_seconds)).await;
            continue;
        }
        let (video_sender, video_receiver) = broadcast::channel(64);
        let video_runtime = media::spawn_native_video(config.clone(), video_sender);
        let mut audio_runtime = None;
        let audio_receiver = if config.audio.enabled && config.audio.device.is_some() {
            let (audio_sender, audio_receiver) = broadcast::channel(64);
            match media::NativeAudioRuntime::spawn(config.clone(), audio_sender) {
                Ok(runtime) => { audio_runtime = Some(runtime); Some(audio_receiver) }
                Err(error) => { status.lock().await.last_error = Some(format!("RTMP audio: {error}")); None }
            }
        } else { None };
        match media::NativeMuxRuntime::spawn(config.clone(), target.clone(), "flv".into(), video_receiver, audio_receiver) {
            Ok(runtime) => {
                status.lock().await.rtmp_connected = true;
                if let Err(error) = runtime.wait().await { status.lock().await.last_error = Some(format!("RTMP: {error}")); }
            }
            Err(error) => { status.lock().await.last_error = Some(format!("RTMP: {error}")); }
        }
        video_runtime.stop().await;
        if let Some(runtime) = audio_runtime { runtime.stop().await; }
        status.lock().await.rtmp_connected = false;
        tokio::time::sleep(std::time::Duration::from_secs(delay_seconds)).await;
    }
}
