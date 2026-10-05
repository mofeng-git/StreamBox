use std::{ffi::{c_char, c_int, c_void, CStr, CString}, path::{Path, PathBuf}, sync::{atomic::{AtomicBool, Ordering}, Arc, OnceLock}};

use hwcodec::{
    common::{Quality, RateControl},
    ffmpeg::AVPixelFormat,
    ffmpeg_ram::encode::{EncodeBytesFrame, EncodeContext, Encoder},
};
use serde::Serialize;
use tokio::sync::broadcast;

use crate::config::{AppConfig, AudioCodec, EncoderMode, VideoCodec};
use crate::capture::V4l2Capture;

#[derive(Debug, Clone, Serialize)]
pub struct MediaCapabilities {
    pub encoder_probe_complete: bool,
    pub native_bridge: bool,
    pub native_h264: bool,
    pub native_h265: bool,
    pub ffmpeg_available: bool,
    pub ffmpeg_path: Option<String>,
    pub ffmpeg_h264: bool,
    pub ffmpeg_h265: bool,
    pub active_video_path: String,
    pub active_video_reason: String,
    pub h264: bool,
    pub h265: bool,
    pub hardware_encoder: bool,
    pub note: String,
    pub encoders: Vec<EncoderCapability>,
}

#[derive(Debug, Clone, Serialize)]
pub struct EncoderCapability { pub id: String, pub name: String, pub codecs: Vec<String> }

pub struct NativeVideoEncoder {
    encoder: Option<Encoder>,
    software: *mut c_void,
}

impl NativeVideoEncoder {
    pub fn new(config: &AppConfig) -> anyhow::Result<Self> {
        if matches!(config.video.encoder, EncoderMode::Software) {
            let preset = CString::new(config.video.preset.as_deref().unwrap_or("veryfast"))?;
            let profile = CString::new(config.video.profile.as_deref().unwrap_or(""))?;
            let level = CString::new(config.video.level.as_deref().unwrap_or(""))?;
            let software = unsafe { streambox_video_open(i32::from(matches!(config.video.codec, VideoCodec::H265)), config.video.width as i32, config.video.height as i32, config.video.fps as i32, config.video.bitrate_kbps as i32, preset.as_ptr(), profile.as_ptr(), level.as_ptr()) };
            if software.is_null() { anyhow::bail!(unsafe { CStr::from_ptr(streambox_video_error()).to_string_lossy().into_owned() }); }
            return Ok(Self { encoder: None, software });
        }
        let name = encoder_name(config);
        let encoder = Encoder::new(EncodeContext {
            name,
            width: config.video.width as i32,
            height: config.video.height as i32,
            pixfmt: AVPixelFormat::AV_PIX_FMT_NV12 as i32,
            align: 32,
            fps: config.video.fps as i32,
            gop: keyframe_interval(config) as i32,
            rc: RateControl::RC_CBR,
            quality: Quality::Quality_Default,
            kbs: config.video.bitrate_kbps as i32,
            q: 26,
            thread_count: 1,
        }).map_err(|_| anyhow::anyhow!(hwcodec::ffmpeg_ram::encode::encoder_last_error_message()))?;
        Ok(Self { encoder: Some(encoder), software: std::ptr::null_mut() })
    }

    pub fn encode_nv12(&mut self, frame: &[u8], pts_ms: i64) -> anyhow::Result<Vec<EncodedPacket>> {
        if !self.software.is_null() {
            let mut packets = Vec::<EncodedPacket>::new();
            let result = unsafe { streambox_video_encode(self.software, frame.as_ptr(), frame.len().try_into()?, pts_ms, software_packet_callback, &mut packets as *mut _ as *mut c_void) };
            if result < 0 { anyhow::bail!(unsafe { CStr::from_ptr(streambox_video_error()).to_string_lossy().into_owned() }); }
            return Ok(packets);
        }
        let packets = self.encoder.as_mut().unwrap().encode_bytes(frame, pts_ms).map_err(|error| anyhow::anyhow!("FFmpeg encoder returned {}", error))?;
        Ok(packets.into_iter().map(EncodedPacket::from).collect())
    }

    pub fn request_keyframe(&mut self) { if let Some(encoder) = self.encoder.as_mut() { encoder.request_keyframe(); } }
}

impl Drop for NativeVideoEncoder { fn drop(&mut self) { if !self.software.is_null() { unsafe { streambox_video_close(self.software) }; } } }
extern "C" fn software_packet_callback(data: *const u8, size: c_int, pts: i64, key: c_int, opaque: *mut c_void) {
    if data.is_null() || size <= 0 || opaque.is_null() { return; }
    let packets = unsafe { &mut *(opaque as *mut Vec<EncodedPacket>) };
    packets.push(EncodedPacket { data: unsafe { std::slice::from_raw_parts(data, size as usize) }.to_vec(), pts_ms: pts, keyframe: key != 0 });
}
extern "C" {
    fn streambox_video_open(hevc: c_int, width: c_int, height: c_int, fps: c_int, bitrate: c_int, preset: *const c_char, profile: *const c_char, level: *const c_char) -> *mut c_void;
    fn streambox_video_encode(raw: *mut c_void, data: *const u8, size: c_int, pts: i64, callback: extern "C" fn(*const u8, c_int, i64, c_int, *mut c_void), opaque: *mut c_void) -> c_int;
    fn streambox_video_close(raw: *mut c_void);
    fn streambox_video_error() -> *const c_char;
}

#[derive(Debug, Clone)]
pub struct EncodedPacket { pub data: Vec<u8>, pub pts_ms: i64, pub keyframe: bool }

impl From<EncodeBytesFrame> for EncodedPacket {
    fn from(packet: EncodeBytesFrame) -> Self { Self { data: packet.data.to_vec(), pts_ms: packet.pts, keyframe: packet.key != 0 } }
}

pub struct NativeMuxer { raw: *mut c_void }

unsafe impl Send for NativeMuxer {}

impl NativeMuxer {
    pub fn open_target(target: &str, format: &str, config: &AppConfig, audio: bool) -> anyhow::Result<Self> {
        let target = CString::new(target)?;
        let format = CString::new(format)?;
        let video_codec_id = match config.video.codec { VideoCodec::H264 => 27, VideoCodec::H265 => 173 };
        let (width, height) = video_dimensions(config);
        let audio_codec_id = if audio { match config.audio.codec { AudioCodec::Aac => 86018, AudioCodec::G711Alaw => 65543, AudioCodec::G711Ulaw => 65542 } } else { 0 };
        let (sample_rate, channels) = config.audio.output_params();
        let raw = unsafe { streambox_muxer_open_target(target.as_ptr(), format.as_ptr(), video_codec_id, width as c_int, height as c_int, config.video.fps as c_int, audio_codec_id, sample_rate as c_int, channels as c_int) };
        if raw.is_null() { anyhow::bail!(last_muxer_error()) }
        Ok(Self { raw })
    }

    pub fn open_recording(path: PathBuf, config: &AppConfig, audio: bool) -> anyhow::Result<Self> {
        let format = path.extension().and_then(|extension| extension.to_str()).filter(|extension| extension.eq_ignore_ascii_case("mp4")).map(|_| "mp4").unwrap_or("mpegts");
        Self::open_target(&path.to_string_lossy(), format, config, audio)
    }

    pub fn write(&mut self, packet: &EncodedPacket) -> anyhow::Result<()> {
        let result = unsafe { streambox_muxer_write(self.raw, packet.data.as_ptr(), packet.data.len() as c_int, packet.pts_ms, i32::from(packet.keyframe)) };
        if result < 0 { anyhow::bail!(last_muxer_error()) }
        Ok(())
    }

    pub fn write_audio(&mut self, data: &[u8], pts: i64, duration: i32) -> anyhow::Result<()> {
        let result = unsafe { streambox_muxer_write_audio(self.raw, data.as_ptr(), data.len() as c_int, pts, duration) };
        if result < 0 { anyhow::bail!(last_muxer_error()) }
        Ok(())
    }
}

impl Drop for NativeMuxer {
    fn drop(&mut self) { unsafe { streambox_muxer_close(self.raw) } }
}

pub struct NativeRuntime {
    stop: Arc<AtomicBool>,
    task: tokio::task::JoinHandle<anyhow::Result<()>>,
}

pub struct NativeAudioRuntime {
    stop: Arc<AtomicBool>,
    task: tokio::task::JoinHandle<anyhow::Result<()>>,
}

pub struct NativeMuxRuntime {
    stop: Arc<AtomicBool>,
    task: tokio::task::JoinHandle<anyhow::Result<()>>,
}

impl NativeMuxRuntime {
    pub fn spawn(config: AppConfig, target: String, format: String, mut video: broadcast::Receiver<Vec<u8>>, mut audio: Option<broadcast::Receiver<Vec<u8>>>) -> anyhow::Result<Self> {
        let stop = Arc::new(AtomicBool::new(false));
        let task_stop = stop.clone();
        let task = tokio::task::spawn_blocking(move || {
            let mut muxer = NativeMuxer::open_target(&target, &format, &config, audio.is_some())?;
            let mut video_pts = 0i64;
            let mut audio_pts = 0i64;
            let audio_step = if matches!(config.audio.codec, AudioCodec::Aac) { 1024 } else { 160 };
            while !task_stop.load(Ordering::Acquire) {
                match video.try_recv() {
                    Ok(packet) => {
                        muxer.write(&EncodedPacket { data: packet, pts_ms: video_pts, keyframe: false })?;
                        video_pts += 1000 / config.video.fps.max(1) as i64;
                        if let Some(receiver) = audio.as_mut() {
                            while let Ok(packet) = receiver.try_recv() {
                                muxer.write_audio(&packet, audio_pts, audio_step)?;
                                audio_pts += i64::from(audio_step);
                            }
                        }
                    }
                    Err(broadcast::error::TryRecvError::Empty) => std::thread::sleep(std::time::Duration::from_millis(2)),
                    Err(broadcast::error::TryRecvError::Lagged(_)) => continue,
                    Err(broadcast::error::TryRecvError::Closed) => break,
                }
            }
            Ok(())
        });
        Ok(Self { stop, task })
    }

    pub async fn stop(self) {
        self.stop.store(true, Ordering::Release);
        let _ = self.task.await;
    }

    pub async fn wait(self) -> anyhow::Result<()> {
        self.task.await.map_err(|error| anyhow::anyhow!(error))?
    }
}

struct AudioCallbackContext {
    sender: broadcast::Sender<Vec<u8>>,
    stop: Arc<AtomicBool>,
    audio: *mut c_void,
}

unsafe impl Send for AudioCallbackContext {}

impl NativeAudioRuntime {
    pub fn spawn(config: AppConfig, output: broadcast::Sender<Vec<u8>>) -> anyhow::Result<Self> {
        let device = config.audio.device.clone().ok_or_else(|| anyhow::anyhow!("no audio device selected"))?;
        let stop = Arc::new(AtomicBool::new(false));
        let task_stop = stop.clone();
        let task = tokio::task::spawn_blocking(move || {
            let device = CString::new(device)?;
            let input_format = CString::new(config.audio.input_format.as_str())?;
            let raw = unsafe {
                streambox_audio_open(
                    device.as_ptr(),
                    audio_codec_id(&config.audio.codec),
                    config.audio.sample_rate as c_int,
                    config.audio.channels as c_int,
                    config.audio.volume as c_int,
                    input_format.as_ptr(),
                    (config.audio.bitrate_kbps * 1000) as c_int,
                )
            };
            if raw.is_null() { anyhow::bail!(last_audio_error()) }
            let mut callback_context = AudioCallbackContext { sender: output, stop: task_stop.clone(), audio: raw };
            let result = unsafe { streambox_audio_run(raw, audio_packet_callback, &mut callback_context as *mut _ as *mut c_void) };
            unsafe { streambox_audio_close(raw) };
            if result < 0 { anyhow::bail!(last_audio_error()) }
            Ok(())
        });
        Ok(Self { stop, task })
    }

    pub async fn stop(self) {
        self.stop.store(true, Ordering::Release);
        let _ = self.task.await;
    }

    pub fn is_finished(&self) -> bool { self.task.is_finished() }
}

extern "C" fn audio_packet_callback(data: *const u8, size: c_int, _pts: i64, opaque: *mut c_void) {
    if data.is_null() || size <= 0 || opaque.is_null() { return; }
    let context = unsafe { &mut *(opaque as *mut AudioCallbackContext) };
    if context.stop.load(Ordering::Acquire) {
        unsafe { streambox_audio_stop(context.audio) };
        return;
    }
    let bytes = unsafe { std::slice::from_raw_parts(data, size as usize) }.to_vec();
    let _ = context.sender.send(bytes);
}

fn audio_codec_id(codec: &AudioCodec) -> c_int {
    match codec { AudioCodec::Aac => 0, AudioCodec::G711Alaw => 1, AudioCodec::G711Ulaw => 2 }
}

fn last_audio_error() -> String {
    unsafe { CStr::from_ptr(streambox_audio_last_error()).to_string_lossy().into_owned() }
}

impl NativeRuntime {
    pub async fn spawn(config: AppConfig, output: PathBuf) -> anyhow::Result<Self> {
        let stop = Arc::new(AtomicBool::new(false));
        let task_stop = stop.clone();
        let (sender, receiver) = tokio::sync::oneshot::channel();
        let task = tokio::task::spawn_blocking(move || {
            let mut ready = Some(sender);
            let result = run_native_recording(config, output.clone(), task_stop, &mut ready);
            if ready.is_some() && result.is_err() { let _ = std::fs::remove_file(&output); }
            if let Some(sender) = ready.take() {
                let message = result.as_ref().err().map(ToString::to_string).unwrap_or_else(|| "录像未产生视频帧".into());
                let _ = sender.send(Err(message));
            }
            if let Err(error) = &result { tracing::error!(error = %error, "native recording worker stopped"); }
            result
        });
        let runtime = Self { stop, task };
        match tokio::time::timeout(std::time::Duration::from_secs(15), receiver).await {
            Ok(Ok(Ok(()))) => Ok(runtime),
            result => {
                runtime.stop().await;
                let message = match result { Ok(Ok(Err(message))) => message, _ => "录像启动超时或任务退出".into() };
                anyhow::bail!(message)
            }
        }
    }

    pub async fn stop(self) {
        self.stop.store(true, Ordering::Release);
        let _ = self.task.await;
    }

    pub fn is_finished(&self) -> bool { self.task.is_finished() }
}

pub fn native_video_supported(config: &AppConfig) -> bool {
    cfg!(any(target_arch = "arm", target_arch = "aarch64"))
        && matches!(config.video.codec, VideoCodec::H264 | VideoCodec::H265)
        && matches!(config.video.input_format.to_ascii_lowercase().as_str(), "mjpeg" | "mjpg" | "yuyv" | "yuyv422")
}

pub fn spawn_native_video(config: AppConfig, output: broadcast::Sender<Vec<u8>>) -> NativeRuntime {
    let stop = Arc::new(AtomicBool::new(false));
    let task_stop = stop.clone();
    let task = tokio::task::spawn_blocking(move || {
        let result = run_native_video(config, output, task_stop);
        if let Err(error) = &result { tracing::error!(error = %error, "native video worker stopped"); }
        result
    });
    NativeRuntime { stop, task }
}

fn run_native_recording(config: AppConfig, output: PathBuf, stop: Arc<AtomicBool>, ready: &mut Option<tokio::sync::oneshot::Sender<Result<(), String>>>) -> anyhow::Result<()> {
    let device = config.video.device.clone().or_else(|| crate::device::video_devices().into_iter().find(|device| device.likely_capture).map(|device| device.path.to_string_lossy().into_owned())).ok_or_else(|| anyhow::anyhow!("no video device selected"))?;
    let audio_enabled = config.audio.enabled && config.audio.device.is_some();
    let video_output = output.clone();
    let mut capture = V4l2Capture::open(Path::new(&device), config.video.width, config.video.height, config.video.fps, &config.video.input_format)?;
    let mut encoder = NativeVideoEncoder::new(&config)?;
    let audio_step = if matches!(config.audio.codec, AudioCodec::Aac) { 1024 } else { 160 };
    let mut audio_runtime = None;
    let mut audio_receiver = None;
    let mut muxer = NativeMuxer::open_recording(video_output.clone(), &config, audio_enabled)?;
    if audio_enabled {
        let (sender, receiver) = broadcast::channel(128);
        audio_runtime = Some(NativeAudioRuntime::spawn(config.clone(), sender)?);
        audio_receiver = Some(receiver);
    }
    let mjpeg_input = matches!(config.video.input_format.to_ascii_lowercase().as_str(), "mjpeg" | "mjpg");
    let yuyv_input = config.video.input_format.eq_ignore_ascii_case("yuyv") || config.video.input_format.eq_ignore_ascii_case("yuyv422");
    let mut decoded_nv12 = Vec::with_capacity(config.video.width as usize * config.video.height as usize * 3 / 2);
    let mut audio_pts = 0i64;
    let mut warmup_frames = 4u8;
    let mut video_packets = 0usize;
    let result = (|| -> anyhow::Result<()> {
        while !stop.load(Ordering::Acquire) {
        let frame = capture.next()?;
        if warmup_frames > 0 { warmup_frames -= 1; continue; }
        let nv12 = if mjpeg_input {
            libyuv::mjpg_to_nv12_vec(
                &frame.data,
                &mut decoded_nv12,
                config.video.width as i32,
                config.video.height as i32,
            ).map_err(|error| anyhow::anyhow!("libyuv MJPEG to NV12: {error}"))?;
            decoded_nv12.as_slice()
        } else if yuyv_input {
            decoded_nv12.resize(config.video.width as usize * config.video.height as usize * 3 / 2, 0);
            libyuv::yuy2_to_nv12(
                &frame.data,
                &mut decoded_nv12,
                config.video.width as i32,
                config.video.height as i32,
            ).map_err(|error| anyhow::anyhow!("libyuv YUYV to NV12: {error}"))?;
            decoded_nv12.as_slice()
        } else {
            frame.data.as_slice()
        };
            for packet in encoder.encode_nv12(nv12, frame.sequence as i64 * 1000 / config.video.fps as i64)? {
                muxer.write(&packet)?;
                video_packets += 1;
                if let Some(sender) = ready.take() { let _ = sender.send(Ok(())); }
            }
            if let Some(receiver) = audio_receiver.as_mut() {
                while let Ok(audio) = receiver.try_recv() {
                    muxer.write_audio(&audio, audio_pts, audio_step)?;
                    audio_pts += i64::from(audio_step);
                }
            }
        }
        Ok(())
    })();
    drop(muxer);
    if let Some(runtime) = audio_runtime.take() { tokio::runtime::Handle::current().block_on(runtime.stop()); }
    if video_packets == 0 { let _ = std::fs::remove_file(&video_output); }
    result
}

fn run_native_video(config: AppConfig, output: broadcast::Sender<Vec<u8>>, stop: Arc<AtomicBool>) -> anyhow::Result<()> {
    let device = config.video.device.clone().or_else(|| crate::device::video_devices().into_iter().find(|device| device.likely_capture).map(|device| device.path.to_string_lossy().into_owned())).ok_or_else(|| anyhow::anyhow!("no video device selected"))?;
    let mut capture = V4l2Capture::open(Path::new(&device), config.video.width, config.video.height, config.video.fps, &config.video.input_format)?;
    let mut encoder = NativeVideoEncoder::new(&config)?;
    let mjpeg_input = matches!(config.video.input_format.to_ascii_lowercase().as_str(), "mjpeg" | "mjpg");
    let yuyv_input = config.video.input_format.eq_ignore_ascii_case("yuyv") || config.video.input_format.eq_ignore_ascii_case("yuyv422");
    let mut decoded_nv12 = Vec::with_capacity(config.video.width as usize * config.video.height as usize * 3 / 2);
    let mut warmup_frames = 4u8;
    while !stop.load(Ordering::Acquire) {
        let frame = capture.next()?;
        if warmup_frames > 0 { warmup_frames -= 1; continue; }
        let nv12 = if mjpeg_input {
            libyuv::mjpg_to_nv12_vec(&frame.data, &mut decoded_nv12, config.video.width as i32, config.video.height as i32).map_err(|error| anyhow::anyhow!("libyuv MJPEG to NV12: {error}"))?;
            decoded_nv12.as_slice()
        } else if yuyv_input {
            decoded_nv12.resize(config.video.width as usize * config.video.height as usize * 3 / 2, 0);
            libyuv::yuy2_to_nv12(&frame.data, &mut decoded_nv12, config.video.width as i32, config.video.height as i32).map_err(|error| anyhow::anyhow!("libyuv YUYV to NV12: {error}"))?;
            decoded_nv12.as_slice()
        } else {
            frame.data.as_slice()
        };
        for packet in encoder.encode_nv12(nv12, frame.sequence as i64 * 1000 / config.video.fps as i64)? {
            let data = if packet.data.starts_with(&[0, 0, 0, 1]) || packet.data.starts_with(&[0, 0, 1]) {
                packet.data
            } else {
                let mut annex_b = vec![0, 0, 0, 1];
                annex_b.extend_from_slice(&packet.data);
                annex_b
            };
            let _ = output.send(data);
        }
    }
    Ok(())
}

fn last_muxer_error() -> String {
    unsafe { CStr::from_ptr(streambox_muxer_last_error()).to_string_lossy().into_owned() }
}

extern "C" {
    fn streambox_muxer_open_target(target: *const c_char, format: *const c_char, video_codec_id: c_int, width: c_int, height: c_int, fps: c_int, audio_codec_id: c_int, sample_rate: c_int, channels: c_int) -> *mut c_void;
    fn streambox_muxer_write(muxer: *mut c_void, data: *const u8, size: c_int, pts_ms: i64, keyframe: c_int) -> c_int;
    fn streambox_muxer_write_audio(muxer: *mut c_void, data: *const u8, size: c_int, pts: i64, duration: c_int) -> c_int;
    fn streambox_muxer_close(muxer: *mut c_void);
    fn streambox_remux_mp4(source: *const c_char, target: *const c_char) -> c_int;
    fn streambox_muxer_last_error() -> *const c_char;
    fn streambox_audio_open(device: *const c_char, codec: c_int, sample_rate: c_int, channels: c_int, volume: c_int, input_format: *const c_char, bitrate: c_int) -> *mut c_void;
    fn streambox_audio_run(audio: *mut c_void, callback: extern "C" fn(*const u8, c_int, i64, *mut c_void), opaque: *mut c_void) -> c_int;
    fn streambox_audio_stop(audio: *mut c_void);
    fn streambox_audio_close(audio: *mut c_void);
    fn streambox_audio_last_error() -> *const c_char;
}

extern "C" { fn streambox_encoder_available(name: *const c_char) -> c_int; }

fn encoder_available(name: &str) -> bool {
    let name = CString::new(name).unwrap();
    unsafe { streambox_encoder_available(name.as_ptr()) != 0 }
}

pub fn encoder_capabilities() -> Vec<EncoderCapability> {
    let mut encoders = Vec::new();
    let v4l2_codecs = crate::device::v4l2_encoder_codecs();
    let rockchip_present = ["/dev/mpp_service", "/dev/rkvenc", "/dev/vepu"].iter().any(|path| Path::new(path).exists());
    for (id, name, h264, h265) in [("software", "软件编码（FFmpeg）", "libx264", "libx265"), ("v4l2m2m", "V4L2M2M", "h264_v4l2m2m", "hevc_v4l2m2m"), ("rkmpp", "Rockchip MPP", "h264_rkmpp", "hevc_rkmpp")] {
        let codecs = [("h264", h264), ("h265", h265)].into_iter().filter(|(codec, encoder)| encoder_available(encoder) && (id != "v4l2m2m" || v4l2_codecs.contains(*codec)) && (id != "rkmpp" || rockchip_present)).map(|(codec, _)| codec.to_owned()).collect::<Vec<_>>();
        if !codecs.is_empty() { encoders.push(EncoderCapability { id: id.into(), name: name.into(), codecs }); }
    }
    let codecs = encoders.iter().flat_map(|encoder| encoder.codecs.iter().cloned()).collect::<std::collections::BTreeSet<_>>().into_iter().collect();
    encoders.insert(0, EncoderCapability { id: "auto".into(), name: "自动选择".into(), codecs });
    encoders
}

fn encoder_name(config: &AppConfig) -> String {
    let hevc = matches!(config.video.codec, VideoCodec::H265);
    let software = if hevc { "libx265" } else { "libx264" };
    let v4l2 = if hevc { "hevc_v4l2m2m" } else { "h264_v4l2m2m" };
    let rockchip = if hevc { "hevc_rkmpp" } else { "h264_rkmpp" };
    match config.video.encoder {
        EncoderMode::Software => software,
        EncoderMode::Rkmpp => rockchip,
        EncoderMode::Hardware | EncoderMode::V4l2m2m => v4l2,
        EncoderMode::Auto if cfg!(any(target_arch = "arm", target_arch = "aarch64")) && encoder_available(v4l2) => v4l2,
        EncoderMode::Auto => software,
    }.to_owned()
}

fn video_dimensions(config: &AppConfig) -> (u32, u32) {
    (config.video.width, config.video.height)
}

pub fn capabilities() -> MediaCapabilities {
    capabilities_for(&AppConfig::default())
}

pub fn capabilities_for(config: &AppConfig) -> MediaCapabilities {
    // Encoder probing initializes the platform hardware codec stack. Keep status
    // reads side-effect free; probing can be enabled explicitly during a media
    // bring-up session instead of being triggered by every dashboard refresh.
    static NATIVE: OnceLock<(bool, bool)> = OnceLock::new();
    let native = NATIVE.get_or_init(|| {
        if std::env::var("STREAMBOX_PROBE_ENCODER").as_deref() != Ok("1") { return (false, false); }
        let config = AppConfig::default();
        let h264 = NativeVideoEncoder::new(&config).is_ok();
        let mut h265_config = config.clone();
        h265_config.video.codec = VideoCodec::H265;
        let h265 = NativeVideoEncoder::new(&h265_config).is_ok();
        (h264, h265)
    });
    let ffmpeg_path = Some("bundled-static".to_owned());
    let ffmpeg_available = true;
    let ffmpeg_h264 = encoder_available("libx264");
    let ffmpeg_h265 = encoder_available("libx265");
    let h264 = native.0 || ffmpeg_h264;
    let h265 = native.1 || ffmpeg_h265;
    let hardware_encoder = native.0 || native.1;
    let encoder_probe_complete = std::env::var("STREAMBOX_PROBE_ENCODER").as_deref() == Ok("1");
    let active_video_path = if matches!(config.video.encoder, EncoderMode::Software) { "ffmpeg" } else if !encoder_probe_complete { "unprobed" } else if native_video_supported(config) && (native.0 || native.1) { "native_bridge" } else { "unavailable" };
    let active_video_reason = match active_video_path {
        "native_bridge" => "当前配置使用 Rust 原生采集、libyuv 转换和硬件编码桥接".into(),
        "ffmpeg" => "当前配置使用内置 FFmpeg 库".into(),
        "unprobed" => "硬件编码器会在启动采集时初始化，状态刷新不进行硬件探测".into(),
        _ => "当前配置没有可用的视频编码路径".into(),
    };
    MediaCapabilities { encoder_probe_complete, native_bridge: native.0 || native.1, native_h264: native.0, native_h265: native.1, ffmpeg_available, ffmpeg_path, ffmpeg_h264, ffmpeg_h265, active_video_path: active_video_path.into(), active_video_reason, h264, h265, hardware_encoder, encoders: encoder_capabilities(), note: "内置 FFmpeg 库提供视频编码；非标准 MJPEG 预览帧使用系统 FFmpeg 解码".into() }
}

fn keyframe_interval(config: &AppConfig) -> u32 { config.video.fps.max(1).saturating_mul(2) }

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_h264_encoder_accepts_nv12_frames() {
        let mut config = AppConfig::default();
        config.video.width = 64;
        config.video.height = 64;
        config.video.encoder = EncoderMode::Software;
        config.video.profile = Some("baseline".into());
        config.video.preset = Some("ultrafast".into());
        let mut encoder = NativeVideoEncoder::new(&config).unwrap();
        let frame = vec![128u8; 64 * 64 * 3 / 2];
        let packets = encoder.encode_nv12(&frame, 0).unwrap();
        assert!(!packets.is_empty(), "software encoder must produce a playable packet");
        assert!(packets.iter().any(|packet| packet.keyframe));
    }

    #[test]
    #[cfg(any(target_arch = "arm", target_arch = "aarch64"))]
    fn native_mpegts_muxer_opens_and_writes_header() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("probe.ts");
        let config = AppConfig::default();
        let muxer = NativeMuxer::open_recording(path.clone(), &config, false).unwrap();
        drop(muxer);
        assert!(std::fs::metadata(path).unwrap().is_file());
    }
}

/// Remux transport streams into a browser-readable MP4 without re-encoding.
pub fn remux_preview(source: &Path, target: &Path) -> anyhow::Result<()> {
    let source = CString::new(source.to_string_lossy().as_bytes())?;
    let output = CString::new(target.to_string_lossy().as_bytes())?;
    let result = unsafe { streambox_remux_mp4(source.as_ptr(), output.as_ptr()) };
    if result < 0 {
        let _ = std::fs::remove_file(target);
        anyhow::bail!(last_muxer_error());
    }
    Ok(())
}
