//! Embedded static asset serving (single-binary web UI).

use axum::extract::Request;
use axum::http::{header, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use rust_embed::RustEmbed;

#[derive(RustEmbed)]
#[folder = "web/dist/"]
struct Assets;

/// Serves embedded assets from memory, falling back to `index.html` for
/// unknown paths (SPA-style) and for the root route.
pub async fn serve_static(request: Request) -> Response {
    let requested = request.uri().path().trim_start_matches('/');
    let path = if requested.is_empty() { "index.html" } else { requested };
    match Assets::get(path).or_else(|| Assets::get("index.html")) {
        Some(file) => {
            let mime = file.metadata.mimetype();
            let content_type =
                HeaderValue::from_str(mime).unwrap_or_else(|_| HeaderValue::from_static(
                    "application/octet-stream",
                ));
            (
                [(header::CONTENT_TYPE, content_type)],
                file.data,
            )
                .into_response()
        }
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

#[cfg(test)]
mod tests {
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use tower::ServiceExt;

    use crate::server::{build_router, AppState};

    fn router() -> axum::Router {
        build_router(AppState::new(std::path::PathBuf::from(".")))
    }

    #[tokio::test]
    async fn serves_index_html_at_root() {
        let response = router()
            .oneshot(
                Request::builder()
                    .uri("/")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let content_type = response.headers()["content-type"].to_str().unwrap();
        assert!(content_type.starts_with("text/html"), "got {content_type}");
        let bytes = axum::body::to_bytes(response.into_body(), 1024 * 1024)
            .await
            .unwrap();
        let html = String::from_utf8_lossy(&bytes);
        assert!(html.contains("<title>Grit</title>"));
    }

    #[tokio::test]
    async fn serves_javascript_bundle() {
        let response = router()
            .oneshot(
                Request::builder()
                    .uri("/app.js")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert!(response.headers()["content-type"]
            .to_str()
            .unwrap()
            .starts_with("text/javascript"));
        let bytes = axum::body::to_bytes(response.into_body(), 1024 * 1024)
            .await
            .unwrap();
        let js = String::from_utf8_lossy(&bytes);
        assert!(js.contains("WebSocket"));
    }

    #[tokio::test]
    async fn unknown_path_falls_back_to_index_html() {
        let response = router()
            .oneshot(
                Request::builder()
                    .uri("/some/unknown/route")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = axum::body::to_bytes(response.into_body(), 1024 * 1024)
            .await
            .unwrap();
        assert!(String::from_utf8_lossy(&bytes).contains("<title>Grit</title>"));
    }
}