use std::sync::Arc;

use anyhow::Result;
use base64::{engine::general_purpose::STANDARD, Engine as _};
use tokio::{io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt}, net::{TcpListener, TcpStream, UdpSocket}, sync::{broadcast, RwLock}, task::JoinHandle};

use crate::config::{AudioCodec, RtspConfig, VideoCodec};

const MTU: usize = 1200;

#[derive(Clone, Copy)]
enum Track { Video, Audio }

#[derive(Clone)]
pub struct Hub { tx: broadcast::Sender<Vec<u8>>, audio_tx: broadcast::Sender<Vec<u8>>, params: Arc<RwLock<Params>>, codec: VideoCodec, audio_codec: AudioCodec, sample_rate: u32, channels: u32 }

#[derive(Default, Clone)]
struct Params { vps: Option<Vec<u8>>, sps: Option<Vec<u8>>, pps: Option<Vec<u8>> }

pub async fn start_native(config: RtspConfig, app_config: crate::config::AppConfig, audio_receiver: Option<broadcast::Receiver<Vec<u8>>>, audio_codec: AudioCodec, sample_rate: u32, channels: u32) -> Result<(Hub, crate::media::NativeRuntime, Vec<JoinHandle<()>>)> {
    let listener = TcpListener::bind(format!("{}:{}", config.bind, config.port)).await?;
    let (tx, _) = broadcast::channel(64);
    let (encoded_tx, _) = broadcast::channel(64);
    let (audio_tx, _) = broadcast::channel(64);
    let hub = Hub { tx, audio_tx, params: Arc::new(RwLock::new(Params::default())), codec: app_config.video.codec.clone(), audio_codec: audio_codec.clone(), sample_rate, channels };
    let server_hub = hub.clone();
    let server = tokio::spawn(async move {
        loop {
            let Ok((stream, _)) = listener.accept().await else { break };
            let hub = server_hub.clone();
            let config = config.clone();
            tokio::spawn(async move { let _ = client(stream, config, hub).await; });
        }
    });
    let mut handles = vec![server];
    let encoded_hub = hub.clone();
    let encoded_receiver = encoded_tx.clone();
    handles.push(tokio::spawn(async move { read_native(encoded_receiver.subscribe(), encoded_hub).await; }));
    if let Some(audio_receiver) = audio_receiver { let audio_hub = hub.clone(); handles.push(tokio::spawn(async move { read_native_audio(audio_receiver, audio_hub).await; })); }
    let runtime = crate::media::spawn_native_video(app_config, encoded_tx);
    Ok((hub, runtime, handles))
}

async fn read_native_audio(mut receiver: broadcast::Receiver<Vec<u8>>, hub: Hub) {
    while let Ok(frame) = receiver.recv().await { let _ = hub.audio_tx.send(frame); }
}

async fn read_native(mut receiver: broadcast::Receiver<Vec<u8>>, hub: Hub) {
    let mut pending = Vec::new();
    while let Ok(data) = receiver.recv().await {
        pending.extend_from_slice(&data);
        loop {
            let Some(start) = start_code(&pending) else { pending.clear(); break };
            if start > 0 { pending.drain(..start); }
            let code_len = if pending.starts_with(&[0, 0, 0, 1]) { 4 } else { 3 };
            let Some(relative_next) = find_next_start(&pending[code_len..]) else { break };
            let next = code_len + relative_next;
            let nal = pending[code_len..next].to_vec();
            pending.drain(..next);
            publish(&hub, nal).await;
        }
    }
    if let Some(start) = start_code(&pending) {
        let code_len = if pending[start..].starts_with(&[0, 0, 0, 1]) { 4 } else { 3 };
        if pending.len() > start + code_len { publish(&hub, pending[start + code_len..].to_vec()).await; }
    }
}

async fn publish(hub: &Hub, nal: Vec<u8>) {
    if nal.is_empty() { return; }
    let nal_type = match hub.codec { VideoCodec::H264 => nal[0] & 0x1f, VideoCodec::H265 if nal.len() >= 2 => (nal[0] >> 1) & 0x3f, VideoCodec::H265 => return };
    match (hub.codec.clone(), nal_type) {
        (VideoCodec::H264, 7) | (VideoCodec::H265, 33) => hub.params.write().await.sps = Some(nal.clone()),
        (VideoCodec::H264, 8) | (VideoCodec::H265, 34) => hub.params.write().await.pps = Some(nal.clone()),
        (VideoCodec::H265, 32) => hub.params.write().await.vps = Some(nal.clone()),
        _ => {}
    }
    let _ = hub.tx.send(nal);
}

async fn client(mut stream: TcpStream, config: RtspConfig, hub: Hub) -> Result<()> {
    let mut request = read_request(&mut stream).await?;
    loop {
        let method = request.split_whitespace().next().unwrap_or_default();
        let cseq = header(&request, "CSeq").unwrap_or_else(|| "1".into());
        let uri = request.split_whitespace().nth(1).unwrap_or_default();
        if uri != "*" && !uri.contains(&config.path) { write_response(&mut stream, &cseq, 404, "Not Found", "").await?; return Ok(()); }
        if !authorized(&request, &config) {
            write_response(&mut stream, &cseq, 401, "Unauthorized", "WWW-Authenticate: Basic realm=StreamBox\r\n").await?;
            request = read_request(&mut stream).await?;
            continue;
        }
        match method {
            "OPTIONS" => write_response(&mut stream, &cseq, 200, "OK", "Public: OPTIONS, DESCRIBE, SETUP, PLAY, TEARDOWN\r\n").await?,
            "DESCRIBE" => {
                let sdp = make_sdp(&config, &hub).await;
                let extra = format!("Content-Type: application/sdp\r\nContent-Base: {}\r\n", uri);
                write_body_response(&mut stream, &cseq, 200, "OK", &extra, &sdp).await?;
            }
            "SETUP" => {
                let audio_track = uri.contains("streamid=1") || uri.to_ascii_lowercase().contains("audio");
                if request.to_ascii_uppercase().contains("RTP/AVP/TCP") {
                    let channels = if audio_track { "2-3" } else { "0-1" };
                    let transport = format!("Transport: RTP/AVP/TCP;unicast;interleaved={channels}\r\nSession: streambox\r\n");
                    write_response(&mut stream, &cseq, 200, "OK", &transport).await?;
                    let (reader, writer) = stream.into_split();
                    return session_tcp_loop(reader, writer, hub, !audio_track, audio_track).await;
                }
                let Some((rtp_port, _rtcp_port)) = client_ports(&request) else { write_response(&mut stream, &cseq, 461, "Unsupported Transport", "").await?; return Ok(()); };
                let peer = stream.peer_addr()?.ip();
                let udp = UdpSocket::bind("0.0.0.0:0").await?;
                udp.connect((peer, rtp_port)).await?;
                let server_port = udp.local_addr()?.port();
                let transport = format!("Transport: RTP/AVP;unicast;client_port={rtp_port}-{};server_port={server_port}-{}\r\nSession: streambox\r\n", rtp_port + 1, server_port + 1);
                write_response(&mut stream, &cseq, 200, "OK", &transport).await?;
                let (reader, writer) = stream.into_split();
                return play_udp_loop(reader, writer, cseq, hub, udp, if audio_track { Track::Audio } else { Track::Video }).await;
            }
            _ => write_response(&mut stream, &cseq, 455, "Method Not Valid in This State", "").await?,
        }
        request = read_request(&mut stream).await?;
    }
}

async fn play_udp_loop<R: AsyncRead + Unpin, W: AsyncWrite + Unpin>(mut reader: R, mut writer: W, _cseq: String, hub: Hub, udp: UdpSocket, track: Track) -> Result<()> {
    let request = read_request(&mut reader).await?;
    if request.starts_with("PLAY") { let cseq = header(&request, "CSeq").unwrap_or_else(|| "1".into()); write_response(&mut writer, &cseq, 200, "OK", "Session: streambox\r\n").await?; }
    let mut video_rx = hub.tx.subscribe(); let mut audio_rx = hub.audio_tx.subscribe(); let mut sequence = 0u16; let mut timestamp = 0u32; let mut control = [0u8; 4096];
    loop {
        tokio::select! {
            result = video_rx.recv(), if matches!(track, Track::Video) => { let nal = match result { Ok(nal) => nal, Err(_) => continue }; for packet in rtp_packets(&nal, sequence, timestamp, &hub.codec) { udp.send(&packet).await?; sequence = sequence.wrapping_add(1); } timestamp = timestamp.wrapping_add(3000); }
            result = audio_rx.recv(), if matches!(track, Track::Audio) => { let frame = match result { Ok(frame) => frame, Err(_) => continue }; udp.send(&audio_rtp_packet(&frame, sequence, timestamp, &hub.audio_codec)).await?; sequence = sequence.wrapping_add(1); timestamp = timestamp.wrapping_add(audio_timestamp_step(&hub.audio_codec)); }
            size = reader.read(&mut control) => { let size = size?; if size == 0 { break; } let request = String::from_utf8_lossy(&control[..size]); if request.starts_with("TEARDOWN") { let cseq = header(&request, "CSeq").unwrap_or_else(|| "1".into()); write_response(&mut writer, &cseq, 200, "OK", "Session: streambox\r\n").await?; break; } }
        }
    }
    Ok(())
}

async fn session_tcp_loop<R: AsyncRead + Unpin, W: AsyncWrite + Unpin>(mut reader: R, mut writer: W, hub: Hub, mut video_setup: bool, mut audio_setup: bool) -> Result<()> {
    let mut playing = false; let mut video_rx = hub.tx.subscribe(); let mut audio_rx = hub.audio_tx.subscribe();
    let mut video_sequence = 0u16; let mut video_timestamp = 0u32; let mut audio_sequence = 0u16; let mut audio_timestamp = 0u32; let mut control = [0u8; 4096];
    loop {
        tokio::select! {
            result = video_rx.recv(), if playing && video_setup => {
                let nal = match result { Ok(nal) => nal, Err(_) => continue };
                for packet in rtp_packets(&nal, video_sequence, video_timestamp, &hub.codec) { write_interleaved(&mut writer, 0, &packet).await?; video_sequence = video_sequence.wrapping_add(1); }
                video_timestamp = video_timestamp.wrapping_add(3000);
            }
            result = audio_rx.recv(), if playing && audio_setup => {
                let frame = match result { Ok(frame) => frame, Err(_) => continue };
                let packet = audio_rtp_packet(&frame, audio_sequence, audio_timestamp, &hub.audio_codec); write_interleaved(&mut writer, 2, &packet).await?;
                audio_sequence = audio_sequence.wrapping_add(1); audio_timestamp = audio_timestamp.wrapping_add(audio_timestamp_step(&hub.audio_codec));
            }
            size = reader.read(&mut control) => {
                let size = size?; if size == 0 { break; }
                let request = String::from_utf8_lossy(&control[..size]).into_owned(); let method = request.split_whitespace().next().unwrap_or_default(); let cseq = header(&request, "CSeq").unwrap_or_else(|| "1".into());
                match method {
                    "SETUP" => { let audio = request.to_ascii_lowercase().contains("streamid=1") || request.to_ascii_lowercase().contains("audio"); if audio { audio_setup = true; } else { video_setup = true; } let channels = if audio { "2-3" } else { "0-1" }; let transport = format!("Transport: RTP/AVP/TCP;unicast;interleaved={channels}\r\nSession: streambox\r\n"); write_response(&mut writer, &cseq, 200, "OK", &transport).await?; }
                    "PLAY" => { playing = true; write_response(&mut writer, &cseq, 200, "OK", "Session: streambox\r\nRTP-Info: url=streamid=0\r\n").await?; }
                    "TEARDOWN" => { write_response(&mut writer, &cseq, 200, "OK", "Session: streambox\r\n").await?; break; }
                    "OPTIONS" | "GET_PARAMETER" | "SET_PARAMETER" => { write_response(&mut writer, &cseq, 200, "OK", "Session: streambox\r\n").await?; }
                    _ => {}
                }
            }
        }
    }
    Ok(())
}

async fn write_interleaved<W: AsyncWrite + Unpin>(writer: &mut W, channel: u8, packet: &[u8]) -> Result<()> { writer.write_all(&[b'$', channel, (packet.len() >> 8) as u8, packet.len() as u8]).await?; writer.write_all(packet).await?; Ok(()) }
fn audio_rtp_packet(frame: &[u8], sequence: u16, timestamp: u32, codec: &AudioCodec) -> Vec<u8> {
    match codec {
        AudioCodec::Aac => { let mut payload = vec![((frame.len() >> 5) & 0xff) as u8, ((frame.len() & 0x1f) << 3) as u8]; payload.extend_from_slice(frame); rtp_header(sequence, timestamp, true, 97, &payload) }
        AudioCodec::G711Alaw => rtp_header(sequence, timestamp, true, 8, frame),
        AudioCodec::G711Ulaw => rtp_header(sequence, timestamp, true, 0, frame),
    }
}

fn audio_timestamp_step(codec: &AudioCodec) -> u32 { if matches!(codec, AudioCodec::Aac) { 1024 } else { 160 } }

async fn play_loop<R: AsyncRead + Unpin, W: AsyncWrite + Unpin>(mut reader: R, mut writer: W, _cseq: String, hub: Hub) -> Result<()> {
    let mut request = read_request(&mut reader).await?;
    if request.starts_with("PLAY") {
        let cseq = header(&request, "CSeq").unwrap_or_else(|| "1".into());
        write_response(&mut writer, &cseq, 200, "OK", "Session: streambox\r\nRTP-Info: url=streamid=0\r\n").await?;
    }
    let mut rx = hub.tx.subscribe();
    let mut sequence = 0u16;
    let mut timestamp = 0u32;
    let mut control = [0u8; 4096];
    loop {
        tokio::select! {
            result = rx.recv() => {
                let nal = match result { Ok(nal) => nal, Err(_) => continue };
                for packet in rtp_packets(&nal, sequence, timestamp, &hub.codec) {
                    sequence = sequence.wrapping_add(1);
                    writer.write_all(&[b'$', 0, (packet.len() >> 8) as u8, packet.len() as u8]).await?;
                    writer.write_all(&packet).await?;
                }
                timestamp = timestamp.wrapping_add(3000);
            }
            size = reader.read(&mut control) => {
                let size = size?;
                if size == 0 { break; }
                request = String::from_utf8_lossy(&control[..size]).into_owned();
                if request.starts_with("TEARDOWN") { let cseq = header(&request, "CSeq").unwrap_or_else(|| "1".into()); write_response(&mut writer, &cseq, 200, "OK", "Session: streambox\r\n").await?; break; }
            }
        }
    }
    Ok(())
}

fn rtp_packets(nal: &[u8], sequence: u16, timestamp: u32, codec: &VideoCodec) -> Vec<Vec<u8>> {
    let payload_type = if matches!(codec, VideoCodec::H264) { 96 } else { 98 };
    let header_size = if matches!(codec, VideoCodec::H264) { 1 } else { 2 };
    if nal.len() <= MTU { return vec![rtp_header(sequence, timestamp, true, payload_type, nal)]; }
    let mut result = Vec::new(); let mut offset = header_size; let mut seq = sequence;
    while offset < nal.len() {
        let end = (offset + MTU - header_size - 1).min(nal.len());
        let mut payload = if matches!(codec, VideoCodec::H264) { vec![(nal[0] & 0xe0) | 28, nal[0] & 0x1f] } else { vec![(nal[0] & 0x81) | (49 << 1), nal[1], nal[0] & 0x7e] };
        if offset == header_size { payload[header_size] |= 0x80; }
        if end == nal.len() { payload[header_size] |= 0x40; }
        payload.extend_from_slice(&nal[offset..end]);
        result.push(rtp_header(seq, timestamp, end == nal.len(), payload_type, &payload));
        seq = seq.wrapping_add(1); offset = end;
    }
    result
}

fn rtp_header(sequence: u16, timestamp: u32, marker: bool, payload_type: u8, payload: &[u8]) -> Vec<u8> { let mut packet = vec![0u8; 12 + payload.len()]; packet[0] = 0x80; packet[1] = payload_type | if marker { 0x80 } else { 0 }; packet[2..4].copy_from_slice(&sequence.to_be_bytes()); packet[4..8].copy_from_slice(&timestamp.to_be_bytes()); packet[8..12].copy_from_slice(&0x53544258u32.to_be_bytes()); packet[12..].copy_from_slice(payload); packet }

async fn make_sdp(config: &RtspConfig, hub: &Hub) -> String {
    let p = hub.params.read().await;
    let (payload, codec, fmtp) = match hub.codec {
        VideoCodec::H264 => (96, "H264", match (&p.sps, &p.pps) { (Some(sps), Some(pps)) => format!("a=fmtp:96 packetization-mode=1;sprop-parameter-sets={},{}\r\n", b64(sps), b64(pps)), _ => "a=fmtp:96 packetization-mode=1\r\n".into() }),
        VideoCodec::H265 => (98, "H265", match (&p.vps, &p.sps, &p.pps) { (Some(vps), Some(sps), Some(pps)) => format!("a=fmtp:98 sprop-vps={};sprop-sps={};sprop-pps={}\r\n", b64(vps), b64(sps), b64(pps)), _ => String::new() }),
    };
    let audio = if hub.sample_rate > 0 && hub.channels > 0 {
        match hub.audio_codec {
            AudioCodec::Aac => format!("m=audio 0 RTP/AVP 97\r\nc=IN IP4 0.0.0.0\r\na=rtpmap:97 MPEG4-GENERIC/{}/{}\r\na=fmtp:97 streamtype=5;profile-level-id=15;mode=AAC-hbr;config={};SizeLength=13;IndexLength=3;IndexDeltaLength=3\r\na=control:streamid=1\r\n", hub.sample_rate, hub.channels, aac_config(hub.sample_rate, hub.channels)),
            AudioCodec::G711Alaw => "m=audio 0 RTP/AVP 8\r\nc=IN IP4 0.0.0.0\r\na=rtpmap:8 PCMA/8000/1\r\na=control:streamid=1\r\n".into(),
            AudioCodec::G711Ulaw => "m=audio 0 RTP/AVP 0\r\nc=IN IP4 0.0.0.0\r\na=rtpmap:0 PCMU/8000/1\r\na=control:streamid=1\r\n".into(),
        }
    } else { String::new() };
    format!("v=0\r\no=- 0 0 IN IP4 0.0.0.0\r\ns={}\r\nt=0 0\r\na=control:*\r\nm=video 0 RTP/AVP {}\r\nc=IN IP4 0.0.0.0\r\na=rtpmap:{} {}/90000\r\n{}a=control:streamid=0\r\n{}", config.path, payload, payload, codec, fmtp, audio)
}

fn aac_config(sample_rate: u32, channels: u32) -> String { let index = match sample_rate { 96000 => 0, 88200 => 1, 64000 => 2, 48000 => 3, 44100 => 4, 32000 => 5, 24000 => 6, 22050 => 7, 16000 => 8, 12000 => 9, 11025 => 10, 8000 => 11, _ => 3 }; let value = (2u16 << 11) | ((index as u16) << 7) | ((channels.min(7) as u16) << 3); format!("{value:04x}") }

async fn read_request<R: AsyncRead + Unpin>(reader: &mut R) -> Result<String> { let mut data = Vec::new(); let mut buf = [0u8; 2048]; loop { let n = reader.read(&mut buf).await?; if n == 0 { anyhow::bail!("RTSP client disconnected") } data.extend_from_slice(&buf[..n]); if data.windows(4).any(|x| x == b"\r\n\r\n") { return Ok(String::from_utf8_lossy(&data).into_owned()); } if data.len() > 65536 { anyhow::bail!("RTSP request too large") } } }
fn header(request: &str, name: &str) -> Option<String> { request.lines().find_map(|line| line.split_once(':').filter(|(key, _)| key.eq_ignore_ascii_case(name)).map(|(_, value)| value.trim().to_owned())) }
fn client_ports(request: &str) -> Option<(u16, u16)> { let transport = header(request, "Transport")?; let ports = transport.split(';').find_map(|part| part.strip_prefix("client_port="))?; let mut values = ports.split('-').map(|value| value.parse().ok()); Some((values.next()??, values.next()??)) }
async fn write_response<W: AsyncWrite + Unpin>(stream: &mut W, cseq: &str, code: u16, reason: &str, headers: &str) -> Result<()> { let message = format!("RTSP/1.0 {} {}\r\nCSeq: {}\r\n{}Content-Length: 0\r\n\r\n", code, reason, cseq, headers); stream.write_all(message.as_bytes()).await?; Ok(()) }
async fn write_body_response<W: AsyncWrite + Unpin>(stream: &mut W, cseq: &str, code: u16, reason: &str, headers: &str, body: &str) -> Result<()> { let message = format!("RTSP/1.0 {} {}\r\nCSeq: {}\r\n{}Content-Length: {}\r\n\r\n{}", code, reason, cseq, headers, body.len(), body); stream.write_all(message.as_bytes()).await?; Ok(()) }
fn start_code(data: &[u8]) -> Option<usize> { data.windows(3).position(|x| x == [0, 0, 1]).or_else(|| data.windows(4).position(|x| x == [0, 0, 0, 1])) }
fn find_next_start(data: &[u8]) -> Option<usize> { start_code(data) }
fn b64(data: &[u8]) -> String { const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/"; let mut out = String::new(); for c in data.chunks(3) { let a = c[0] as u32; let b = c.get(1).copied().unwrap_or(0) as u32; let d = c.get(2).copied().unwrap_or(0) as u32; out.push(T[(a >> 2) as usize & 63] as char); out.push(T[((a << 4 | b >> 4) as usize) & 63] as char); out.push(if c.len() > 1 { T[((b << 2 | d >> 6) as usize) & 63] as char } else { '=' }); out.push(if c.len() > 2 { T[d as usize & 63] as char } else { '=' }); } out }

fn authorized(request: &str, config: &RtspConfig) -> bool {
    let Some((username, password)) = config.username.as_ref().zip(config.password.as_ref()) else { return true };
    let Some(value) = header(request, "Authorization") else { return false };
    let Some(encoded) = value.strip_prefix("Basic ") else { return false };
    let Ok(decoded) = STANDARD.decode(encoded.trim()) else { return false };
    decoded == format!("{}:{}", username, password).as_bytes()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn g711_rtp_uses_static_payload_types_and_20ms_timestamps() {
        let frame = vec![0xd5; 160];
        let alaw = audio_rtp_packet(&frame, 1, 160, &AudioCodec::G711Alaw);
        let ulaw = audio_rtp_packet(&frame, 2, 320, &AudioCodec::G711Ulaw);
        assert_eq!(alaw[1] & 0x7f, 8);
        assert_eq!(ulaw[1] & 0x7f, 0);
        assert_eq!(&alaw[12..], frame.as_slice());
        assert_eq!(audio_timestamp_step(&AudioCodec::G711Alaw), 160);
    }
}
