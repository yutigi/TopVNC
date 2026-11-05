//! The built web app (`web/dist`), served by the gateway itself on the Quick
//! Tunnel route, where no Worker serves it. Only a fixed list of files is
//! reachable, each with a fixed content type and the security headers of
//! `web/_headers`. The files are read once at start, so a gateway that starts
//! always serves a complete page.

use axum::{
    body::Bytes,
    http::{HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
};
use std::path::Path;

/// URL path, file name in `web/dist`, and content type. Nothing else is
/// served.
pub const PAGE_FILES: [(&str, &str, &str); 6] = [
    ("/", "index.html", "text/html; charset=utf-8"),
    ("/index.html", "index.html", "text/html; charset=utf-8"),
    ("/app.js", "app.js", "text/javascript; charset=utf-8"),
    (
        "/session-worker.js",
        "session-worker.js",
        "text/javascript; charset=utf-8",
    ),
    ("/style.css", "style.css", "text/css; charset=utf-8"),
    (
        "/topvnc_web_bg.wasm",
        "topvnc_web_bg.wasm",
        "application/wasm",
    ),
];

/// The headers `web/_headers` gives every page file on the Worker route; a
/// test keeps the two equal.
pub const PAGE_HEADERS: [(&str, &str); 4] = [
    (
        "content-security-policy",
        "default-src 'self'; script-src 'self' 'wasm-unsafe-eval'; style-src 'self' 'unsafe-inline'; connect-src 'self'; img-src 'self' data: blob:; worker-src 'self' blob:; frame-ancestors 'none'; base-uri 'none'; form-action 'self'",
    ),
    ("referrer-policy", "no-referrer"),
    ("x-content-type-options", "nosniff"),
    ("x-frame-options", "DENY"),
];

/// Larger page files are refused; the WebAssembly module is the largest by
/// far, at about a megabyte.
pub const MAX_PAGE_FILE_BYTES: u64 = 16 * 1024 * 1024;

pub struct WebAssets {
    /// The body of each entry of [`PAGE_FILES`], in the same order.
    bodies: Vec<Bytes>,
}

impl WebAssets {
    /// Read every page file from `dir`, normally `web/dist`.
    pub fn load(dir: &Path) -> Result<Self, String> {
        let mut bodies = Vec::with_capacity(PAGE_FILES.len());
        for (_, name, _) in PAGE_FILES {
            let path = dir.join(name);
            let missing = |error: std::io::Error| {
                format!(
                    "cannot read {}: {error}. Build the web app first with `npm run build` in web/.",
                    path.display()
                )
            };
            let size = std::fs::metadata(&path).map_err(missing)?.len();
            if size > MAX_PAGE_FILE_BYTES {
                return Err(format!("{} is too large to serve", path.display()));
            }
            bodies.push(Bytes::from(std::fs::read(&path).map_err(missing)?));
        }
        Ok(Self { bodies })
    }

    /// The response for a GET or HEAD of `path`: a page file, or 404.
    pub fn response(&self, path: &str) -> Response {
        let Some(index) = PAGE_FILES.iter().position(|(route, ..)| *route == path) else {
            return StatusCode::NOT_FOUND.into_response();
        };
        let mut response = self.bodies[index].clone().into_response();
        let headers = response.headers_mut();
        headers.insert(
            header::CONTENT_TYPE,
            HeaderValue::from_static(PAGE_FILES[index].2),
        );
        // A new build must reach the phone without a stale cached page.
        headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-cache"));
        for (name, value) in PAGE_HEADERS {
            headers.insert(name, HeaderValue::from_static(value));
        }
        response
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn page_headers_match_the_worker_route() {
        let file = include_str!("../../../web/_headers");
        let mut lines = file.lines().map(str::trim).filter(|line| !line.is_empty());
        assert_eq!(lines.next(), Some("/*"));
        let listed: Vec<(String, String)> = lines
            .map(|line| {
                let (name, value) = line.split_once(": ").expect("a header line");
                (name.to_ascii_lowercase(), value.to_string())
            })
            .collect();
        let ours: Vec<(String, String)> = PAGE_HEADERS
            .iter()
            .map(|(name, value)| (name.to_string(), value.to_string()))
            .collect();
        assert_eq!(listed, ours);
    }
}
