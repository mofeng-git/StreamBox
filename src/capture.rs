use std::{collections::HashMap, fs::File, os::fd::{AsFd, AsRawFd}, os::unix::fs::OpenOptionsExt, path::{Path, PathBuf}, sync::{Mutex, OnceLock}, time::{Duration, Instant}};

use anyhow::{Context, Result};
use nix::poll::{poll, PollFd, PollFlags, PollTimeout};
use v4l2r::{ioctl, memory::{MemoryType, MmapHandle}, Format, PixelFormat, QueueType};
use v4l2r::ioctl::{QBufPlane, QBuffer, QueryBuffer, V4l2Buffer};

#[derive(Clone)]
pub struct CaptureFrame { pub data: Vec<u8>, pub sequence: u64 }
type LatestFrames = HashMap<PathBuf, (Instant, Format, CaptureFrame)>;
static LATEST: OnceLock<Mutex<LatestFrames>> = OnceLock::new();

pub fn recent_frame(path: &Path) -> Option<(Format, CaptureFrame)> {
    let mut frames = LATEST.get_or_init(|| Mutex::new(HashMap::new())).lock().ok()?;
    frames.retain(|_, (time, _, _)| time.elapsed() < Duration::from_secs(3));
    frames.get(path).map(|(_, format, frame)| (format.clone(), frame.clone()))
}

pub struct V4l2Capture {
    fd: File,
    queue: QueueType,
    mappings: Vec<Vec<v4l2r::ioctl::PlaneMapping>>,
    pub format: Format,
    path: PathBuf,
    last_cached: Option<Instant>,
}

impl V4l2Capture {
    pub fn open(path: &Path, width: u32, height: u32, fps: u32, input_format: &str) -> Result<Self> {
        let mut fd = File::options().read(true).write(true).custom_flags(libc::O_NONBLOCK).open(path).with_context(|| format!("open V4L2 device {}", path.display()))?;
        let queue = QueueType::VideoCapture;
        let fourcc = match input_format.to_ascii_lowercase().as_str() {
            "mjpeg" | "mjpg" => *b"MJPG",
            "yuyv" | "yuyv422" => *b"YUYV",
            "nv12" => *b"NV12",
            other => anyhow::bail!("unsupported V4L2 input format {other}"),
        };
        let pixel_format = PixelFormat::from(&fourcc);
        let requested = Format::from((pixel_format, (width as usize, height as usize)));
        let format: Format = ioctl::s_fmt(&mut fd, (queue, &requested)).map_err(|error| anyhow::anyhow!("set V4L2 format: {error}"))?;
        anyhow::ensure!(format.pixelformat == pixel_format, "采集设备不支持所请求的输入格式");
        let frame_rate = v4l2r::bindings::v4l2_streamparm {
            type_: queue as u32,
            parm: v4l2r::bindings::v4l2_streamparm__bindgen_ty_1 {
                capture: v4l2r::bindings::v4l2_captureparm {
                    timeperframe: v4l2r::bindings::v4l2_fract { numerator: 1, denominator: fps.max(1) },
                    ..Default::default()
                },
            },
        };
        let _ = ioctl::s_parm::<_, v4l2r::bindings::v4l2_streamparm>(&fd, frame_rate);
        let req: v4l2r::bindings::v4l2_requestbuffers = ioctl::reqbufs(&fd, queue, MemoryType::Mmap, 4, v4l2r::ioctl::MemoryConsistency::empty()).map_err(|error| anyhow::anyhow!("request V4L2 buffers: {error}"))?;
        if req.count == 0 { anyhow::bail!("V4L2 returned no capture buffers") }
        let mut mappings = Vec::with_capacity(req.count as usize);
        for index in 0..req.count as usize {
            let query: QueryBuffer = ioctl::querybuf(&fd, queue, index).map_err(|error| anyhow::anyhow!("query V4L2 buffer: {error}"))?;
            let mut planes = Vec::with_capacity(query.planes.len());
            for plane in query.planes { planes.push(ioctl::mmap(&fd, plane.mem_offset, plane.length).map_err(|error| anyhow::anyhow!("map V4L2 buffer: {error}"))?); }
            mappings.push(planes);
        }
        let capture = Self { fd, queue, mappings, format, path: path.to_owned(), last_cached: None };
        for index in 0..capture.mappings.len() { capture.queue_buffer(index as u32)?; }
        ioctl::streamon(&capture.fd, capture.queue).map_err(|error| anyhow::anyhow!("start V4L2 stream: {error}"))?;
        Ok(capture)
    }

    pub fn next(&mut self) -> Result<CaptureFrame> {
        let mut poll_fds = [PollFd::new(self.fd.as_fd(), PollFlags::POLLIN | PollFlags::POLLERR | PollFlags::POLLHUP)];
        if poll(&mut poll_fds, PollTimeout::from(2000u16)).map_err(|error| anyhow::anyhow!("poll V4L2: {error}"))? == 0 { anyhow::bail!("V4L2 capture timeout") }
        let buffer: V4l2Buffer = ioctl::dqbuf(&self.fd, self.queue, MemoryType::Mmap).map_err(|error| anyhow::anyhow!("dequeue V4L2 buffer: {error}"))?;
        let index = buffer.index() as usize;
        let plane = buffer.get_first_plane();
        let bytes = (*plane.bytesused as usize).min(self.mappings[index][0].len());
        let data = self.mappings[index][0].as_ref()[..bytes].to_vec();
        self.queue_buffer(index as u32)?;
        let frame = CaptureFrame { data, sequence: buffer.sequence() as u64 };
        if self.last_cached.is_none_or(|time| time.elapsed() >= Duration::from_secs(1)) {
            let mut frames = LATEST.get_or_init(|| Mutex::new(HashMap::new())).lock().unwrap();
            frames.retain(|_, (time, _, _)| time.elapsed() < Duration::from_secs(3));
            if frames.len() < 32 || frames.contains_key(&self.path) { frames.insert(self.path.clone(), (Instant::now(), self.format.clone(), frame.clone())); }
            self.last_cached = Some(Instant::now());
        }
        Ok(frame)
    }

    fn queue_buffer(&self, index: u32) -> Result<()> {
        let handle = MmapHandle;
        let planes = self.mappings[index as usize].iter().map(|mapping| { let mut plane = QBufPlane::new_from_handle(&handle, 0); plane.0.length = mapping.len() as u32; plane }).collect();
        let mut buffer: QBuffer<MmapHandle> = QBuffer::new(self.queue, index);
        buffer.planes = planes;
        ioctl::qbuf::<_, ()>(&self.fd, buffer).map_err(|error| anyhow::anyhow!("queue V4L2 buffer: {error}"))?;
        Ok(())
    }
}

impl Drop for V4l2Capture {
    fn drop(&mut self) {
        let _ = ioctl::streamoff(&self.fd, self.queue);
        let _ = ioctl::reqbufs::<v4l2r::bindings::v4l2_requestbuffers>(&self.fd, self.queue, MemoryType::Mmap, 0, v4l2r::ioctl::MemoryConsistency::empty());
        let _ = self.fd.as_raw_fd();
    }
}
