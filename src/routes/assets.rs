use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::sync::LazyLock;

use axum::http::HeaderMap;
use axum::http::header::{CACHE_CONTROL, CONTENT_TYPE, ETAG, IF_NONE_MATCH};
use axum::http::{StatusCode, header::HeaderValue};
use axum::response::{IntoResponse, Response};

/// The stylesheet and its one script are compiled into the binary so the
/// service stays a single self-contained artifact with no asset directory to
/// deploy alongside it.
const APP_CSS: &str = include_str!("../../static/app.css");
const APP_JS: &str = include_str!("../../static/app.js");

/// Revalidate on every load: the ETag turns that into a 304 for a few bytes,
/// and an upgraded binary never serves a stale stylesheet from the cache.
const CACHE: &str = "public, max-age=0, must-revalidate";

static CSS_ETAG: LazyLock<String> = LazyLock::new(|| etag(APP_CSS));
static JS_ETAG: LazyLock<String> = LazyLock::new(|| etag(APP_JS));

pub async fn app_css(headers: HeaderMap) -> Response {
    asset(headers, APP_CSS, "text/css; charset=utf-8", &CSS_ETAG)
}

pub async fn app_js(headers: HeaderMap) -> Response {
    asset(headers, APP_JS, "text/javascript; charset=utf-8", &JS_ETAG)
}

fn asset(
    headers: HeaderMap,
    body: &'static str,
    content_type: &'static str,
    tag: &str,
) -> Response {
    let mut response_headers = HeaderMap::new();
    response_headers.insert(CACHE_CONTROL, HeaderValue::from_static(CACHE));
    if let Ok(value) = HeaderValue::from_str(tag) {
        response_headers.insert(ETAG, value);
    }

    if is_fresh(&headers, tag) {
        return (StatusCode::NOT_MODIFIED, response_headers).into_response();
    }

    response_headers.insert(CONTENT_TYPE, HeaderValue::from_static(content_type));
    (response_headers, body).into_response()
}

fn is_fresh(headers: &HeaderMap, tag: &str) -> bool {
    headers
        .get(IF_NONE_MATCH)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value.split(',').any(|candidate| candidate.trim() == tag))
}

fn etag(body: &str) -> String {
    let mut hasher = DefaultHasher::new();
    body.hash(&mut hasher);
    format!("\"{:016x}\"", hasher.finish())
}
