use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::{fs, os::unix::fs::FileTypeExt, path::{Path, PathBuf}, process::Command};

#[derive(Debug, Clone, Serialize)]
pub struct StorageInfo {
    pub name: String,
    pub device: String,
    pub parent_device: String,
    pub mount_point: String,
    pub filesystem: String,
    pub total_bytes: u64,
    pub used_bytes: Option<u64>,
    pub available_bytes: Option<u64>,
    pub read_only: bool,
    pub removable: bool,
    pub mounted: bool,
    pub protected: bool,
    pub can_format: bool,
}

#[derive(Debug, Deserialize)]
struct BlockList { blockdevices: Vec<BlockDevice> }
#[derive(Debug, Deserialize)]
struct BlockDevice {
    name: String, path: String,
    #[serde(rename = "type")] kind: String,
    size: u64, fstype: Option<String>, mountpoint: Option<String>,
    ro: bool, rm: bool, model: Option<String>,
    #[serde(default)] children: Vec<BlockDevice>,
}

pub fn devices(data_dir: &Path) -> Result<Vec<StorageInfo>> {
    let data_dir = fs::canonicalize(data_dir).unwrap_or_else(|_| data_dir.to_path_buf());
    let output = Command::new("lsblk").args(["-J", "-b", "-o", "NAME,PATH,TYPE,SIZE,FSTYPE,MOUNTPOINT,RO,RM,MODEL"]).output().context("读取块设备列表需要 lsblk")?;
    anyhow::ensure!(output.status.success(), "读取块设备失败: {}", String::from_utf8_lossy(&output.stderr));
    let list: BlockList = serde_json::from_slice(&output.stdout).context("解析块设备列表")?;
    let mut result = Vec::new();
    for disk in &list.blockdevices {
        if disk.kind != "disk" || disk.name.starts_with("zram") || disk.name.starts_with("loop") || disk.name.contains("boot") || disk.size == 0 { continue; }
        let protected = tree_protected(disk, &data_dir);
        collect(disk, disk, protected, &mut result);
    }
    result.sort_by_key(|disk| (!disk.protected, disk.device.clone()));
    Ok(result)
}

fn tree_protected(disk: &BlockDevice, data_dir: &Path) -> bool {
    disk.mountpoint.as_deref().is_some_and(|mount| matches!(mount, "/" | "/boot" | "/boot/efi" | "/usr" | "/var" | "/etc") || data_dir.starts_with(mount)) || disk.children.iter().any(|child| tree_protected(child, data_dir))
}

fn collect(node: &BlockDevice, parent: &BlockDevice, protected: bool, result: &mut Vec<StorageInfo>) {
    if !node.children.is_empty() { for child in &node.children { collect(child, parent, protected, result); } return; }
    if !matches!(node.kind.as_str(), "disk" | "part") { return; }
    let mounted = node.mountpoint.is_some();
    let mount_point = node.mountpoint.clone().unwrap_or_default();
    let (total_bytes, available_bytes, used_bytes) = if mounted {
        let (total, available, used) = stat_space(Path::new(&mount_point));
        (total, Some(available), Some(used))
    } else { (node.size, None, None) };
    let mount_read_only = fs::read_to_string("/proc/mounts").unwrap_or_default().lines().any(|line| { let fields: Vec<_> = line.split_whitespace().collect(); fields.len() >= 4 && fields[0] == node.path && fields[3].split(',').any(|option| option == "ro") });
    let read_only = node.ro || mount_read_only;
    result.push(StorageInfo {
        name: if mount_point == "/" { "系统盘".into() } else { format!("{} · {}", parent.model.as_deref().filter(|name| !name.trim().is_empty()).unwrap_or(&parent.name).trim(), node.name) },
        device: node.path.clone(), parent_device: parent.path.clone(), mount_point,
        filesystem: node.fstype.clone().unwrap_or_default(), total_bytes, used_bytes, available_bytes,
        read_only, removable: !protected && parent.rm, mounted, protected,
        can_format: !protected && !node.ro && node.kind == "part" && !mounted,
    });
}

pub fn operation(device: &str, action: &str, confirmation: Option<&str>, data_dir: &Path, active_directories: &[PathBuf]) -> Result<()> {
    let target = devices(data_dir)?.into_iter().find(|disk| disk.device == device).context("设备不存在，请刷新列表")?;
    anyhow::ensure!(!target.protected, "系统盘和服务数据盘不能执行此操作");
    anyhow::ensure!(fs::metadata(device)?.file_type().is_block_device(), "目标不是块设备");
    if target.mounted {
        anyhow::ensure!(!active_directories.iter().any(|directory| fs::canonicalize(directory).unwrap_or_else(|_| directory.clone()).starts_with(&target.mount_point)), "此设备正在用于录像，请先停止录像");
    }
    match action {
        "mount" => {
            anyhow::ensure!(!target.mounted, "设备已挂载");
            anyhow::ensure!(!target.filesystem.is_empty(), "未检测到文件系统，请先格式化分区");
            let name = Path::new(device).file_name().context("无效设备名")?;
            let mount = Path::new("/mnt/streambox").join(name);
            fs::create_dir_all(&mount)?;
            anyhow::ensure!(fs::canonicalize(&mount)? == mount, "挂载目录不能是符号链接");
            run("mount", &["-o", "nosuid,nodev,noexec", "--", device, mount.to_str().context("无效挂载目录")?])?;
        }
        "unmount" => { anyhow::ensure!(target.mounted, "设备未挂载"); run("umount", &["--", &target.mount_point])?; }
        "format" => {
            anyhow::ensure!(target.can_format, "仅允许格式化未挂载的非系统分区");
            anyhow::ensure!(confirmation == Some(device), "请完整输入设备路径以确认清除数据");
            run("mkfs.ext4", &["-F", "--", device])?;
        }
        _ => bail!("不支持的磁盘操作"),
    }
    Ok(())
}

pub fn smart(device: &str, data_dir: &Path) -> Result<serde_json::Value> {
    let target = devices(data_dir)?.into_iter().find(|disk| disk.device == device).context("设备不存在")?;
    let output = Command::new("timeout").args(["8", "smartctl", "-j", "-a", &target.parent_device]).output().context("无法读取 SMART")?;
    anyhow::ensure!(output.status.code() != Some(127), "测试机没有安装 smartctl，无法读取 SMART");
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).context("设备不支持 SMART 或返回了无效数据")?;
    Ok(value)
}

fn run(program: &str, args: &[&str]) -> Result<()> {
    let output = Command::new("timeout").args(["60", program]).args(args).output().with_context(|| format!("执行 {program}"))?;
    anyhow::ensure!(output.status.success(), "{}: {}", program, String::from_utf8_lossy(&output.stderr));
    Ok(())
}

fn stat_space(path: &Path) -> (u64, u64, u64) {
    let Ok(path) = std::ffi::CString::new(path.to_string_lossy().as_bytes()) else { return (0, 0, 0) };
    let mut stats = std::mem::MaybeUninit::<libc::statvfs>::uninit();
    if unsafe { libc::statvfs(path.as_ptr(), stats.as_mut_ptr()) } != 0 { return (0, 0, 0); }
    let stats = unsafe { stats.assume_init() };
    let block = stats.f_frsize as u64;
    (stats.f_blocks as u64 * block, stats.f_bavail as u64 * block, (stats.f_blocks as u64).saturating_sub(stats.f_bfree as u64) * block)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn protects_all_partitions_on_system_disk_and_excludes_virtual_disks() {
        let disk: BlockDevice = serde_json::from_str(r#"{"name":"mmcblk1","path":"/dev/mmcblk1","type":"disk","size":1024,"fstype":null,"mountpoint":null,"ro":false,"rm":false,"model":null,"children":[{"name":"mmcblk1p1","path":"/dev/mmcblk1p1","type":"part","size":512,"fstype":"ext4","mountpoint":"/","ro":false,"rm":false,"model":null},{"name":"mmcblk1p2","path":"/dev/mmcblk1p2","type":"part","size":512,"fstype":null,"mountpoint":null,"ro":false,"rm":false,"model":null}]}"#).unwrap();
        assert!(tree_protected(&disk, Path::new("/var/lib/streambox")));
        let mut result = Vec::new(); collect(&disk, &disk, true, &mut result);
        assert_eq!(result.len(), 2);
        assert!(result.iter().all(|disk| disk.protected && !disk.can_format));
        assert_eq!(result[1].available_bytes, None);
    }
    #[test]
    fn service_data_mount_is_protected() {
        let disk: BlockDevice = serde_json::from_str(r#"{"name":"sda1","path":"/dev/sda1","type":"part","size":512,"fstype":"ext4","mountpoint":"/mnt/data","ro":false,"rm":true,"model":null}"#).unwrap();
        assert!(tree_protected(&disk, Path::new("/mnt/data/streambox")));
        assert!(!tree_protected(&disk, Path::new("/var/lib/streambox")));
    }
}
