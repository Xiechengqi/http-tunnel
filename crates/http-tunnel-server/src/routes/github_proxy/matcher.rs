use reqwest::Url;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum GithubUrlKind {
    ReleaseOrArchive,
    BlobOrRaw,
    GitService,
    RawFile,
    Gist,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct GithubUrlMatch {
    pub kind: GithubUrlKind,
    pub owner: String,
    pub repo: Option<String>,
}

pub(crate) fn normalize_input(input: &str, query: Option<&str>) -> String {
    let mut value = input.trim().trim_start_matches('/').to_string();
    if value.starts_with("https:/") && !value.starts_with("https://") {
        value = format!("https://{}", &value["https:/".len()..]);
    } else if value.starts_with("http:/") && !value.starts_with("http://") {
        value = format!("http://{}", &value["http:/".len()..]);
    } else if !value.starts_with("http://") && !value.starts_with("https://") {
        value = format!("https://{value}");
    }

    if let Some(query) = query.filter(|query| !query.is_empty()) {
        if value.contains('?') {
            value.push('&');
        } else {
            value.push('?');
        }
        value.push_str(query);
    }
    value
}

pub(crate) fn parse_allowed_url(value: &str) -> Option<(Url, GithubUrlMatch)> {
    let url = Url::parse(value).ok()?;
    let matched = match_url(&url)?;
    Some((url, matched))
}

pub(crate) fn match_url(url: &Url) -> Option<GithubUrlMatch> {
    if !matches!(url.scheme(), "http" | "https") {
        return None;
    }
    let host = url.host_str()?.to_ascii_lowercase();
    let segments = url.path_segments()?.collect::<Vec<_>>();
    match host.as_str() {
        "github.com" => match_github_path(&segments),
        "raw.githubusercontent.com" | "raw.github.com" => match_raw_path(&segments),
        "gist.githubusercontent.com" | "gist.github.com" => match_gist_path(&segments),
        _ => None,
    }
}

fn match_github_path(segments: &[&str]) -> Option<GithubUrlMatch> {
    if segments.len() < 3 {
        return None;
    }
    let owner = clean_segment(segments[0])?;
    let repo = clean_segment(segments[1])?;
    let kind = segments[2];
    let kind = if matches!(kind, "releases" | "archive") && segments.len() >= 4 {
        GithubUrlKind::ReleaseOrArchive
    } else if matches!(kind, "blob" | "raw") && segments.len() >= 4 {
        GithubUrlKind::BlobOrRaw
    } else if kind == "info" || kind.starts_with("git-") {
        GithubUrlKind::GitService
    } else {
        return None;
    };
    Some(GithubUrlMatch {
        kind,
        owner,
        repo: Some(repo),
    })
}

fn match_raw_path(segments: &[&str]) -> Option<GithubUrlMatch> {
    if segments.len() < 4 {
        return None;
    }
    Some(GithubUrlMatch {
        kind: GithubUrlKind::RawFile,
        owner: clean_segment(segments[0])?,
        repo: Some(clean_segment(segments[1])?),
    })
}

fn match_gist_path(segments: &[&str]) -> Option<GithubUrlMatch> {
    if segments.len() < 3 {
        return None;
    }
    Some(GithubUrlMatch {
        kind: GithubUrlKind::Gist,
        owner: clean_segment(segments[0])?,
        repo: None,
    })
}

fn clean_segment(value: &str) -> Option<String> {
    (!value.is_empty()).then(|| value.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn matched(value: &str) -> GithubUrlMatch {
        parse_allowed_url(value).expect("allowed url").1
    }

    #[test]
    fn normalizes_proxy_targets() {
        assert_eq!(
            normalize_input("github.com/owner/repo/archive/main.zip", None),
            "https://github.com/owner/repo/archive/main.zip"
        );
        assert_eq!(
            normalize_input(
                "https:/github.com/owner/repo/info/refs",
                Some("service=git-upload-pack")
            ),
            "https://github.com/owner/repo/info/refs?service=git-upload-pack"
        );
    }

    #[test]
    fn matches_supported_github_urls() {
        assert_eq!(
            matched("https://github.com/owner/repo/releases/download/v1/app.tar.gz"),
            GithubUrlMatch {
                kind: GithubUrlKind::ReleaseOrArchive,
                owner: "owner".to_string(),
                repo: Some("repo".to_string()),
            }
        );
        assert_eq!(
            matched("https://github.com/owner/repo/blob/main/src/lib.rs").kind,
            GithubUrlKind::BlobOrRaw
        );
        assert_eq!(
            matched("https://github.com/owner/repo/info/refs?service=git-upload-pack").kind,
            GithubUrlKind::GitService
        );
        assert_eq!(
            matched("https://github.com/owner/repo/git-upload-pack").kind,
            GithubUrlKind::GitService
        );
        assert_eq!(
            matched("https://raw.githubusercontent.com/owner/repo/main/file.txt").kind,
            GithubUrlKind::RawFile
        );
        assert_eq!(
            matched("https://gist.githubusercontent.com/owner/hash/raw/file.txt"),
            GithubUrlMatch {
                kind: GithubUrlKind::Gist,
                owner: "owner".to_string(),
                repo: None,
            }
        );
    }

    #[test]
    fn rejects_non_github_urls() {
        assert!(parse_allowed_url("https://example.com/owner/repo/archive/main.zip").is_none());
        assert!(parse_allowed_url("ftp://github.com/owner/repo/archive/main.zip").is_none());
        assert!(parse_allowed_url("https://github.com/owner/repo/issues/1").is_none());
        assert!(parse_allowed_url("https://github.com/owner/repo/blob").is_none());
        assert!(parse_allowed_url("https://github.com/owner/repo/archive").is_none());
    }
}
