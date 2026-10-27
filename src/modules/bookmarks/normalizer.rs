use sha2::{Digest, Sha256};
use url::Url;

pub struct NormalizedUrl {
    pub raw_url: String,
    pub normalized_url: String,
    pub url_hash: Vec<u8>,
    pub domain: String,
}

pub fn normalize_url(raw: &str) -> Result<NormalizedUrl, String> {
    let mut parsed = Url::parse(raw.trim()).map_err(|e| format!("Invalid URL: {}", e))?;

    let host = parsed
        .host_str()
        .ok_or_else(|| "URL has no host".to_string())?
        .to_lowercase();
    let domain = host.trim_start_matches("www.").to_string();

    // Strip common tracking query params
    let tracking_keys = [
        "utm_source",
        "utm_medium",
        "utm_campaign",
        "utm_term",
        "utm_content",
        "fbclid",
        "gclid",
        "ref",
        "_ga",
    ];

    let pairs: Vec<(String, String)> = parsed
        .query_pairs()
        .filter(|(k, _)| !tracking_keys.contains(&k.as_ref()))
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();

    if pairs.is_empty() {
        parsed.set_query(None);
    } else {
        parsed.query_pairs_mut().clear().extend_pairs(pairs.iter());
    }

    parsed.set_fragment(None);

    let mut normalized_str = parsed.to_string();
    if normalized_str.ends_with('/') {
        normalized_str.pop();
    }

    let mut hasher = Sha256::new();
    hasher.update(normalized_str.as_bytes());
    let url_hash = hasher.finalize().to_vec();

    Ok(NormalizedUrl {
        raw_url: raw.to_string(),
        normalized_url: normalized_str,
        url_hash,
        domain,
    })
}
