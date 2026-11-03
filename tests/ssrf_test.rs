use cabinet_api::infra::http_client::SsrfSafeClient;
use std::net::IpAddr;

fn safe(ip: &str) -> bool {
    SsrfSafeClient::is_ip_safe(ip.parse::<IpAddr>().unwrap())
}

#[test]
fn private_and_tunnelled_addresses_are_blocked() {
    for ip in [
        "127.0.0.1", "10.1.2.3", "172.16.0.1", "192.168.1.1", "169.254.169.254", "100.64.0.1", "0.0.0.0",
        "::1", "::", "::7f00:1", "::ffff:127.0.0.1", "::ffff:10.0.0.1", "::ffff:169.254.169.254",
        "64:ff9b::7f00:1", "2002:7f00:1::1", "fe80::1", "fc00::1", "ff02::1",
    ] {
        assert!(!safe(ip), "{ip} must be blocked");
    }
}

#[test]
fn public_addresses_are_allowed() {
    for ip in ["8.8.8.8", "93.184.216.34", "2606:4700:4700::1111", "::ffff:8.8.8.8"] {
        assert!(safe(ip), "{ip} must be allowed");
    }
}

#[tokio::test]
async fn literal_ip_hosts_are_rejected_including_bracketed_ipv6() {
    let client = SsrfSafeClient::new().unwrap();
    for url in ["http://127.0.0.1/", "http://[::1]/", "http://[::ffff:169.254.169.254]/", "http://169.254.169.254/latest"] {
        assert!(client.get_safe(url).await.is_err(), "{url} must be rejected");
    }
}
