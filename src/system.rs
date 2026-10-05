use serde::Serialize;
use std::{fs, sync::{Mutex, OnceLock}, time::Instant};

#[derive(Debug, Clone, Serialize)]
pub struct SystemInfo {
    pub uptime_seconds: u64,
    pub cpu_usage_percent: f32,
    pub cpu_frequency_mhz: Option<u32>,
    pub memory_available_bytes: u64,
    pub memory_total_bytes: u64,
    pub memory_used_bytes: u64,
    pub load_average: String,
    pub temperature_celsius: Option<f32>,
    pub cpu_count: usize,
    pub disk_available_bytes: u64,
    pub disk_total_bytes: u64,
    pub disk_used_bytes: u64,
}

#[derive(Default)]
struct CpuSample { total: u64, idle: u64, at: Option<Instant> }

pub fn info() -> SystemInfo {
    let uptime_seconds = fs::read_to_string("/proc/uptime").ok().and_then(|s| s.split_whitespace().next()?.parse::<f64>().ok()).map(|value| value.max(0.0) as u64).unwrap_or(0);
    let memory_total_bytes = meminfo_kib("MemTotal");
    let memory_available_bytes = meminfo_kib("MemAvailable");
    let memory_used_bytes = memory_total_bytes.saturating_sub(memory_available_bytes);
    let load_average = fs::read_to_string("/proc/loadavg").unwrap_or_default().split_whitespace().take(3).collect::<Vec<_>>().join(" ");
    let temperature_celsius = fs::read_dir("/sys/class/thermal").ok().and_then(|entries| entries.flatten().find_map(|entry| fs::read_to_string(entry.path().join("temp")).ok())).and_then(|value| value.trim().parse::<f32>().ok()).map(|value| if value > 1000.0 { value / 1000.0 } else { value });
    let cpu_count = std::thread::available_parallelism().map(|value| value.get()).unwrap_or(1);
    let (disk_available_bytes, disk_total_bytes, disk_used_bytes) = disk_space("/");
    SystemInfo { uptime_seconds, cpu_usage_percent: cpu_usage(), cpu_frequency_mhz: cpu_frequency_mhz(), memory_available_bytes, memory_total_bytes, memory_used_bytes, load_average, temperature_celsius, cpu_count, disk_available_bytes, disk_total_bytes, disk_used_bytes }
}

fn meminfo_kib(name: &str) -> u64 {
    fs::read_to_string("/proc/meminfo").ok().and_then(|s| s.lines().find(|line| line.starts_with(name)).and_then(|line| line.split_whitespace().nth(1)?.parse::<u64>().ok())).unwrap_or(0) * 1024
}

fn cpu_usage() -> f32 {
    let Some(line) = fs::read_to_string("/proc/stat").ok().and_then(|s| s.lines().find(|line| line.starts_with("cpu ")).map(str::to_owned)) else { return 0.0 };
    let values = line.split_whitespace().skip(1).filter_map(|value| value.parse::<u64>().ok()).collect::<Vec<_>>();
    if values.len() < 4 { return 0.0; }
    let idle = values[3] + values.get(4).copied().unwrap_or(0);
    let total = values.iter().take(8).sum::<u64>();
    static SAMPLE: OnceLock<Mutex<CpuSample>> = OnceLock::new();
    let mut sample = SAMPLE.get_or_init(|| Mutex::new(CpuSample::default())).lock().unwrap();
    let usage = match sample.at {
        Some(_) if total > sample.total => (1.0 - (idle.saturating_sub(sample.idle) as f32 / (total - sample.total) as f32)) * 100.0,
        _ => 0.0,
    };
    sample.total = total; sample.idle = idle; sample.at = Some(Instant::now());
    usage.clamp(0.0, 100.0)
}

fn cpu_frequency_mhz() -> Option<u32> {
    ["/sys/devices/system/cpu/cpu0/cpufreq/scaling_cur_freq", "/sys/devices/system/cpu/cpu0/cpufreq/cpuinfo_cur_freq"].iter().find_map(|path| fs::read_to_string(path).ok().and_then(|value| value.trim().parse::<u64>().ok()).map(|value| (value / 1000) as u32)).or_else(|| fs::read_to_string("/proc/cpuinfo").ok().and_then(|text| text.lines().find(|line| line.to_ascii_lowercase().contains("cpu mhz")).and_then(|line| line.split(':').nth(1)?.trim().parse::<f32>().ok()).map(|value| value as u32)))
}

fn disk_space(path: &str) -> (u64, u64, u64) {
    let Ok(path) = std::ffi::CString::new(path) else { return (0, 0, 0) };
    let mut stats = std::mem::MaybeUninit::<libc::statvfs>::uninit();
    if unsafe { libc::statvfs(path.as_ptr(), stats.as_mut_ptr()) } != 0 { return (0, 0, 0); }
    let stats = unsafe { stats.assume_init() };
    let block = stats.f_frsize as u64;
    (stats.f_bavail as u64 * block, stats.f_blocks as u64 * block, (stats.f_blocks as u64).saturating_sub(stats.f_bfree as u64) * block)
}
