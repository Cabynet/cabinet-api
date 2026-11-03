//! HMAC signatures for /v1/bookmarks/proxy-image, so the proxy only fetches URLs this API handed out.
use hmac::{Hmac, Mac};
use rand::RngCore;
use sha2::Sha256;
use std::sync::OnceLock;

fn key() -> &'static [u8] {
    static KEY: OnceLock<Vec<u8>> = OnceLock::new();
    KEY.get_or_init(|| match std::env::var("PROXY_SIGNING_KEY") {
        Ok(k) if !k.is_empty() => k.into_bytes(),
        _ => {
            tracing::warn!("PROXY_SIGNING_KEY not set: using a random per-process key, signed proxy URLs break on restart");
            let mut k = vec![0u8; 32];
            rand::thread_rng().fill_bytes(&mut k);
            k
        }
    })
}

/// Fails boot in production (https API_URL) when the key is missing; dev falls back to a random key.
pub fn init() -> Result<(), anyhow::Error> {
    let is_prod = std::env::var("API_URL").is_ok_and(|u| u.starts_with("https://"));
    if is_prod && std::env::var("PROXY_SIGNING_KEY").map_or(true, |k| k.is_empty()) {
        anyhow::bail!("PROXY_SIGNING_KEY is required when API_URL is https");
    }
    key();
    Ok(())
}

fn mac(message: &str) -> Hmac<Sha256> {
    let mut mac = Hmac::<Sha256>::new_from_slice(key()).expect("HMAC accepts any key length");
    mac.update(message.as_bytes());
    mac
}

pub fn sign(url: &str) -> String {
    hex::encode(mac(url).finalize().into_bytes())
}

pub fn verify(url: &str, sig: &str) -> bool {
    hex::decode(sig).is_ok_and(|bytes| mac(url).verify_slice(&bytes).is_ok())
}

// Page signatures are domain-separated so an image signature cannot unlock /proxy-page (and vice versa).
pub fn sign_page(url: &str) -> String {
    sign(&format!("page:{url}"))
}

pub fn verify_page(url: &str, sig: &str) -> bool {
    verify(&format!("page:{url}"), sig)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signature_binds_to_the_exact_url() {
        let sig = sign("https://example.com/a.png");
        assert!(verify("https://example.com/a.png", &sig));
        assert!(!verify("https://example.com/b.png", &sig));
        assert!(!verify("https://example.com/a.png", "zz"));
        assert!(!verify("https://example.com/a.png", ""));
    }

    #[test]
    fn page_and_image_signatures_are_not_interchangeable() {
        let url = "https://example.com/a";
        assert!(verify_page(url, &sign_page(url)));
        assert!(!verify(url, &sign_page(url)));
        assert!(!verify_page(url, &sign(url)));
    }
}
