## 项目介绍

StreamBox 是一款面向 USB 摄像头和 USB HDMI 采集卡的视频采集与编码软件，适用于 HDMI 编码器、录制盒和嵌入式视频设备。

项目使用 Rust 开发，以 FFmpeg 为媒体处理基础，将 USB 设备输入的音视频编码为实时视频流或本地录像，并通过 Web 界面统一管理。

> 目标：将 USB 摄像头或 USB 视频采集卡输入的音视频，稳定地采集、编码、存储，并通过 RTSP、RTMP、ONVIF 等标准方式提供给其他设备或平台。

## 网页截图

<table width="100%" cellpadding="8">
  <tr>
    <td width="50%" align="center">
      <img src="https://github.com/user-attachments/assets/92da5608-2adb-4689-8b51-6abe49a8d71a" width="100%" />
    </td>
    <td width="50%" align="center">
      <img src="https://github.com/user-attachments/assets/7a1de66d-0f49-4390-9854-08d0c8a049a1" width="100%" />
    </td>
  </tr>
  <tr>
    <td width="50%" align="center">
      <img src="https://github.com/user-attachments/assets/8c2ba6a6-c1ee-4050-b220-700bafefd521" width="100%" />
    </td>
    <td width="50%" align="center">
      <img src="https://github.com/user-attachments/assets/c3190f31-e0c1-471b-af3d-019c0d159787" width="100%" />
    </td>
  </tr>
  <tr>
    <td width="50%" align="center">
      <img src="https://github.com/user-attachments/assets/3837f883-8255-479e-9b75-4101f13f59e5" width="100%" />
    </td>
    <td width="50%" align="center">
      <img src="https://github.com/user-attachments/assets/090a15ab-65e4-46b7-a244-cb7e173a3da3" width="100%" />
    </td>
  </tr>
  <tr>
    <td width="50%" align="center">
      <img src="https://github.com/user-attachments/assets/769b74b8-f918-4d14-8367-6c20cce423e7" width="100%" />
    </td>
    <td width="50%" align="center">
      <img src="https://github.com/user-attachments/assets/07a72cae-cd03-4848-b5a7-9e8d3192f566" width="100%" />
    </td>
  </tr>
</table>

## 主要功能

> 部分功能可能尚未完全实现。

- **设备采集**：支持 V4L2 视频设备和 ALSA 音频设备，提供设备发现、输入选择和画面预览。
- **音视频编码**：支持 H.264/H.265 硬件或软件编码，以及 AAC、G.711 音频编码，可配置分辨率、帧率和码率。
- **网络分发**：提供 RTSP 服务、RTMP 推流和断线重连，通过 ONVIF 提供设备发现和媒体信息。
- **本地录像**：支持 MP4、MPEG-TS，提供手动、自动和定时录像，支持按时间或大小分段及循环清理。
- **存储与文件管理**：管理本地存储和录像文件，支持录像播放、下载和删除。
- **Web 管理**：支持多通道配置、运行状态监控、系统指标、日志查看、账号认证、HTTPS 和 Web 升级。

输入源为 USB 视频采集设备，适合配合 eMMC、SSD、USB 硬盘或 SD 卡进行本地录像。
## 编译方式

当前编译依赖同级 `One-KVM` 目录中的 `hwcodec`、`libyuv` 和 `v4l2r`。ARMv7 构建默认复用 One-KVM 交叉编译基础镜像，在仓库根目录运行 `scripts/build-armv7-docker.sh`，产物为 `dist/streambox`；修改前端后需先在 `web` 目录执行 `npm ci && npm run build`。

## 灵感来源

[[开源] 400元实现 4K 60fps VRR P010 HDR 视频采集编码推流一体机 - streambox项目介绍](https://www.bilibili.com/video/BV1VXQkBBEnF/?)
