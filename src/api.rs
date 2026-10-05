use std::path::PathBuf;

use axum::{body::{Body, Bytes}, extract::{Path, Query, State, WebSocketUpgrade}, http::{header, HeaderValue, StatusCode}, response::Response, Json};
use serde::Deserialize;
use anyhow::Context;
use serde_json::json;
use sha2::{Digest, Sha256};
use std::os::unix::fs::PermissionsExt;
use std::process::Command;

use crate::{capture::V4l2Capture, config::AppConfig, device, media, state::{PipelineStatus, SharedState}, storage, system};

#[derive(Debug, Deserialize)]
pub struct LoginRequest { pub username: String, pub password: String }

pub async fn login(State(state): State<SharedState>, Json(credentials): Json<LoginRequest>) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let security = state.config.get().await.security;
    if credentials.username == security.username && credentials.password == security.password {
        Ok(Json(json!({"ok": true, "username": security.username})))
    } else {
        Err((StatusCode::UNAUTHORIZED, "用户名或密码错误".into()))
    }
}

pub async fn health() -> Json<serde_json::Value> { Json(json!({"status":"ok","service":"streambox","version":env!("CARGO_PKG_VERSION")})) }

pub async fn status(State(state): State<SharedState>) -> Json<serde_json::Value> {
    let config = state.config.get().await;
    Json(json!({"runtime": state.snapshot().await, "media": media::capabilities_for(&config), "outputs": state.outputs.status().await, "video_devices": device::video_devices().len(), "audio_devices": device::audio_devices().len()}))
}

pub async fn logs(State(state): State<SharedState>) -> Json<Vec<crate::state::LogRecord>> { Json(state.log_snapshot().await) }
pub async fn channels(State(state): State<SharedState>) -> Json<Vec<crate::state::ChannelStatus>> { Json(state.channel_statuses().await) }

pub async fn start_channel(State(state): State<SharedState>, Path(id): Path<String>) -> Result<Json<serde_json::Value>, (axum::http::StatusCode, String)> {
    let Some(channel) = state.channel(&id).await else { return Err((axum::http::StatusCode::NOT_FOUND, "channel not found".into())); };
    channel.outputs.start(&channel.config).await.map_err(internal)?;
    channel.recordings.manual_start(&channel.config).await.map_err(|error| (axum::http::StatusCode::SERVICE_UNAVAILABLE, error.to_string()))?;
    state.log("info", format!("通道 {id} 已启动")).await;
    Ok(Json(serde_json::json!({"ok": true, "channel": id, "running": true})))
}

pub async fn stop_channel(State(state): State<SharedState>, Path(id): Path<String>) -> Result<Json<serde_json::Value>, (axum::http::StatusCode, String)> {
    let Some(channel) = state.channel(&id).await else { return Err((axum::http::StatusCode::NOT_FOUND, "channel not found".into())); };
    channel.recordings.manual_stop().await;
    channel.outputs.stop().await;
    state.log("info", format!("通道 {id} 已停止")).await;
    Ok(Json(serde_json::json!({"ok": true, "channel": id, "running": false})))
}

pub async fn channel_recordings(State(state): State<SharedState>, Path(id): Path<String>) -> Result<Json<Vec<crate::recording::RecordingFile>>, (axum::http::StatusCode, String)> {
    let Some(channel) = state.channel(&id).await else { return Err((axum::http::StatusCode::NOT_FOUND, "channel not found".into())); };
    Ok(Json(channel.recordings.files(&channel.config).await.map_err(internal)?))
}

pub async fn delete_channel_recording(State(state): State<SharedState>, Path((id, name)): Path<(String, String)>) -> Result<Json<serde_json::Value>, (axum::http::StatusCode, String)> {
    let Some(channel) = state.channel(&id).await else { return Err((axum::http::StatusCode::NOT_FOUND, "channel not found".into())); };
    let files = channel.recordings.files(&channel.config).await.map_err(internal)?;
    let Some(file) = files.into_iter().find(|file| file.name == name) else { return Err((axum::http::StatusCode::NOT_FOUND, "recording not found".into())); };
    tokio::fs::remove_file(file.path).await.map_err(internal)?;
    Ok(Json(serde_json::json!({"ok": true})))
}

pub async fn download_channel_recording(State(state): State<SharedState>, Path((id, name)): Path<(String, String)>) -> Result<Response, (axum::http::StatusCode, String)> {
    let Some(channel) = state.channel(&id).await else { return Err((axum::http::StatusCode::NOT_FOUND, "channel not found".into())); };
    let files = channel.recordings.files(&channel.config).await.map_err(internal)?;
    let Some(file) = files.into_iter().find(|file| file.name == name) else { return Err((axum::http::StatusCode::NOT_FOUND, "recording not found".into())); };
    let bytes = tokio::fs::read(&file.path).await.map_err(internal)?;
    let mut response = Response::new(Body::from(bytes));
    let content_type = if file.name.ends_with(".mp4") { "video/mp4" } else { "video/mp2t" };
    response.headers_mut().insert(header::CONTENT_TYPE, HeaderValue::from_static(content_type));
    response.headers_mut().insert(header::CONTENT_DISPOSITION, HeaderValue::from_str(&format!("attachment; filename=\"{}\"", file.name)).map_err(internal)?);
    Ok(response)
}

pub async fn system_info() -> Json<system::SystemInfo> { Json(system::info()) }
pub async fn storage_info(State(state): State<SharedState>) -> Result<Json<Vec<storage::StorageInfo>>, (StatusCode, String)> {
    Ok(Json(tokio::task::spawn_blocking(move || storage::devices(&state.data_dir)).await.map_err(internal)?.map_err(internal)?))
}

#[derive(Debug, Deserialize)]
pub struct StorageRequest { pub device: String, pub action: String, pub confirmation: Option<String> }

pub async fn storage_action(State(state): State<SharedState>, Json(request): Json<StorageRequest>) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    static LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
    let _guard = LOCK.lock().await;
    let mut directories = Vec::new();
    if state.recordings.is_running().await { directories.push(state.config.get().await.recording.directory); }
    let channels = state.channels.read().await.values().cloned().collect::<Vec<_>>();
    for channel in channels { if channel.recordings.is_running().await { directories.push(channel.config.get().await.recording.directory); } }
    let data_dir = state.data_dir.clone();
    let device = request.device.clone();
    let action = request.action.clone();
    tokio::task::spawn_blocking(move || storage::operation(&request.device, &request.action, request.confirmation.as_deref(), &data_dir, &directories))
        .await.map_err(internal)?.map_err(|error| (StatusCode::BAD_REQUEST, error.to_string()))?;
    state.log("info", format!("磁盘操作 {action}: {device}")).await;
    Ok(Json(json!({"ok": true})))
}

#[derive(Debug, Deserialize)]
pub struct SmartQuery { pub device: String }
pub async fn storage_smart(State(state): State<SharedState>, axum::extract::Query(query): axum::extract::Query<SmartQuery>) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    Ok(Json(tokio::task::spawn_blocking(move || storage::smart(&query.device, &state.data_dir)).await.map_err(internal)?.map_err(|error| (StatusCode::SERVICE_UNAVAILABLE, error.to_string()))?))
}

struct CachedPreview { source: String, captured_at: std::time::Instant, jpeg: Vec<u8> }

pub async fn preview(State(state): State<SharedState>) -> Result<Response, (StatusCode, String)> {
    static CACHE: tokio::sync::Mutex<Option<CachedPreview>> = tokio::sync::Mutex::const_new(None);
    let mut cached = CACHE.lock().await;
    let config = state.config.get().await;
    let source = format!("{}:{}x{}:{}:{}", config.video.device.as_deref().unwrap_or("auto"), config.video.width, config.video.height, config.video.fps, config.video.input_format);
    if let Some(frame) = cached.as_ref().filter(|frame| frame.source == source && frame.captured_at.elapsed() < std::time::Duration::from_secs(2)) {
        return Ok(preview_response(frame.jpeg.clone()));
    }
    let video = config.video.clone();
    let device_path = video.device.clone().or_else(|| device::video_devices().into_iter().find(|item| item.likely_capture).map(|item| item.path.to_string_lossy().into_owned())).ok_or((StatusCode::NOT_FOUND, "没有可用的视频输入设备".into()))?;
    let frame = tokio::task::spawn_blocking(move || {
        let cached = crate::capture::recent_frame(std::path::Path::new(&device_path));
        let mut capture = if cached.is_none() { Some(V4l2Capture::open(std::path::Path::new(&device_path), video.width, video.height, video.fps, &video.input_format)?) } else { None };
        let (format, mut frame) = if let Some(cached) = cached { cached } else { let device = capture.as_mut().unwrap(); (device.format.clone(), device.next()?) };
        if matches!(video.input_format.to_ascii_lowercase().as_str(), "mjpeg" | "mjpg") {
            for _ in 0..4 {
                if frame.data.starts_with(&[0xff, 0xd8]) { break; }
                if let Some(capture) = capture.as_mut() { frame = capture.next()?; } else { break; }
            }
        }
        if matches!(video.input_format.to_ascii_lowercase().as_str(), "mjpeg" | "mjpg") && frame.data.starts_with(&[0xff, 0xd8]) {
            image::load_from_memory(&frame.data).context("MJPEG 帧解码失败")?;
            return Ok::<Vec<u8>, anyhow::Error>(frame.data);
        }
        if matches!(video.input_format.to_ascii_lowercase().as_str(), "mjpeg" | "mjpg") {
            drop(capture);
            let output = Command::new("timeout").args(["8", "ffmpeg"])
                .args(["-loglevel", "error", "-f", "v4l2", "-input_format", "mjpeg", "-video_size"])
                .arg(format!("{}x{}", video.width, video.height))
                .args(["-framerate"])
                .arg(video.fps.to_string())
                .args(["-i", &device_path, "-frames:v", "1", "-f", "image2pipe", "-vcodec", "mjpeg", "pipe:1"])
                .output();
            if let Ok(output) = output { if output.status.success() && output.stdout.starts_with(&[0xff, 0xd8]) { return Ok(output.stdout); } }
            anyhow::bail!("采集设备返回了无法解码的 MJPEG 帧")
        }
        anyhow::ensure!(matches!(video.input_format.as_str(), "yuyv" | "yuyv422"), "预览暂不支持此输入格式");
        let width = format.width as usize;
        let height = format.height as usize;
        let stride = format.plane_fmt.first().map(|plane| plane.bytesperline as usize).unwrap_or(width * 2).max(width * 2);
        anyhow::ensure!(width % 2 == 0 && frame.data.len() >= stride * height, "YUYV 图像大小无效");
        let mut rgb = vec![0u8; width * height * 3];
        for y in 0..height {
            for x in 0..width {
                let offset = y * stride + (x / 2) * 4;
                let yv = (frame.data[offset + if x % 2 == 0 { 0 } else { 2 }] as f32 - 16.0).max(0.0) * 1.164;
                let u = frame.data[offset + 1] as f32 - 128.0;
                let v = frame.data[offset + 3] as f32 - 128.0;
                let out = (y * width + x) * 3;
                rgb[out] = (yv + 1.596 * v).clamp(0.0, 255.0) as u8;
                rgb[out + 1] = (yv - 0.392 * u - 0.813 * v).clamp(0.0, 255.0) as u8;
                rgb[out + 2] = (yv + 2.017 * u).clamp(0.0, 255.0) as u8;
            }
        }
        let mut jpeg = Vec::new();
        let encoder = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut jpeg, 82);
        image::ImageEncoder::write_image(encoder, &rgb, width as u32, height as u32, image::ExtendedColorType::Rgb8)?;
        Ok(jpeg)
    }).await.map_err(|error| (StatusCode::INTERNAL_SERVER_ERROR, error.to_string()))?.map_err(|error| (StatusCode::SERVICE_UNAVAILABLE, error.to_string()))?;
    *cached = Some(CachedPreview { source, captured_at: std::time::Instant::now(), jpeg: frame.clone() });
    Ok(preview_response(frame))
}

fn preview_response(frame: Vec<u8>) -> Response {
    let mut response = Response::new(Body::from(frame));
    response.headers_mut().insert(header::CONTENT_TYPE, HeaderValue::from_static("image/jpeg"));
    response.headers_mut().insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store, max-age=0"));
    response
}
pub async fn get_config(State(state): State<SharedState>) -> Json<AppConfig> { Json(state.config.get().await) }

pub async fn ota_status(State(state): State<SharedState>) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let path = state.data_dir.join("update/streambox.new");
    let metadata = tokio::fs::metadata(&path).await.ok();
    let digest = if metadata.is_some() { Some(file_sha256(&path).await.map_err(internal)?) } else { None };
    Ok(Json(json!({"staged": metadata.is_some(), "bytes": metadata.map(|value| value.len()).unwrap_or(0), "sha256": digest})))
}

pub async fn ota_upload(State(state): State<SharedState>, body: Bytes) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    validate_update_binary(&body)?;
    let directory = state.data_dir.join("update");
    tokio::fs::create_dir_all(&directory).await.map_err(internal)?;
    let temporary = directory.join("streambox.new.tmp");
    let staged = directory.join("streambox.new");
    tokio::fs::write(&temporary, &body).await.map_err(internal)?;
    let mut permissions = tokio::fs::metadata(&temporary).await.map_err(internal)?.permissions();
    permissions.set_mode(0o755);
    tokio::fs::set_permissions(&temporary, permissions).await.map_err(internal)?;
    tokio::fs::rename(&temporary, &staged).await.map_err(internal)?;
    let digest = sha256(&body);
    state.log("info", format!("OTA 固件已暂存: {} bytes, sha256={digest}", body.len())).await;
    Ok(Json(json!({"staged": true, "bytes": body.len(), "sha256": digest})))
}

pub async fn ota_apply(State(state): State<SharedState>) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let staged = state.data_dir.join("update/streambox.new");
    let current = std::env::current_exe().map_err(internal)?;
    let metadata = tokio::fs::metadata(&staged).await.map_err(|_| (StatusCode::NOT_FOUND, "no staged OTA binary".into()))?;
    if metadata.len() == 0 { return Err((StatusCode::BAD_REQUEST, "staged OTA binary is empty".into())); }
    let backup = current.with_extension("previous");
    let failed = current.with_extension("failed");
    let _ = tokio::fs::remove_file(&failed).await;
    let _ = tokio::fs::remove_file(&backup).await;
    tokio::fs::rename(&current, &backup).await.map_err(internal)?;
    if let Err(error) = tokio::fs::rename(&staged, &current).await {
        let _ = tokio::fs::rename(&backup, &current).await;
        return Err(internal(error));
    }
    state.log("warn", format!("OTA 已应用，准备重启服务: {}", current.display())).await;
    tokio::spawn(async {
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
        let _ = tokio::process::Command::new("systemctl").args(["restart", "streambox"]).status().await;
    });
    Ok(Json(json!({"applied": true, "restart": "scheduled"})))
}

pub async fn put_config(State(state): State<SharedState>, Json(config): Json<AppConfig>) -> Result<Json<AppConfig>, (axum::http::StatusCode, String)> {
    if let Err(error) = config.validate() { state.log("warn", format!("配置校验失败: {error}" )).await; return Err((axum::http::StatusCode::BAD_REQUEST, error.to_string())); }
    let previous = state.config.get().await;
    let check = config.clone();
    tokio::task::spawn_blocking(move || validate_input_selection(&check, &previous))
        .await.map_err(internal)?.map_err(|error| (StatusCode::BAD_REQUEST, error.to_string()))?;
    let previous_outputs = state.outputs.status().await;
    let was_output_active = previous_outputs.rtsp || previous_outputs.rtmp || previous_outputs.onvif;
    if was_output_active { state.outputs.stop().await; }
    state.recordings.update_config(&state.config, config.clone()).await.map_err(internal)?;
    state.sync_channels().await;
    let mut restart_errors = Vec::new();
    if was_output_active {
        if let Err(error) = state.outputs.start(&state.config).await { restart_errors.push(format!("输出重启失败: {error}")); }
    }
    if !restart_errors.is_empty() {
        let message = restart_errors.join("; ");
        state.log("error", message.clone()).await;
        return Err((axum::http::StatusCode::SERVICE_UNAVAILABLE, format!("配置已保存，但运行任务恢复失败: {message}")));
    }
    state.log("info", "配置已保存").await;
    Ok(Json(config))
}

fn validate_input_selection(config: &AppConfig, previous: &AppConfig) -> anyhow::Result<()> {
    let v = &config.video;
    let old = &previous.video;
    if v.device != old.device || v.width != old.width || v.height != old.height || v.fps != old.fps || v.input_format != old.input_format {
        let devices = device::video_devices();
        let selected = match &v.device { Some(path) => devices.iter().find(|device| device.path.to_string_lossy() == path.as_str()), None => devices.first() };
        let selected = selected.ok_or_else(|| anyhow::anyhow!("所选视频设备未连接"))?;
        anyhow::ensure!(selected.modes.iter().any(|mode| mode.resolution == format!("{}x{}", v.width, v.height) && mode.frame_rate == v.fps && mode.format == v.input_format), "所选视频设备不支持该输入预设");
    }
    if std::mem::discriminant(&v.encoder) != std::mem::discriminant(&old.encoder) || std::mem::discriminant(&v.codec) != std::mem::discriminant(&old.codec) {
        let id = match v.encoder { crate::config::EncoderMode::Auto => "auto", crate::config::EncoderMode::Software => "software", crate::config::EncoderMode::Hardware | crate::config::EncoderMode::V4l2m2m => "v4l2m2m", crate::config::EncoderMode::Rkmpp => "rkmpp" };
        let codec = match v.codec { crate::config::VideoCodec::H264 => "h264", crate::config::VideoCodec::H265 => "h265" };
        anyhow::ensure!(media::encoder_capabilities().iter().any(|encoder| encoder.id == id && encoder.codecs.iter().any(|value| value == codec)), "所选编码器不支持该视频编码");
    }
    let a = &config.audio;
    let old = &previous.audio;
    if a.enabled && a.device.is_some() && (!old.enabled || a.device != old.device || a.sample_rate != old.sample_rate || a.input_format != old.input_format || a.channels != old.channels) {
        let devices = device::audio_devices();
        let selected = devices.iter().find(|device| Some(&device.name) == a.device.as_ref()).ok_or_else(|| anyhow::anyhow!("所选音频设备未连接"))?;
        anyhow::ensure!(selected.modes.iter().any(|mode| mode.sample_rate == a.sample_rate && mode.format == a.input_format && mode.channels.contains(&a.channels)), "所选音频设备不支持该输入预设");
    }
    Ok(())
}

pub async fn video_devices() -> Result<Json<Vec<device::VideoDevice>>, (StatusCode, String)> { Ok(Json(tokio::task::spawn_blocking(device::video_devices).await.map_err(internal)?)) }
pub async fn audio_devices() -> Result<Json<Vec<device::AudioDevice>>, (StatusCode, String)> { Ok(Json(tokio::task::spawn_blocking(device::audio_devices).await.map_err(internal)?)) }

pub async fn start_pipeline(State(state): State<SharedState>) -> Json<serde_json::Value> {
    let config = state.config.get().await;
    let outputs = state.outputs.start(&state.config).await.ok();
    state.set_runtime(|runtime| { runtime.status = PipelineStatus::Running; runtime.selected_video = config.video.device.clone(); runtime.last_error = None; }).await;
    Json(json!({"ok":true,"status":"running","outputs":outputs}))
}

pub async fn stop_pipeline(State(state): State<SharedState>) -> Json<serde_json::Value> {
    state.recordings.manual_stop().await;
    state.outputs.stop().await;
    state.set_runtime(|runtime| { runtime.status = PipelineStatus::Idle; runtime.recording = false; }).await;
    Json(json!({"ok":true,"status":"idle"}))
}

pub async fn start_outputs(State(state): State<SharedState>) -> Result<Json<serde_json::Value>, (axum::http::StatusCode, String)> {
    let status = state.outputs.start(&state.config).await.map_err(internal)?;
    Ok(Json(json!({"ok": true, "outputs": status})))
}

pub async fn stop_outputs(State(state): State<SharedState>) -> Json<serde_json::Value> {
    state.outputs.stop().await;
    if !state.recordings.is_running().await { state.set_runtime(|runtime| runtime.status = PipelineStatus::Idle).await; }
    Json(json!({"ok": true}))
}

pub async fn start_recording(State(state): State<SharedState>) -> Result<Json<serde_json::Value>, (axum::http::StatusCode, String)> {
    if let Err(error) = state.recordings.manual_start(&state.config).await { state.log("error", format!("录像启动失败: {error}" )).await; return Err((axum::http::StatusCode::SERVICE_UNAVAILABLE, error.to_string())); }
    state.log("info", "录像已启动").await;
    state.set_runtime(|runtime| { runtime.status = PipelineStatus::Running; runtime.recording = true; runtime.last_error = None; }).await;
    Ok(Json(json!({"ok":true,"recording":true})))
}

pub async fn stop_recording(State(state): State<SharedState>) -> Json<serde_json::Value> {
    state.recordings.manual_stop().await;
    state.log("info", "录像已停止").await;
    let outputs = state.outputs.status().await;
    state.set_runtime(|runtime| { runtime.recording = false; if !(outputs.rtsp || outputs.rtmp || outputs.onvif) { runtime.status = PipelineStatus::Idle; } }).await;
    Json(json!({"ok":true,"recording":false}))
}

pub async fn resume_recording_policy(State(state): State<SharedState>) -> Json<serde_json::Value> {
    state.recordings.resume_policy(&state.config).await;
    Json(json!({"ok": true, "recording": state.recordings.is_running().await}))
}

pub async fn recordings(State(state): State<SharedState>) -> Result<Json<Vec<crate::recording::RecordingFile>>, (axum::http::StatusCode, String)> { Ok(Json(state.recordings.files(&state.config).await.map_err(internal)?)) }

pub async fn delete_recording(State(state): State<SharedState>, Path(name): Path<String>) -> Result<Json<serde_json::Value>, (axum::http::StatusCode, String)> {
    let files = state.recordings.files(&state.config).await.map_err(internal)?;
    let Some(file) = files.into_iter().find(|file| file.name == name) else { return Err((axum::http::StatusCode::NOT_FOUND, "recording not found".into())) };
    if state.recordings.is_current(&file.path).await { return Err((StatusCode::CONFLICT, "此文件正在录制，请先停止录像".into())); }
    tokio::fs::remove_file(PathBuf::from(file.path)).await.map_err(internal)?;
    Ok(Json(json!({"ok":true})))
}

pub async fn download_recording(State(state): State<SharedState>, Path(name): Path<String>, request: axum::http::Request<Body>) -> Result<Response, (StatusCode, String)> {
    let files = state.recordings.files(&state.config).await.map_err(internal)?;
    let file = files.into_iter().find(|file| file.name == name).ok_or((StatusCode::NOT_FOUND, "recording not found".into()))?;
    let response = tower_http::services::ServeFile::new(&file.path).try_call(request).await.map_err(internal)?;
    let mut response = response.map(Body::new);
    response.headers_mut().insert(header::CONTENT_DISPOSITION, HeaderValue::from_str(&format!("attachment; filename=\"{}\"", file.name)).map_err(internal)?);
    Ok(response)
}

struct PlaybackSession {
    name: String,
    source: String,
    path: PathBuf,
    expires: std::time::Instant,
}
static PLAYBACK: std::sync::LazyLock<tokio::sync::Mutex<std::collections::HashMap<String, PlaybackSession>>> = std::sync::LazyLock::new(Default::default);

pub async fn prepare_recording_preview(State(state): State<SharedState>, Path(name): Path<String>) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let files = state.recordings.files(&state.config).await.map_err(internal)?;
    let file = files.into_iter().find(|file| file.name == name).ok_or((StatusCode::NOT_FOUND, "录像文件不存在".into()))?;
    if state.recordings.is_current(&file.path).await { return Err((StatusCode::CONFLICT, "此文件正在录制，请在分段完成或停止录像后预览".into())); }
    let mut sessions = PLAYBACK.lock().await;
    sessions.retain(|_, session| session.expires > std::time::Instant::now());
    if sessions.len() >= 128 { return Err((StatusCode::TOO_MANY_REQUESTS, "预览会话过多，请稍后重试".into())); }
    let mut path = PathBuf::from(&file.path);
    if file.name.ends_with(".ts") {
        let directory = state.data_dir.join("playback");
        tokio::fs::create_dir_all(&directory).await.map_err(internal)?;
        // Remove old preview copies, retaining originals and active sessions.
        let mut entries = tokio::fs::read_dir(&directory).await.map_err(internal)?;
        while let Some(entry) = entries.next_entry().await.map_err(internal)? {
            let metadata = entry.metadata().await.map_err(internal)?;
            if metadata.modified().ok().and_then(|time| time.elapsed().ok()).is_some_and(|age| age.as_secs() > 3600)
                && !sessions.values().any(|session| session.path == entry.path()) { let _ = tokio::fs::remove_file(entry.path()).await; }
        }
        let key = Sha256::digest(format!("{}:{}:{:?}", file.path, file.bytes, file.modified)).iter().map(|byte| format!("{byte:02x}")).collect::<String>();
        path = directory.join(format!("{key}.mp4"));
        if !path.exists() {
            let source = PathBuf::from(&file.path);
            let target = path.clone();
            tokio::task::spawn_blocking(move || media::remux_preview(&source, &target)).await.map_err(internal)?.map_err(|error| (StatusCode::UNPROCESSABLE_ENTITY, format!("此录像暂不能在网页预览：{error}")))?;
        }
    }
    let token = uuid::Uuid::new_v4().to_string();
    sessions.insert(token.clone(), PlaybackSession { name: name.clone(), source: file.path, path, expires: std::time::Instant::now() + std::time::Duration::from_secs(3600) });
    Ok(Json(json!({"token": token})))
}

#[derive(Deserialize)]
pub struct PlaybackQuery { token: String }

pub async fn recording_preview(State(state): State<SharedState>, Path(name): Path<String>, Query(query): Query<PlaybackQuery>, request: axum::http::Request<Body>) -> Result<Response, (StatusCode, String)> {
    let (source, path) = {
        let sessions = PLAYBACK.lock().await;
        let session = sessions.get(&query.token).filter(|session| session.name == name && session.expires > std::time::Instant::now()).ok_or((StatusCode::UNAUTHORIZED, "预览已过期，请重新打开播放器".into()))?;
        (session.source.clone(), session.path.clone())
    };
    let files = state.recordings.files(&state.config).await.map_err(internal)?;
    if !files.iter().any(|file| file.name == name && file.path == source) { return Err((StatusCode::NOT_FOUND, "录像文件不存在".into())); }
    let response = tower_http::services::ServeFile::new(path).try_call(request).await.map_err(internal)?;
    let mut response = response.map(Body::new);
    response.headers_mut().insert(header::CACHE_CONTROL, HeaderValue::from_static("private, no-store"));
    response.headers_mut().insert(header::REFERRER_POLICY, HeaderValue::from_static("no-referrer"));
    Ok(response)
}

pub async fn status_ws(ws: WebSocketUpgrade, State(state): State<SharedState>) -> Response {
    ws.on_upgrade(move |mut socket| async move {
        let mut events = state.events.subscribe();
        let _ = socket.send(axum::extract::ws::Message::Text(serde_json::to_string(&state.snapshot().await).unwrap().into())).await;
        while let Ok(status) = events.recv().await { if socket.send(axum::extract::ws::Message::Text(serde_json::to_string(&status).unwrap().into())).await.is_err() { break; } }
    })
}

fn internal(error: impl std::fmt::Display) -> (axum::http::StatusCode, String) { (axum::http::StatusCode::INTERNAL_SERVER_ERROR, error.to_string()) }

fn validate_update_binary(body: &[u8]) -> Result<(), (StatusCode, String)> {
    if body.len() < 20 || &body[..4] != b"\x7fELF" { return Err((StatusCode::BAD_REQUEST, "OTA payload is not an ELF executable".into())); }
    if body[4] != 1 || body[5] != 1 { return Err((StatusCode::BAD_REQUEST, "OTA binary must be a 32-bit little-endian ELF".into())); }
    let machine = u16::from_le_bytes([body[18], body[19]]);
    let expected_machine = if cfg!(target_arch = "arm") { 40 } else if cfg!(target_arch = "aarch64") { 183 } else if cfg!(target_arch = "x86_64") { 62 } else { 3 };
    if machine != expected_machine { return Err((StatusCode::BAD_REQUEST, "OTA binary architecture does not match this device".into())); }
    Ok(())
}

async fn file_sha256(path: &std::path::Path) -> anyhow::Result<String> { Ok(sha256(&tokio::fs::read(path).await?)) }
fn sha256(data: &[u8]) -> String { Sha256::digest(data).iter().map(|byte| format!("{byte:02x}")).collect() }
