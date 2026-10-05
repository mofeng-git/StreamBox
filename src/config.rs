use std::{path::PathBuf, sync::Arc};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct AppConfig {
    pub video: VideoConfig,
    pub audio: AudioConfig,
    pub recording: RecordingConfig,
    pub streaming: StreamingConfig,
    pub security: SecurityConfig,
    pub channels: Vec<InputChannelConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct InputChannelConfig {
    pub id: String,
    pub name: String,
    pub enabled: bool,
    pub video: VideoConfig,
    pub audio: AudioConfig,
    pub recording: RecordingConfig,
    pub streaming: StreamingConfig,
}

impl InputChannelConfig {
    pub fn as_app_config(&self) -> AppConfig { AppConfig { video: self.video.clone(), audio: self.audio.clone(), recording: self.recording.clone(), streaming: self.streaming.clone(), security: SecurityConfig::default(), channels: Vec::new() } }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct SecurityConfig {
    pub enabled: bool,
    pub username: String,
    pub password: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct VideoConfig {
    pub device: Option<String>,
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    pub input_format: String,
    pub codec: VideoCodec,
    pub encoder: EncoderMode,
    pub bitrate_kbps: u32,
    pub profile: Option<String>,
    pub level: Option<String>,
    pub preset: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum VideoCodec { H264, H265 }

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum EncoderMode { Auto, Hardware, V4l2m2m, Rkmpp, Software }

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct AudioConfig {
    pub enabled: bool,
    pub device: Option<String>,
    pub codec: AudioCodec,
    pub sample_rate: u32,
    pub channels: u32,
    pub volume: u8,
    pub input_format: String,
    pub bitrate_kbps: u32,
}

impl AudioConfig {
    pub fn output_params(&self) -> (u32, u32) {
        match self.codec { AudioCodec::Aac => (self.sample_rate, self.channels), AudioCodec::G711Alaw | AudioCodec::G711Ulaw => (8000, 1) }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AudioCodec { Aac, G711Alaw, G711Ulaw }

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct RecordingConfig {
    pub directory: PathBuf,
    pub container: String,
    pub auto_start: bool,
    pub segment_seconds: u64,
    pub max_segment_bytes: u64,
    pub loop_recording: bool,
    pub min_free_bytes: u64,
    pub schedule_enabled: bool,
    pub schedule_start: String,
    pub schedule_stop: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct StreamingConfig {
    pub rtsp: RtspConfig,
    pub rtmp: RtmpConfig,
    pub onvif: OnvifConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct RtspConfig {
    pub enabled: bool,
    pub bind: String,
    pub port: u16,
    pub path: String,
    pub username: Option<String>,
    pub password: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct RtmpConfig {
    pub enabled: bool,
    pub url: Option<String>,
    pub reconnect_seconds: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct OnvifConfig {
    pub enabled: bool,
    pub bind: String,
    pub port: u16,
    pub name: String,
    pub username: Option<String>,
    pub password: Option<String>,
}

impl Default for AppConfig {
    fn default() -> Self { Self { video: VideoConfig::default(), audio: AudioConfig::default(), recording: RecordingConfig::default(), streaming: StreamingConfig::default(), security: SecurityConfig::default(), channels: Vec::new() } }
}
impl Default for InputChannelConfig {
    fn default() -> Self { Self { id: "channel-1".into(), name: "输入通道 1".into(), enabled: true, video: VideoConfig::default(), audio: AudioConfig::default(), recording: RecordingConfig::default(), streaming: StreamingConfig::default() } }
}
impl Default for SecurityConfig {
    fn default() -> Self { Self { enabled: true, username: "admin".into(), password: "admin".into() } }
}
impl Default for VideoConfig {
    fn default() -> Self { Self { device: None, width: 1920, height: 1080, fps: 30, input_format: "mjpeg".into(), codec: VideoCodec::H264, encoder: EncoderMode::Auto, bitrate_kbps: 4000, profile: None, level: None, preset: None } }
}
impl Default for AudioConfig {
    fn default() -> Self { Self { enabled: true, device: None, codec: AudioCodec::Aac, sample_rate: 48000, channels: 2, volume: 100, input_format: "S16_LE".into(), bitrate_kbps: 128 } }
}
impl Default for RecordingConfig {
    fn default() -> Self { Self { directory: PathBuf::from("recordings"), container: "mpegts".into(), auto_start: false, segment_seconds: 600, max_segment_bytes: 0, loop_recording: true, min_free_bytes: 256 * 1024 * 1024, schedule_enabled: false, schedule_start: "08:00".into(), schedule_stop: "18:00".into() } }
}
impl Default for StreamingConfig {
    fn default() -> Self { Self { rtsp: RtspConfig::default(), rtmp: RtmpConfig::default(), onvif: OnvifConfig::default() } }
}
impl Default for RtspConfig {
    fn default() -> Self { Self { enabled: false, bind: "0.0.0.0".into(), port: 8554, path: "/camera/main".into(), username: None, password: None } }
}
impl Default for RtmpConfig {
    fn default() -> Self { Self { enabled: false, url: None, reconnect_seconds: 5 } }
}
impl Default for OnvifConfig {
    fn default() -> Self { Self { enabled: false, bind: "0.0.0.0".into(), port: 8000, name: "StreamBox".into(), username: None, password: None } }
}
#[derive(Clone)]
pub struct ConfigStore { path: PathBuf, value: Arc<RwLock<AppConfig>> }

impl ConfigStore {
    pub fn in_memory(value: AppConfig) -> Self { Self { path: PathBuf::new(), value: Arc::new(RwLock::new(value)) } }

    pub async fn load(path: PathBuf) -> Result<Self> {
        let mut value = match tokio::fs::read_to_string(&path).await {
            Ok(contents) => toml::from_str(&contents).context("parse config.toml")?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => AppConfig::default(),
            Err(error) => return Err(error).with_context(|| format!("read {}", path.display())),
        };
        if value.security.username.is_empty() { value.security.username = "admin".into(); }
        if value.security.password.is_empty() { value.security.password = "admin".into(); }
        value.security.enabled = true;
        let store = Self { path, value: Arc::new(RwLock::new(value)) };
        store.save().await?;
        Ok(store)
    }

    pub async fn get(&self) -> AppConfig { self.value.read().await.clone() }

    pub async fn set(&self, value: AppConfig) -> Result<()> {
        *self.value.write().await = value;
        self.save().await
    }

    pub async fn update_memory(&self, value: AppConfig) {
        *self.value.write().await = value;
    }

    async fn save(&self) -> Result<()> {
        if let Some(parent) = self.path.parent() { tokio::fs::create_dir_all(parent).await?; }
        let value = self.value.read().await.clone();
        let text = toml::to_string_pretty(&value)?;
        let temp = self.path.with_extension("toml.tmp");
        tokio::fs::write(&temp, text).await?;
        tokio::fs::rename(temp, &self.path).await?;
        Ok(())
    }
}

impl AppConfig {
    pub fn validate(&self) -> Result<()> {
        anyhow::ensure!(self.video.width % 2 == 0 && self.video.height % 2 == 0 && self.video.width <= 8192 && self.video.height <= 8192, "video dimensions must be even and at most 8192");
        anyhow::ensure!(self.video.preset.as_deref().is_none_or(|value| matches!(value, "ultrafast" | "veryfast" | "medium" | "slow")), "unsupported encoder preset");
        anyhow::ensure!(self.video.profile.as_deref().is_none_or(|value| matches!(value, "baseline" | "main" | "high")), "unsupported encoder profile");
        anyhow::ensure!(self.video.level.as_deref().is_none_or(|value| matches!(value, "3.1" | "4.0" | "4.1" | "5.0")), "unsupported encoder level");
        anyhow::ensure!(self.video.width > 0 && self.video.height > 0, "video dimensions must be greater than zero");
        anyhow::ensure!(self.video.fps > 0 && self.video.fps <= 240, "video fps must be between 1 and 240");
        anyhow::ensure!(self.video.bitrate_kbps > 0, "video bitrate must be greater than zero");
        anyhow::ensure!(self.audio.sample_rate > 0 && self.audio.channels > 0 && self.audio.channels <= 8, "audio sample rate/channels are invalid");
        anyhow::ensure!(matches!(self.audio.input_format.as_str(), "S16_LE" | "S24_LE" | "S24_3LE" | "S32_LE" | "FLOAT_LE" | "U8"), "unsupported audio input format");
        anyhow::ensure!(self.audio.bitrate_kbps > 0 && self.audio.bitrate_kbps <= 1024, "audio bitrate must be between 1 and 1024 kbps");
        anyhow::ensure!(self.audio.volume <= 200, "audio volume must be between 0 and 200");
        anyhow::ensure!(self.recording.container.eq_ignore_ascii_case("mpegts") || self.recording.container.eq_ignore_ascii_case("mp4"), "recording container must be mpegts or mp4");
        anyhow::ensure!(!(self.recording.container.eq_ignore_ascii_case("mp4") && self.audio.enabled && self.audio.device.is_some() && matches!(self.audio.codec, AudioCodec::G711Alaw | AudioCodec::G711Ulaw)), "G.711 audio requires MPEG-TS recording; use AAC for MP4");
        anyhow::ensure!(self.recording.segment_seconds <= 7 * 24 * 3600, "recording segment is too long");
        anyhow::ensure!(self.streaming.rtsp.port != 0 && self.streaming.onvif.port != 0, "service ports must not be zero");
        anyhow::ensure!(!self.streaming.rtsp.enabled || self.streaming.rtsp.path.starts_with('/'), "RTSP path must start with '/'");
        anyhow::ensure!(self.streaming.rtsp.username.is_some() == self.streaming.rtsp.password.is_some(), "RTSP username/password must be configured together");
        anyhow::ensure!(self.streaming.onvif.username.is_some() == self.streaming.onvif.password.is_some(), "ONVIF username/password must be configured together");
        anyhow::ensure!(!self.streaming.onvif.enabled || self.streaming.rtsp.enabled, "ONVIF requires RTSP output to be enabled");
        anyhow::ensure!(!self.streaming.rtmp.enabled || self.streaming.rtmp.url.as_ref().is_some_and(|url| !url.trim().is_empty()), "RTMP URL is required when RTMP is enabled");
        anyhow::ensure!(!self.recording.schedule_enabled || valid_time(&self.recording.schedule_start) && valid_time(&self.recording.schedule_stop), "recording schedule must use HH:MM");
        anyhow::ensure!(!self.security.username.trim().is_empty() && !self.security.password.is_empty(), "management username/password are required");
        let mut channel_ids = std::collections::HashSet::new();
        for channel in &self.channels {
            anyhow::ensure!(!channel.id.trim().is_empty(), "channel id is required");
            anyhow::ensure!(channel_ids.insert(channel.id.trim().to_owned()), "channel ids must be unique");
            channel.as_app_config().validate()?;
        }
        Ok(())
    }
}

fn valid_time(value: &str) -> bool {
    let mut parts = value.split(':');
    let hour = parts.next().and_then(|part| part.parse::<u8>().ok());
    let minute = parts.next().and_then(|part| part.parse::<u8>().ok());
    parts.next().is_none() && hour.is_some_and(|hour| hour < 24) && minute.is_some_and(|minute| minute < 60)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn older_configuration_defaults_new_audio_parameters() {
        let config: AppConfig = toml::from_str("[audio]\nsample_rate = 44100\n[video]\nencoder = 'hardware'\n").unwrap();
        assert_eq!(config.audio.input_format, "S16_LE");
        assert_eq!(config.audio.bitrate_kbps, 128);
        assert!(matches!(config.video.encoder, EncoderMode::Hardware));
        assert!(config.validate().is_ok());
    }

    #[test]
    fn serializes_explicit_hardware_encoder_and_audio_preset() {
        let mut config = AppConfig::default();
        config.video.encoder = EncoderMode::V4l2m2m;
        config.audio.input_format = "S24_3LE".into();
        config.audio.bitrate_kbps = 192;
        let reloaded: AppConfig = serde_json::from_value(serde_json::to_value(&config).unwrap()).unwrap();
        assert!(matches!(reloaded.video.encoder, EncoderMode::V4l2m2m));
        assert_eq!(reloaded.audio.input_format, "S24_3LE");
        assert_eq!(reloaded.audio.bitrate_kbps, 192);
        config.audio.input_format = "invalid".into();
        assert!(config.validate().is_err());
    }

    #[tokio::test]
    async fn persists_and_reloads_toml_configuration() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("config.toml");
        let store = ConfigStore::load(path.clone()).await.unwrap();
        let mut config = store.get().await;
        config.video.width = 1280;
        config.recording.segment_seconds = 30;
        store.set(config).await.unwrap();

        let reloaded = ConfigStore::load(path).await.unwrap().get().await;
        assert_eq!(reloaded.video.width, 1280);
        assert_eq!(reloaded.recording.segment_seconds, 30);
    }

    #[tokio::test]
    async fn legacy_network_config_retains_supported_services_only() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("config.toml");
        tokio::fs::write(&path, r#"[streaming.rtsp]
port = 8555
path = "/camera/test"
[streaming.gb28181]
enabled = true
server = "192.0.2.1"
"#).await.unwrap();
        let config = ConfigStore::load(path.clone()).await.unwrap().get().await;
        assert_eq!(config.streaming.rtsp.port, 8555);
        assert_eq!(config.streaming.rtsp.path, "/camera/test");
        let streaming = serde_json::to_value(config.streaming).unwrap();
        assert_eq!(streaming.as_object().unwrap().len(), 3);
        assert!(streaming.get("gb28181").is_none());
        assert!(!tokio::fs::read_to_string(path).await.unwrap().contains("gb28181"));
    }

    #[test]
    fn g711_uses_telephony_output_parameters() {
        let mut audio = AudioConfig::default();
        audio.codec = AudioCodec::G711Alaw;
        assert_eq!(audio.output_params(), (8000, 1));
        audio.codec = AudioCodec::G711Ulaw;
        assert_eq!(audio.output_params(), (8000, 1));
    }

    #[test]
    fn rejects_g711_in_mp4_recordings() {
        let mut config = AppConfig::default();
        config.audio.device = Some("hw:0,0".into());
        config.audio.codec = AudioCodec::G711Alaw;
        config.recording.container = "mp4".into();
        assert!(config.validate().is_err());
    }
}
