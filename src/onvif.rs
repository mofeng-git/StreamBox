use std::sync::Arc;

use anyhow::Result;
use base64::{engine::general_purpose::STANDARD, Engine as _};
use chrono::{Datelike, Timelike};
use sha1::{Digest, Sha1};
use tokio::{io::{AsyncReadExt, AsyncWriteExt}, net::{TcpListener, TcpStream, UdpSocket}, task::JoinHandle};

use crate::config::OnvifConfig;

pub async fn start(config: &OnvifConfig, rtsp_uri: String) -> Result<Vec<JoinHandle<()>>> {
    let listener = TcpListener::bind(format!("{}:{}", config.bind, config.port)).await?;
    let advertised_host = advertised_host(&config.bind).await;
    let device_service = format!("http://{}:{}/onvif/device_service", advertised_host, config.port);
    let media_uri = rtsp_uri.clone();
    let config = Arc::new(config.clone());
    let http_config = config.clone();
    let http = tokio::spawn(async move {
        loop {
            let Ok((stream, _)) = listener.accept().await else { break };
            let config = http_config.clone();
            let media_uri = media_uri.clone();
            tokio::spawn(async move { let _ = handle_http(stream, config, media_uri).await; });
        }
    });
    let discovery_uri = device_service.clone();
    let discovery = tokio::spawn(async move { discovery_loop(discovery_uri).await; });
    Ok(vec![http, discovery])
}

async fn advertised_host(bind: &str) -> String {
    if bind != "0.0.0.0" && bind != "::" { return bind.to_owned(); }
    let Ok(socket) = UdpSocket::bind("0.0.0.0:0").await else { return bind.to_owned() };
    if socket.connect("192.0.2.1:9").await.is_ok() { socket.local_addr().ok().map(|address| address.ip().to_string()).unwrap_or_else(|| bind.to_owned()) } else { bind.to_owned() }
}

async fn handle_http(mut stream: TcpStream, config: Arc<OnvifConfig>, rtsp_uri: String) -> Result<()> {
    let mut data = vec![0u8; 65536];
    let size = stream.read(&mut data).await?;
    let request = String::from_utf8_lossy(&data[..size]);
    let body = request.split_once("\r\n\r\n").map(|(_, body)| body).unwrap_or_default();
    if config.username.is_some() && config.password.is_some() && !authenticated(&request, body, &config) {
            stream.write_all(b"HTTP/1.1 401 Unauthorized\r\nWWW-Authenticate: Basic realm=StreamBox\r\nContent-Length: 0\r\n\r\n").await?;
            return Ok(());
    }
    let host = request.lines().find_map(|line| line.strip_prefix("Host:").or_else(|| line.strip_prefix("host:")).map(str::trim)).unwrap_or_else(|| config.bind.as_str());
    let public_rtsp_uri = rtsp_uri.replace("0.0.0.0", host.split(':').next().unwrap_or(host));
    let device_uri = format!("http://{host}/onvif/device_service");
    let response = soap_response(body, &config.name, &public_rtsp_uri, &device_uri);
    let headers = format!("HTTP/1.1 200 OK\r\nContent-Type: application/soap+xml; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", response.len());
    stream.write_all(headers.as_bytes()).await?;
    stream.write_all(response.as_bytes()).await?;
    Ok(())
}

fn soap_response(body: &str, name: &str, rtsp_uri: &str, device_uri: &str) -> String {
    let content = if body.contains("GetDeviceInformation") {
        "<tds:GetDeviceInformationResponse><tds:Manufacturer>StreamBox</tds:Manufacturer><tds:Model>StreamBox Capture</tds:Model><tds:FirmwareVersion>0.1.0</tds:FirmwareVersion><tds:SerialNumber>streambox</tds:SerialNumber><tds:HardwareId>streambox</tds:HardwareId></tds:GetDeviceInformationResponse>".to_owned()
    } else if body.contains("GetCapabilities") || body.contains("GetServices") {
        format!("<tds:GetCapabilitiesResponse><tds:Capabilities><tt:Device><tt:XAddr>{device_uri}</tt:XAddr></tt:Device><tt:Media><tt:XAddr>{device_uri}</tt:XAddr></tt:Media></tds:Capabilities></tds:GetCapabilitiesResponse>")
    } else if body.contains("GetSystemDateAndTime") {
        let now = chrono::Utc::now();
        format!("<tds:GetSystemDateAndTimeResponse><tds:SystemDateAndTime><tt:UTCDateTime><tt:Time>{:02}</tt:Time><tt:Minute>{:02}</tt:Minute><tt:Hour>{:02}</tt:Hour></tt:UTCDateTime><tt:LocalDateTime><tt:Time>{:02}</tt:Time><tt:Minute>{:02}</tt:Minute><tt:Hour>{:02}</tt:Hour></tt:LocalDateTime><tt:TimeZone><tt:TZ>UTC</tt:TZ></tt:TimeZone><tt:DaylightSavings>false</tt:DaylightSavings></tds:SystemDateAndTime></tds:GetSystemDateAndTimeResponse>", now.day(), now.minute(), now.hour(), now.hour(), now.minute(), now.second())
    } else if body.contains("GetNetworkInterfaces") {
        "<tds:GetNetworkInterfacesResponse><tds:NetworkInterfaces token=\"eth0\"><tt:Enabled>true</tt:Enabled><tt:Info><tt:Name>eth0</tt:Name><tt:HwAddress>000000000000</tt:HwAddress></tt:Info></tds:NetworkInterfaces></tds:GetNetworkInterfacesResponse>".to_owned()
    } else if body.contains("GetVideoEncoderConfigurations") {
        "<trt:GetVideoEncoderConfigurationsResponse><trt:Configurations token=\"encoder_main\"><tt:Name>H264 Main</tt:Name><tt:UseCount>1</tt:UseCount><tt:Encoding>H264</tt:Encoding><tt:Resolution><tt:Width>1920</tt:Width><tt:Height>1080</tt:Height></tt:Resolution><tt:RateControl><tt:FrameRateLimit>30</tt:FrameRateLimit><tt:EncodingInterval>1</tt:EncodingInterval><tt:BitrateLimit>4000</tt:BitrateLimit></tt:RateControl></trt:Configurations></trt:GetVideoEncoderConfigurationsResponse>".to_owned()
    } else if body.contains("GetVideoEncoderConfigurationOptions") {
        "<trt:GetVideoEncoderConfigurationOptionsResponse><trt:Options><tt:QualityRange><tt:Min>1</tt:Min><tt:Max>10</tt:Max></tt:QualityRange><tt:JPEG><tt:ResolutionsAvailable><tt:Width>1920</tt:Width><tt:Height>1080</tt:Height></tt:ResolutionsAvailable></tt:JPEG><tt:H264><tt:ResolutionsAvailable><tt:Width>1920</tt:Width><tt:Height>1080</tt:Height></tt:ResolutionsAvailable><tt:GovLengthRange><tt:Min>1</tt:Min><tt:Max>300</tt:Max></tt:GovLengthRange><tt:FrameRateRange><tt:Min>1</tt:Min><tt:Max>60</tt:Max></tt:FrameRateRange></tt:H264></trt:Options></trt:GetVideoEncoderConfigurationOptionsResponse>".to_owned()
    } else if body.contains("GetProfiles") {
        format!("<trt:GetProfilesResponse><trt:Profiles token=\"profile_main\" fixed=\"true\"><tt:Name>{name}</tt:Name><tt:token>profile_main</tt:token></trt:Profiles></trt:GetProfilesResponse>")
    } else if body.contains("GetStreamUri") {
        format!("<trt:GetStreamUriResponse><trt:MediaUri><tt:Uri>{rtsp_uri}</tt:Uri><tt:InvalidAfterConnect>false</tt:InvalidAfterConnect><tt:InvalidAfterReboot>false</tt:InvalidAfterReboot><tt:Timeout>PT60S</tt:Timeout></trt:MediaUri></trt:GetStreamUriResponse>")
    } else if body.contains("GetVideoSources") {
        "<trt:GetVideoSourcesResponse><trt:VideoSources token=\"video_source_1\"><tt:Framerate>30</tt:Framerate><tt:Resolution><tt:Width>1920</tt:Width><tt:Height>1080</tt:Height></tt:Resolution></trt:VideoSources></trt:GetVideoSourcesResponse>".to_owned()
    } else {
        "<tds:ActionResponse/>".to_owned()
    };
    format!("<?xml version=\"1.0\" encoding=\"UTF-8\"?><s:Envelope xmlns:s=\"http://www.w3.org/2003/05/soap-envelope\" xmlns:tds=\"http://www.onvif.org/ver10/device/wsdl\" xmlns:trt=\"http://www.onvif.org/ver10/media/wsdl\" xmlns:tt=\"http://www.onvif.org/ver10/schema\"><s:Body>{content}</s:Body></s:Envelope>")
}

async fn discovery_loop(device_uri: String) {
    let Ok(socket) = UdpSocket::bind("0.0.0.0:3702").await else { return };
    let _ = socket.join_multicast_v4(std::net::Ipv4Addr::new(239, 255, 255, 250), std::net::Ipv4Addr::UNSPECIFIED);
    let mut buffer = [0u8; 8192];
    while let Ok((size, peer)) = socket.recv_from(&mut buffer).await {
        let request = String::from_utf8_lossy(&buffer[..size]);
        if request.contains("Probe") {
            let response = format!("<?xml version=\"1.0\"?><s:Envelope xmlns:s=\"http://www.w3.org/2003/05/soap-envelope\" xmlns:d=\"http://schemas.xmlsoap.org/ws/2005/04/discovery\" xmlns:dn=\"http://www.onvif.org/ver10/network/wsdl\"><s:Body><d:ProbeMatches><d:ProbeMatch><d:Types>dn:NetworkVideoTransmitter</d:Types><d:Scopes>onvif://www.onvif.org/type/video_encoder</d:Scopes><d:XAddrs>{device_uri}</d:XAddrs><d:MetadataVersion>1</d:MetadataVersion></d:ProbeMatch></d:ProbeMatches></s:Body></s:Envelope>");
            let _ = socket.send_to(response.as_bytes(), peer).await;
        }
    }
}

fn base64_token(value: &str) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let bytes = value.as_bytes();
    let mut result = String::new();
    for chunk in bytes.chunks(3) {
        let first = chunk[0] as u32;
        let second = chunk.get(1).copied().unwrap_or(0) as u32;
        let third = chunk.get(2).copied().unwrap_or(0) as u32;
        result.push(TABLE[((first >> 2) & 63) as usize] as char);
        result.push(TABLE[(((first & 3) << 4) | (second >> 4)) as usize] as char);
        result.push(if chunk.len() > 1 { TABLE[(((second & 15) << 2) | (third >> 6)) as usize] as char } else { '=' });
        result.push(if chunk.len() > 2 { TABLE[(third & 63) as usize] as char } else { '=' });
    }
    result
}

fn authenticated(request: &str, body: &str, config: &OnvifConfig) -> bool {
    let Some((username, password)) = config.username.as_ref().zip(config.password.as_ref()) else { return true };
    let basic = request.lines().find_map(|line| line.strip_prefix("Authorization: Basic ").or_else(|| line.strip_prefix("authorization: Basic "))).is_some_and(|token| token.trim() == base64_token(&format!("{}:{}", username, password)));
    if basic { return true; }
    let Some(wsse_user) = xml_value(body, "Username") else { return false };
    let Some(wsse_password) = xml_value(body, "Password") else { return false };
    let Some(nonce_text) = xml_value(body, "Nonce") else { return false };
    let Some(created) = xml_value(body, "Created") else { return false };
    if wsse_user != *username { return false; }
    let Ok(nonce) = STANDARD.decode(nonce_text) else { return false };
    let mut digest = Sha1::new();
    digest.update(nonce);
    digest.update(created.as_bytes());
    digest.update(password.as_bytes());
    wsse_password == STANDARD.encode(digest.finalize())
}

fn xml_value(body: &str, local_name: &str) -> Option<String> {
    let start = body.find(&format!(":{local_name}>"))? + local_name.len() + 2;
    let end = body[start..].find('<')? + start;
    Some(body[start..end].trim().to_owned())
}
