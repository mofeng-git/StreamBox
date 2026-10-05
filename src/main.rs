mod api;
mod capture;
mod config;
mod device;
mod media;
mod outputs;
mod onvif;
mod rtsp;
mod recording;
mod state;
mod static_files;
mod system;
mod storage;

use std::{net::SocketAddr, path::PathBuf, sync::Arc};

use anyhow::Context;
use axum::{extract::DefaultBodyLimit, middleware, routing::get, Router};
use base64::Engine as _;
use clap::Parser;
use tokio::signal;
use tower_http::{cors::CorsLayer, trace::TraceLayer};
use tracing::info;

use crate::{config::ConfigStore, state::AppState};

#[derive(Debug, Parser)]
#[command(name = "streambox", version, about = "USB capture and recording service")]
struct Args {
    #[arg(long, default_value = "/var/lib/streambox")]
    data_dir: PathBuf,
    #[arg(long, default_value = "0.0.0.0")]
    bind: String,
    #[arg(long, default_value_t = 8090)]
    port: u16,
    #[arg(long)]
    tls_cert: Option<PathBuf>,
    #[arg(long)]
    tls_key: Option<PathBuf>,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let _ = rustls::crypto::ring::default_provider().install_default();
    tracing_subscriber::fmt()
        .with_env_filter(std::env::var("RUST_LOG").unwrap_or_else(|_| "info".into()))
        .init();

    let args = Args::parse();
    tokio::fs::create_dir_all(&args.data_dir)
        .await
        .with_context(|| format!("create data directory {}", args.data_dir.display()))?;

    let config = ConfigStore::load(args.data_dir.join("config.toml")).await?;
    let state = AppState::new(config, args.data_dir.clone()).await?;
    let state = Arc::new(state);
    state.start_background_tasks();

    let api = Router::new()
        .route("/api/health", get(api::health))
        .route("/api/status", get(api::status))
        .route("/api/login", axum::routing::post(api::login))
        .route("/api/preview", get(api::preview))
        .route("/api/logs", get(api::logs))
        .route("/api/system/ota", get(api::ota_status).put(api::ota_upload))
        .route("/api/system/ota/apply", axum::routing::post(api::ota_apply))
        .route("/api/channels", get(api::channels))
        .route("/api/system", get(api::system_info))
        .route("/api/storage", get(api::storage_info))
        .route("/api/storage/action", axum::routing::post(api::storage_action))
        .route("/api/storage/smart", get(api::storage_smart))
        .route("/api/config", get(api::get_config).put(api::put_config))
        .route("/api/devices/video", get(api::video_devices))
        .route("/api/devices/audio", get(api::audio_devices))
        .route("/api/pipeline/start", axum::routing::post(api::start_pipeline))
        .route("/api/pipeline/stop", axum::routing::post(api::stop_pipeline))
        .route("/api/outputs/start", axum::routing::post(api::start_outputs))
        .route("/api/outputs/stop", axum::routing::post(api::stop_outputs))
        .route("/api/channels/{id}/start", axum::routing::post(api::start_channel))
        .route("/api/channels/{id}/stop", axum::routing::post(api::stop_channel))
        .route("/api/channels/{id}/recordings", get(api::channel_recordings))
        .route("/api/channels/{id}/recordings/{name}/download", get(api::download_channel_recording))
        .route("/api/channels/{id}/recordings/{name}", axum::routing::delete(api::delete_channel_recording))
        .route("/api/recording/start", axum::routing::post(api::start_recording))
        .route("/api/recording/stop", axum::routing::post(api::stop_recording))
        .route("/api/recording/policy", axum::routing::post(api::resume_recording_policy))
        .route("/api/recordings", get(api::recordings))
        .route("/api/recordings/{name}/download", get(api::download_recording))
        .route("/api/recordings/{name}/preview", get(api::recording_preview).post(api::prepare_recording_preview))
        .route("/api/recordings/{name}", axum::routing::delete(api::delete_recording))
        .route("/api/ws/status", get(api::status_ws))
        .layer(middleware::from_fn_with_state(state.clone(), api_auth))
        .layer(DefaultBodyLimit::max(128 * 1024 * 1024))
        .with_state(state.clone());
    let app = api
        .fallback(static_files::handler)
        .layer(CorsLayer::permissive())
        .layer(TraceLayer::new_for_http())
        ;

    anyhow::ensure!(args.tls_cert.is_none() == args.tls_key.is_none(), "--tls-cert and --tls-key must be supplied together");
    let address: SocketAddr = format!("{}:{}", args.bind, args.port).parse()?;
    info!(%address, "StreamBox listening");
    if let (Some(cert), Some(key)) = (args.tls_cert, args.tls_key) {
        let tls = axum_server::tls_rustls::RustlsConfig::from_pem_file(cert, key).await?;
        axum_server::bind_rustls(address, tls).serve(app.into_make_service()).await?;
    } else {
        let listener = tokio::net::TcpListener::bind(address).await?;
        axum::serve(listener, app).with_graceful_shutdown(shutdown_signal()).await?;
    }
    state.shutdown().await;
    Ok(())
}

async fn api_auth(axum::extract::State(state): axum::extract::State<state::SharedState>, request: axum::http::Request<axum::body::Body>, next: middleware::Next) -> axum::response::Response {
    let path = request.uri().path();
    let security = state.config.get().await.security;
    if path == "/api/health" || path == "/api/login" || (matches!(*request.method(), axum::http::Method::GET | axum::http::Method::HEAD) && path.starts_with("/api/recordings/") && path.ends_with("/preview")) { return next.run(request).await; }
    let expected = format!("{}:{}", security.username, security.password);
    let authorized = request.headers().get(axum::http::header::AUTHORIZATION).and_then(|value| value.to_str().ok()).and_then(|value| value.strip_prefix("Basic ")).and_then(|value| base64::engine::general_purpose::STANDARD.decode(value).ok()).and_then(|value| String::from_utf8(value).ok()).is_some_and(|value| value == expected);
    if authorized { next.run(request).await } else { axum::response::Response::builder().status(axum::http::StatusCode::UNAUTHORIZED).header(axum::http::header::WWW_AUTHENTICATE, "Basic realm=StreamBox").body(axum::body::Body::from("Unauthorized")).unwrap() }
}

async fn shutdown_signal() {
    let ctrl_c = async {
        signal::ctrl_c().await.expect("install Ctrl-C handler");
    };
    #[cfg(unix)]
    let terminate = async {
        signal::unix::signal(signal::unix::SignalKind::terminate())
            .expect("install SIGTERM handler")
            .recv()
            .await;
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();
    tokio::select! { _ = ctrl_c => {}, _ = terminate => {} }
}
