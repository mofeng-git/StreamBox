use std::{collections::{BTreeMap, BTreeSet}, fs, path::{Path, PathBuf}, process::Command};

use serde::Serialize;

#[derive(Debug, Clone, Serialize)]
pub struct VideoDevice { pub path: PathBuf, pub name: String, pub driver: String, pub bus: String, pub usb_vid_pid: Option<String>, pub usb_port: Option<String>, pub formats: Vec<String>, pub resolutions: Vec<String>, pub frame_rates: Vec<String>, pub modes: Vec<VideoMode>, pub current_format: Option<String>, pub current_resolution: Option<String>, pub current_frame_rate: Option<String>, pub signal_present: Option<bool>, pub likely_capture: bool }

#[derive(Debug, Clone, Serialize, PartialEq, Eq, PartialOrd, Ord)]
pub struct VideoMode { pub format: String, pub resolution: String, pub frame_rate: u32 }

#[derive(Debug, Clone, Serialize)]
pub struct AudioDevice {
    pub name: String, pub description: String, pub path: String,
    pub usb_vid_pid: Option<String>, pub usb_port: Option<String>,
    pub capture: bool, pub available: bool, pub modes: Vec<AudioMode>,
    pub sample_rates: Vec<u32>, pub formats: Vec<String>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct AudioMode { pub sample_rate: u32, pub format: String, pub channels: Vec<u32> }

// Both video and audio class nodes point below the physical USB device.
fn usb_info(path: &Path) -> (Option<String>, Option<String>) {
    let Ok(path) = fs::canonicalize(path) else { return (None, None) };
    for ancestor in path.ancestors() {
        let (Ok(vid), Ok(pid)) = (fs::read_to_string(ancestor.join("idVendor")), fs::read_to_string(ancestor.join("idProduct"))) else { continue };
        return (Some(format!("{}:{}", vid.trim(), pid.trim())), ancestor.file_name().map(|name| name.to_string_lossy().into_owned()));
    }
    (None, None)
}

fn v4l2_output(path: &Path, arguments: &[&str]) -> Option<String> {
    let output = Command::new("v4l2-ctl").arg("-d").arg(path).args(arguments).output().ok()?;
    Some(String::from_utf8_lossy(&output.stdout).into_owned())
}

pub fn video_devices() -> Vec<VideoDevice> {
    let mut result = Vec::new();
    let Ok(entries) = fs::read_dir("/sys/class/video4linux") else { return result };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        let path = PathBuf::from("/dev").join(&name);
        if !path.exists() { continue; }
        let base = entry.path();
        let card_name = fs::read_to_string(base.join("name")).unwrap_or_else(|_| name.clone()).trim().to_owned();
        let driver = fs::read_link(base.join("device/driver")).ok().and_then(|path| path.file_name().map(|name| name.to_string_lossy().into_owned())).unwrap_or_else(|| "unknown".into());
        let bus = fs::read_link(base.join("device")).map(|path| path.to_string_lossy().into_owned()).unwrap_or_default();
        let all_text = v4l2_output(&path, &["--all"]).unwrap_or_default();
        let device_caps = all_text.split("Device Caps").nth(1).unwrap_or_default().split("Media Driver Info:").next().unwrap_or_default();
        let likely_capture = device_caps.contains("Video Capture") && !device_caps.contains("Memory-to-Memory");
        if !likely_capture { continue; }
        let capability_text = v4l2_output(&path, &["--list-formats-ext"]).unwrap_or_default();
        let modes = parse_video_modes(&capability_text);
        let formats = unique(modes.iter().map(|mode| mode.format.clone()));
        let resolutions = unique(modes.iter().map(|mode| mode.resolution.clone()));
        let frame_rates = unique(modes.iter().map(|mode| format!("{} fps", mode.frame_rate)));
        let current = v4l2_output(&path, &["--get-fmt-video", "--get-parm"]).unwrap_or_default();
        let current_format = current.lines().find_map(|line| line.split_once("Pixel Format").or_else(|| line.split_once("Pixel format")).and_then(|(_, value)| value.split_once(':').and_then(|(_, value)| value.split_whitespace().next().map(normalize_video_format))));
        let current_resolution = current.lines().find_map(|line| line.split_once("Width/Height").and_then(|(_, value)| value.split_once(':').map(|(_, value)| value.trim().replace('/', "x"))));
        let current_frame_rate = current.lines().find_map(|line| line.split_once("Frames per second").and_then(|(_, value)| value.split_once(':').map(|(_, value)| value.split_whitespace().next().unwrap_or_default().to_owned())));
        let signal_present = parse_signal(&all_text);
        let (usb_vid_pid, usb_port) = usb_info(&base.join("device"));
        result.push(VideoDevice { path, name: card_name, driver, bus, usb_vid_pid, usb_port, formats, resolutions, frame_rates, modes, current_format, current_resolution, current_frame_rate, signal_present, likely_capture });
    }
    result.sort_by(|first, second| first.path.cmp(&second.path));
    result
}

pub fn v4l2_encoder_codecs() -> BTreeSet<String> {
    let mut codecs = BTreeSet::new();
    let Ok(entries) = fs::read_dir("/sys/class/video4linux") else { return codecs };
    for entry in entries.flatten() {
        let path = PathBuf::from("/dev").join(entry.file_name());
        let all = v4l2_output(&path, &["--all"]).unwrap_or_default();
        let caps = all.split("Device Caps").nth(1).unwrap_or_default().split("Media Driver Info:").next().unwrap_or_default();
        if !caps.contains("Memory-to-Memory") { continue; }
        // Capture formats on a M2M encoder are the compressed output formats.
        let formats = v4l2_output(&path, &["--list-formats"]).unwrap_or_default();
        if formats.contains("'H264'") { codecs.insert("h264".into()); }
        if formats.contains("'HEVC'") { codecs.insert("h265".into()); }
    }
    codecs
}

fn unique(values: impl Iterator<Item = String>) -> Vec<String> { values.collect::<BTreeSet<_>>().into_iter().collect() }

fn normalize_video_format(value: &str) -> String {
    match value.trim().trim_matches('\'').to_ascii_uppercase().as_str() {
        "MJPG" | "MJPEG" => "mjpeg".into(),
        "YUYV" | "YUYV422" => "yuyv422".into(),
        "H264" | "H.264" => "h264".into(),
        other => other.to_ascii_lowercase(),
    }
}

fn parse_video_modes(text: &str) -> Vec<VideoMode> {
    let mut modes = BTreeSet::new();
    let mut format = None;
    let mut resolution = None;
    for line in text.lines() {
        if let Some(start) = line.find('\'') {
            if let Some(offset) = line[start + 1..].find('\'') {
                let end = start + 1 + offset;
                if line.contains("]:") { format = Some(normalize_video_format(&line[start + 1..end])); resolution = None; }
            }
        }
        if let Some((_, value)) = line.split_once("Size: Discrete ") { resolution = Some(value.trim().to_owned()); }
        if let Some((_, value)) = line.split_once("Interval: Discrete ") {
            let Some(format) = format.clone() else { continue };
            let Some(resolution) = resolution.clone() else { continue };
            let Some(fps) = value.split('(').nth(1).and_then(|part| part.split(" fps)").next()).and_then(|value| value.trim().parse::<f32>().ok()).map(|value| value.round() as u32) else { continue };
            if fps > 0 { modes.insert(VideoMode { format, resolution, frame_rate: fps }); }
        }
    }
    modes.into_iter().collect()
}

pub fn audio_devices() -> Vec<AudioDevice> {
    let pcm = fs::read_to_string("/proc/asound/pcm").unwrap_or_default();
    parse_audio_devices(&pcm).into_iter().map(|(card, device, description)| {
        let available = Path::new("/dev/snd").join(format!("pcmC{card}D{device}c")).exists();
        let (usb_vid_pid, usb_port) = usb_info(&PathBuf::from(format!("/sys/class/sound/card{card}/device")));
        let name = format!("hw:{card},{device}");
        let stream = fs::read_to_string(format!("/proc/asound/card{card}/stream{device}")).unwrap_or_default();
        let mut modes = parse_usb_audio_modes(&stream);
        if modes.is_empty() && available { modes = probe_audio_modes(&name); }
        let sample_rates = modes.iter().map(|mode| mode.sample_rate).collect::<BTreeSet<_>>().into_iter().collect();
        let formats = unique(modes.iter().map(|mode| mode.format.clone()));
        AudioDevice { path: name.clone(), name, description, usb_vid_pid, usb_port, capture: true, available, modes, sample_rates, formats }
    }).collect()
}

// ALSA stream descriptors remain readable while the PCM is busy recording.
fn parse_usb_audio_modes(text: &str) -> Vec<AudioMode> {
    let mut capture = false;
    let mut format = String::new();
    let mut channels = 0;
    let mut modes = BTreeMap::<(u32, String), BTreeSet<u32>>::new();
    for line in text.lines().map(str::trim) {
        if line == "Capture:" { capture = true; }
        if line == "Playback:" { capture = false; }
        if !capture { continue; }
        if line.starts_with("Altset ") { format.clear(); channels = 0; }
        if let Some(value) = line.strip_prefix("Format: ") { format = value.trim().to_owned(); }
        if let Some(value) = line.strip_prefix("Channels: ") { channels = value.parse().unwrap_or(0); }
        if let Some(value) = line.strip_prefix("Rates: ") {
            if channels == 0 || !matches!(format.as_str(), "S16_LE" | "S24_LE" | "S24_3LE" | "S32_LE" | "FLOAT_LE" | "U8") { continue; }
            let rates: Vec<u32> = if value.contains("continuous") {
                // A continuous range has no finite list; expose common presets inside it.
                let bounds: Vec<u32> = value.split(|c: char| !c.is_ascii_digit()).filter_map(|part| part.parse().ok()).collect();
                [8000, 11025, 16000, 22050, 32000, 44100, 48000, 88200, 96000, 176400, 192000].into_iter().filter(|rate| bounds.len() >= 2 && *rate >= bounds[0] && *rate <= bounds[1]).collect()
            } else { value.split(',').filter_map(|part| part.trim().parse().ok()).collect() };
            for rate in rates { modes.entry((rate, format.clone())).or_default().insert(channels); }
        }
    }
    modes.into_iter().map(|((sample_rate, format), channels)| AudioMode { sample_rate, format, channels: channels.into_iter().collect() }).collect()
}

fn probe_audio_modes(device: &str) -> Vec<AudioMode> {
    use std::ffi::{c_char, c_void, CStr, CString};
    extern "C" { fn streambox_audio_probe(device: *const c_char, callback: extern "C" fn(u32, *const c_char, u32, *mut c_void), opaque: *mut c_void); }
    extern "C" fn collect(rate: u32, format: *const c_char, channels: u32, opaque: *mut c_void) {
        let modes = unsafe { &mut *(opaque as *mut BTreeMap<(u32, String), BTreeSet<u32>>) };
        let format = unsafe { CStr::from_ptr(format) }.to_string_lossy().into_owned();
        modes.entry((rate, format)).or_default().insert(channels);
    }
    let Ok(device) = CString::new(device) else { return Vec::new() };
    let mut modes = BTreeMap::<(u32, String), BTreeSet<u32>>::new();
    unsafe { streambox_audio_probe(device.as_ptr(), collect, &mut modes as *mut _ as *mut c_void) };
    modes.into_iter().map(|((sample_rate, format), channels)| AudioMode { sample_rate, format, channels: channels.into_iter().collect() }).collect()
}

fn parse_audio_devices(pcm: &str) -> Vec<(u32, u32, String)> {
    pcm.lines().filter_map(|line| {
        if !line.to_ascii_lowercase().contains("capture") { return None; }
        let (address, description) = line.split_once(':')?;
        let (card, device) = address.trim().split_once('-')?;
        Some((card.parse().ok()?, device.parse().ok()?, description.trim().to_owned()))
    }).collect()
}

fn parse_signal(text: &str) -> Option<bool> {
    let text = text.to_ascii_lowercase();
    if text.contains("no signal") || text.contains("signal: 0") || text.contains("signal detected: no") { Some(false) }
    else if text.contains("signal detected: yes") || text.lines().any(|line| line.contains("video input") && (line.contains("ok") || line.contains("locked"))) { Some(true) }
    else { None }
}

pub fn signal_present(path: &Path) -> Option<bool> {
    if !path.exists() { return Some(false); }
    let output = Command::new("v4l2-ctl").arg("-d").arg(path).arg("--all").output().ok()?;
    if !output.status.success() { return None; }
    parse_signal(&String::from_utf8_lossy(&output.stdout))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn usb_metadata_comes_from_the_physical_device_ancestor() {
        let directory = tempfile::tempdir().unwrap();
        let usb = directory.path().join("1-1.3");
        let interface = usb.join("1-1.3:1.0/video4linux/video0");
        fs::create_dir_all(&interface).unwrap();
        fs::write(usb.join("idVendor"), "534d\n").unwrap();
        fs::write(usb.join("idProduct"), "2109\n").unwrap();
        let link = directory.path().join("device");
        std::os::unix::fs::symlink(&interface, &link).unwrap();
        assert_eq!(usb_info(&link), (Some("534d:2109".into()), Some("1-1.3".into())));
        assert_eq!(usb_info(directory.path()), (None, None));
    }

    #[test]
    fn audio_presets_keep_format_rate_and_channel_constraints_together() {
        let text = "Playback:\n  Altset 1\n  Format: S32_LE\n  Channels: 2\n  Rates: 96000\nCapture:\n  Altset 1\n  Format: S16_LE\n  Channels: 2\n  Rates: 44100, 48000\n  Altset 2\n  Format: S24_3LE\n  Channels: 1\n  Rates: 48000\n  Altset 3\n  Format: S16_LE\n  Channels: 1\n  Rates: 48000\n";
        let modes = parse_usb_audio_modes(text);
        assert_eq!(modes.len(), 3);
        assert_eq!(modes[0], AudioMode { sample_rate: 44100, format: "S16_LE".into(), channels: vec![2] });
        assert_eq!(modes[1], AudioMode { sample_rate: 48000, format: "S16_LE".into(), channels: vec![1,2] });
        assert_eq!(modes[2], AudioMode { sample_rate: 48000, format: "S24_3LE".into(), channels: vec![1] });
    }

    #[test]
    fn continuous_audio_rates_only_offer_presets_in_the_reported_range() {
        let modes = parse_usb_audio_modes("Capture:\n  Altset 1\n  Format: FLOAT_LE\n  Channels: 2\n  Rates: 32000 - 48000 (continuous)\n");
        assert_eq!(modes.iter().map(|mode| mode.sample_rate).collect::<Vec<_>>(), vec![32000, 44100, 48000]);
    }

    #[test]
    fn enumerates_capture_pcm_devices_not_card_description_lines() {
        let pcm = "00-00: USB Audio : USB Audio : capture 1\n01-02: Mic : Mic : playback 1 : capture 1\n02-00: Speaker : Speaker : playback 1\n";
        let devices = parse_audio_devices(pcm);
        assert_eq!(devices.len(), 2);
        assert_eq!((devices[0].0, devices[0].1), (0, 0));
        assert_eq!((devices[1].0, devices[1].1), (1, 2));
    }

    #[test]
    fn missing_video_device_reports_signal_loss() {
        assert_eq!(signal_present(Path::new("/dev/streambox-test-missing")), Some(false));
    }

    #[test]
    fn signal_capability_is_not_evidence_of_a_signal() {
        assert_eq!(parse_signal("signal detection supported"), None);
        assert_eq!(parse_signal("Video input : 0 (Camera 1: ok)"), Some(true));
        assert_eq!(parse_signal("Video input : 0 (no signal)"), Some(false));
    }

    #[test]
    fn parses_video_modes_with_normalized_formats() {
        let text = "[0]: 'MJPG'\n\tSize: Discrete 1920x1080\n\t\tInterval: Discrete 0.033s (30.000 fps)\n[1]: 'YUYV'\n\tSize: Discrete 640x480\n\t\tInterval: Discrete 0.033s (30.000 fps)\n";
        let modes = parse_video_modes(text);
        assert_eq!(modes, vec![
            VideoMode { format: "mjpeg".into(), resolution: "1920x1080".into(), frame_rate: 30 },
            VideoMode { format: "yuyv422".into(), resolution: "640x480".into(), frame_rate: 30 },
        ]);
    }
}
