use http::{HeaderMap, HeaderName};

pub const HOP_BY_HOP_HEADERS: &[&str] = &[
    "connection",
    "upgrade",
    "keep-alive",
    "proxy-authenticate",
    "proxy-authorization",
    "te",
    "trailer",
    "transfer-encoding",
];

pub fn is_hop_by_hop(name: &HeaderName) -> bool {
    HOP_BY_HOP_HEADERS
        .iter()
        .any(|candidate| name.as_str().eq_ignore_ascii_case(candidate))
}

pub fn filtered_headers(headers: &HeaderMap) -> HeaderMap {
    let dynamic_hop_by_hop = connection_header_names(headers);
    headers
        .iter()
        .filter(|(name, _)| {
            !is_hop_by_hop(name)
                && !dynamic_hop_by_hop
                    .iter()
                    .any(|candidate| name.as_str().eq_ignore_ascii_case(candidate))
        })
        .map(|(name, value)| (name.clone(), value.clone()))
        .collect()
}

fn connection_header_names(headers: &HeaderMap) -> Vec<String> {
    headers
        .get_all("connection")
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|value| value.split(','))
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|value| value.to_ascii_lowercase())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use http::HeaderValue;

    #[test]
    fn hop_by_hop_headers_removed() {
        let mut headers = HeaderMap::new();
        headers.insert("connection", HeaderValue::from_static("upgrade, x-hop"));
        headers.insert("x-hop", HeaderValue::from_static("remove"));
        headers.insert("x-demo", HeaderValue::from_static("1"));

        let filtered = filtered_headers(&headers);
        assert!(!filtered.contains_key("connection"));
        assert!(!filtered.contains_key("x-hop"));
        assert_eq!(filtered.get("x-demo").unwrap(), "1");
    }
}
