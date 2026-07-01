use include_dir::{include_dir, Dir};

static PUBLIC_DIR: Dir<'_> = include_dir!("$CARGO_MANIFEST_DIR/public");

#[derive(Debug, Clone, Copy)]
pub struct StaticPage {
    pub content_type: &'static str,
    pub body: &'static [u8],
}

pub fn index_page() -> Option<StaticPage> {
    asset("index.html")
}

pub fn admin_page() -> Option<StaticPage> {
    asset("admin/index.html").or_else(|| asset("admin.html"))
}

pub fn login_page() -> Option<StaticPage> {
    asset("admin/login/index.html").or_else(|| asset("login.html"))
}

pub fn setup_page() -> Option<StaticPage> {
    asset("admin/setup/index.html").or_else(|| asset("setup.html"))
}

pub fn static_asset(path: &str) -> Option<StaticPage> {
    let normalized = path.trim_start_matches('/');
    if normalized.is_empty() || normalized.contains("..") {
        return None;
    }
    asset(normalized)
}

fn asset(path: &str) -> Option<StaticPage> {
    let file = PUBLIC_DIR.get_file(path)?;
    Some(StaticPage {
        content_type: content_type(path),
        body: file.contents(),
    })
}

fn content_type(path: &str) -> &'static str {
    match path.rsplit('.').next().unwrap_or_default() {
        "html" => "text/html; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "js" | "mjs" => "text/javascript; charset=utf-8",
        "json" => "application/json; charset=utf-8",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "webp" => "image/webp",
        "ico" => "image/x-icon",
        "txt" => "text/plain; charset=utf-8",
        "woff" => "font/woff",
        "woff2" => "font/woff2",
        _ => "application/octet-stream",
    }
}
