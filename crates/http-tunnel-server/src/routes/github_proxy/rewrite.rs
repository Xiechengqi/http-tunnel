use super::matcher;
use reqwest::Url;

pub(crate) fn blob_to_raw(value: &str) -> String {
    value.replacen("/blob/", "/raw/", 1)
}

pub(crate) fn jsdelivr_url(value: &str) -> Option<String> {
    let url = Url::parse(value).ok()?;
    let host = url.host_str()?.to_ascii_lowercase();
    let segments = url.path_segments()?.collect::<Vec<_>>();
    let rewritten = match host.as_str() {
        "github.com" if segments.len() >= 5 && matches!(segments[2], "blob" | "raw") => {
            build_jsdelivr(&segments[0], &segments[1], &segments[3], &segments[4..])
        }
        "raw.githubusercontent.com" | "raw.github.com" if segments.len() >= 4 => {
            build_jsdelivr(&segments[0], &segments[1], &segments[2], &segments[3..])
        }
        _ => return None,
    };
    Some(with_query(rewritten, url.query()))
}

pub(crate) fn proxied_location(
    location: &str,
    current_upstream_url: &Url,
    route_prefix: &str,
) -> Option<String> {
    let resolved = Url::parse(location)
        .or_else(|_| current_upstream_url.join(location))
        .ok()?;
    matcher::match_url(&resolved)?;
    Some(format!("{route_prefix}/{}", resolved.as_str()))
}

fn build_jsdelivr(owner: &str, repo: &str, reference: &str, path: &[&str]) -> String {
    let path = path.join("/");
    format!("https://cdn.jsdelivr.net/gh/{owner}/{repo}@{reference}/{path}")
}

fn with_query(mut value: String, query: Option<&str>) -> String {
    if let Some(query) = query.filter(|query| !query.is_empty()) {
        value.push('?');
        value.push_str(query);
    }
    value
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rewrites_blob_to_raw() {
        assert_eq!(
            blob_to_raw("https://github.com/owner/repo/blob/main/src/lib.rs"),
            "https://github.com/owner/repo/raw/main/src/lib.rs"
        );
        assert_eq!(
            blob_to_raw("https://github.com/owner/repo/raw/main/src/lib.rs"),
            "https://github.com/owner/repo/raw/main/src/lib.rs"
        );
    }

    #[test]
    fn rewrites_branch_files_to_jsdelivr() {
        assert_eq!(
            jsdelivr_url("https://github.com/owner/repo/blob/main/src/lib.rs").as_deref(),
            Some("https://cdn.jsdelivr.net/gh/owner/repo@main/src/lib.rs")
        );
        assert_eq!(
            jsdelivr_url("https://raw.githubusercontent.com/owner/repo/main/src/lib.rs?raw=1")
                .as_deref(),
            Some("https://cdn.jsdelivr.net/gh/owner/repo@main/src/lib.rs?raw=1")
        );
        assert!(jsdelivr_url("https://github.com/owner/repo/releases/download/v1/app").is_none());
    }

    #[test]
    fn rewrites_allowed_locations_to_proxy_path() {
        let current = Url::parse("https://github.com/owner/repo/releases/download/v1/app").unwrap();
        assert_eq!(
            proxied_location(
                "https://github.com/owner/repo/archive/main.zip",
                &current,
                "/gh"
            )
            .as_deref(),
            Some("/gh/https://github.com/owner/repo/archive/main.zip")
        );
        assert_eq!(
            proxied_location("/owner/repo/archive/main.zip", &current, "/gh").as_deref(),
            Some("/gh/https://github.com/owner/repo/archive/main.zip")
        );
        assert!(proxied_location("https://example.com/file", &current, "/gh").is_none());
    }
}
