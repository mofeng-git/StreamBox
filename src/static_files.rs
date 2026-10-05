use axum::{body::Body, http::{header, Uri}, response::Response};
use mime_guess::from_path;
use rust_embed::RustEmbed;

#[derive(RustEmbed)]
#[folder = "web/dist/"]
struct Assets;

pub async fn handler(uri: Uri) -> Response {
    let requested = uri.path().trim_start_matches('/');
    let asset_path = if requested.is_empty() || !requested.contains('.') { "index.html" } else { requested };
    let Some(asset) = Assets::get(asset_path) else {
        return Response::builder().status(404).body(Body::from("Not Found")).unwrap();
    };
    let content_type = from_path(asset_path).first_or_octet_stream();
    Response::builder()
        .header(header::CONTENT_TYPE, content_type.as_ref())
        .body(Body::from(asset.data.into_owned()))
        .unwrap()
}
