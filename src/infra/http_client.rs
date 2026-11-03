use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use std::time::Duration;
use ipnet::{Ipv4Net, Ipv6Net};
use reqwest::{Client, Response};
use url::Url;
use crate::errors::AppError;

/// Resolves DNS and drops unsafe IPs, so the address checked is the one connected to (closes DNS rebinding).
struct SafeResolver;

impl reqwest::dns::Resolve for SafeResolver {
    fn resolve(&self, name: reqwest::dns::Name) -> reqwest::dns::Resolving {
        Box::pin(async move {
            let addrs: Vec<std::net::SocketAddr> = tokio::net::lookup_host((name.as_str(), 0))
                .await?
                .collect();
            if addrs.iter().any(|a| !SsrfSafeClient::is_ip_safe(a.ip())) {
                return Err("Host resolved to a prohibited IP".into());
            }
            Ok(Box::new(addrs.into_iter()) as reqwest::dns::Addrs)
        })
    }
}

pub struct SsrfSafeClient {
    client: Client,
}

impl SsrfSafeClient {
    pub fn new() -> Result<Self, anyhow::Error> {
        Self::with_timeout(Duration::from_secs(8))
    }

    pub fn with_timeout(timeout: Duration) -> Result<Self, anyhow::Error> {
        let client = Client::builder()
            .timeout(timeout)
            .connect_timeout(Duration::from_secs(3))
            .dns_resolver(std::sync::Arc::new(SafeResolver))
            .redirect(reqwest::redirect::Policy::none()) // We handle redirects manually with SSRF checks!
            .user_agent("Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/131.0.0.0 Safari/537.36")
            .build()?;

        Ok(Self { client })
    }

    pub fn is_ip_safe(ip: IpAddr) -> bool {
        match ip {
            IpAddr::V4(v4) => Self::is_ipv4_safe(v4),
            IpAddr::V6(v6) => Self::is_ipv6_safe(v6),
        }
    }

    fn is_ipv4_safe(ip: Ipv4Addr) -> bool {
        if ip.is_loopback() || ip.is_unspecified() || ip.is_broadcast() {
            return false;
        }

        // RFC 1918 Private Ranges & Link Local & CGNAT
        let blocked_ranges = [
            Ipv4Net::new(Ipv4Addr::new(0, 0, 0, 0), 8).unwrap(),       // Current network
            Ipv4Net::new(Ipv4Addr::new(10, 0, 0, 0), 8).unwrap(),      // Private-Use
            Ipv4Net::new(Ipv4Addr::new(100, 64, 0, 0), 10).unwrap(),   // Shared Address Space (CGNAT)
            Ipv4Net::new(Ipv4Addr::new(127, 0, 0, 0), 8).unwrap(),     // Loopback
            Ipv4Net::new(Ipv4Addr::new(169, 254, 0, 0), 16).unwrap(),  // Link-Local / Cloud Metadata!
            Ipv4Net::new(Ipv4Addr::new(172, 16, 0, 0), 12).unwrap(),   // Private-Use
            Ipv4Net::new(Ipv4Addr::new(192, 0, 0, 0), 24).unwrap(),    // IETF Protocol Assignments
            Ipv4Net::new(Ipv4Addr::new(192, 0, 2, 0), 24).unwrap(),    // Documentation (TEST-NET-1)
            Ipv4Net::new(Ipv4Addr::new(192, 168, 0, 0), 16).unwrap(),  // Private-Use
            Ipv4Net::new(Ipv4Addr::new(198, 18, 0, 0), 15).unwrap(),   // Benchmarking
            Ipv4Net::new(Ipv4Addr::new(198, 51, 100, 0), 24).unwrap(), // Documentation (TEST-NET-2)
            Ipv4Net::new(Ipv4Addr::new(203, 0, 113, 0), 24).unwrap(),  // Documentation (TEST-NET-3)
            Ipv4Net::new(Ipv4Addr::new(224, 0, 0, 0), 4).unwrap(),     // Multicast
            Ipv4Net::new(Ipv4Addr::new(240, 0, 0, 0), 4).unwrap(),     // Reserved for Future Use
        ];

        for range in &blocked_ranges {
            if range.contains(&ip) {
                return false;
            }
        }

        true
    }

    fn is_ipv6_safe(ip: Ipv6Addr) -> bool {
        if ip.is_loopback() || ip.is_unspecified() {
            return false;
        }

        // IPv6 Link-Local, Unique Local (fc00::/7), Multicast (ff00::/8)
        let blocked_ranges = [
            Ipv6Net::new(Ipv6Addr::new(0, 0, 0, 0, 0, 0, 0, 0), 96).unwrap(), // Loopback, unspecified, IPv4-compatible
            Ipv6Net::new(Ipv6Addr::new(0x64, 0xff9b, 0, 0, 0, 0, 0, 0), 96).unwrap(), // NAT64
            Ipv6Net::new(Ipv6Addr::new(0x2002, 0, 0, 0, 0, 0, 0, 0), 16).unwrap(), // 6to4
            Ipv6Net::new(Ipv6Addr::new(0xfe80, 0, 0, 0, 0, 0, 0, 0), 10).unwrap(), // Link-Local
            Ipv6Net::new(Ipv6Addr::new(0xfc00, 0, 0, 0, 0, 0, 0, 0), 7).unwrap(),  // Unique Local (ULA)
            Ipv6Net::new(Ipv6Addr::new(0xff00, 0, 0, 0, 0, 0, 0, 0), 8).unwrap(),  // Multicast
            Ipv6Net::new(Ipv6Addr::new(0x2001, 0xdb8, 0, 0, 0, 0, 0, 0), 32).unwrap(), // Documentation
        ];

        for range in &blocked_ranges {
            if range.contains(&ip) {
                return false;
            }
        }

        // Also check IPv4-mapped IPv6 (::ffff:x.x.x.x)
        if let Some(v4) = ip.to_ipv4_mapped() {
            return Self::is_ipv4_safe(v4);
        }

        true
    }

    pub async fn validate_url_safety(&self, url: &Url) -> Result<(), AppError> {
        let scheme = url.scheme();
        if scheme != "http" && scheme != "https" {
            return Err(AppError::SsrfBlocked(format!("Disallowed scheme: {}", scheme)));
        }

        let host_str = url
            .host_str()
            .ok_or_else(|| AppError::SsrfBlocked("Missing host".to_string()))?;

        // If host is an IP literal (host_str keeps IPv6 brackets, so match on url.host())
        let literal = match url.host() {
            Some(url::Host::Ipv4(ip)) => Some(IpAddr::V4(ip)),
            Some(url::Host::Ipv6(ip)) => Some(IpAddr::V6(ip)),
            _ => None,
        };
        if let Some(ip) = literal {
            if !Self::is_ip_safe(ip) {
                return Err(AppError::SsrfBlocked(format!("Blocked IP address: {}", ip)));
            }
            return Ok(());
        }

        // Resolve DNS
        let port = url.port_or_known_default().unwrap_or(80);
        let addrs: Vec<std::net::SocketAddr> = tokio::net::lookup_host(format!("{}:{}", host_str, port))
            .await
            .map_err(|e| AppError::SsrfBlocked(format!("DNS lookup failed for {}: {}", host_str, e)))?
            .collect();

        if addrs.is_empty() {
            return Err(AppError::SsrfBlocked(format!("No DNS record for {}", host_str)));
        }

        for addr in addrs {
            if !Self::is_ip_safe(addr.ip()) {
                return Err(AppError::SsrfBlocked(format!(
                    "Host {} resolved to prohibited IP: {}",
                    host_str,
                    addr.ip()
                )));
            }
        }

        Ok(())
    }

    /// Fetch URL with max 5 redirects and SSRF validation on every redirect step
    pub async fn get_safe(&self, initial_url: &str) -> Result<Response, AppError> {
        self.get_safe_with_ua(initial_url, None).await
    }

    pub async fn get_safe_with_ua(
        &self,
        initial_url: &str,
        user_agent: Option<&str>,
    ) -> Result<Response, AppError> {
        let mut current_url = Url::parse(initial_url)
            .map_err(|e| AppError::BadRequest(format!("Invalid URL: {}", e)))?;

        let mut redirects_remaining = 5;

        loop {
            self.validate_url_safety(&current_url).await?;

            let mut req = self.client.get(current_url.as_str()).header(
                reqwest::header::ACCEPT,
                "text/html,application/xhtml+xml,application/xml;q=0.9,*/*;q=0.8",
            );
            if let Some(ua) = user_agent {
                req = req.header(reqwest::header::USER_AGENT, ua);
            }

            let resp = req
                .send()
                .await
                .map_err(|e| AppError::BadRequest(format!("HTTP request failed: {}", e)))?;

            if resp.status().is_redirection() {
                if redirects_remaining == 0 {
                    return Err(AppError::BadRequest("Too many redirects".to_string()));
                }
                redirects_remaining -= 1;

                if let Some(loc_header) = resp.headers().get(reqwest::header::LOCATION) {
                    let loc_str = loc_header
                        .to_str()
                        .map_err(|_| AppError::BadRequest("Invalid redirect location header".to_string()))?;
                    current_url = current_url
                        .join(loc_str)
                        .map_err(|e| AppError::BadRequest(format!("Invalid redirect URL: {}", e)))?;
                    continue;
                } else {
                    return Err(AppError::BadRequest("Redirect response missing Location header".to_string()));
                }
            }

            return Ok(resp);
        }
    }

    /// Headless Chromium fallback when Cloudflare/bot protection blocks plain HTTP
    pub async fn fetch_html_with_browser(&self, target_url: &str) -> Result<(String, String), AppError> {
        let parsed = Url::parse(target_url)
            .map_err(|e| AppError::BadRequest(format!("Invalid URL: {}", e)))?;
        self.validate_url_safety(&parsed).await?;

        let script = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("scripts/browser-fetch.mjs");
        let output = tokio::time::timeout(
            Duration::from_secs(16),
            tokio::process::Command::new("node")
                .arg(script)
                .arg(target_url)
                .output(),
        )
        .await
        .map_err(|_| AppError::BadRequest("Browser fallback timed out".to_string()))?
        .map_err(|e| AppError::BadRequest(format!("Browser fallback failed: {}", e)))?;

        if !output.status.success() {
            return Err(AppError::BadRequest("Site blocked or unreachable".to_string()));
        }

        let payload: serde_json::Value = serde_json::from_slice(&output.stdout)
            .map_err(|e| AppError::BadRequest(format!("Invalid browser fallback payload: {}", e)))?;
        let final_url = payload["finalUrl"].as_str().unwrap_or(target_url).to_string();
        let html = payload["html"]
            .as_str()
            .ok_or_else(|| AppError::BadRequest("Empty browser HTML".to_string()))?
            .to_string();

        if let Ok(final_parsed) = Url::parse(&final_url) {
            self.validate_url_safety(&final_parsed).await?;
        }

        Ok((html, final_url))
    }
}
