//! Strict URL and SSRF validation for remote MCP (`Streamable HTTP`) and
//! OAuth 2.1 endpoints.
//!
//! Security policy:
//! - Only `https://` URLs or explicit loopback (`http://localhost`,
//!   `http://127.0.0.1`, `http://[::1]`) are accepted.
//! - Embedded credentials (`user:pass@host`), control characters, fragments,
//!   oversized URLs, and secret-like payloads are rejected.
//! - Private/RFC1918, link-local / cloud metadata (`169.254.0.0/16`), CGNAT,
//!   multicast, unspecified (`0.0.0.0`, `::`), and IPv4-mapped private IPv6
//!   addresses are rejected both syntactically and after DNS resolution.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, ToSocketAddrs};
use std::time::Duration;

use url::{Host, Url};

use crate::application::agent::injection::contains_secret;

/// Maximum accepted length of any MCP or OAuth URL (bytes).
pub(crate) const MAX_MCP_URL_LEN: usize = 2048;

/// Default connect timeout for MCP HTTP & OAuth endpoints.
pub(crate) const DEFAULT_CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// Fixed-vocabulary URL validation rejection reasons (never echo the URL).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum UrlRejectReason {
    EmptyOrTooLong,
    ControlChars,
    SecretLike,
    InvalidSyntax,
    UserinfoForbidden,
    FragmentForbidden,
    SchemeNotAllowed,
    HostMissing,
    PrivateOrBlockedHost,
    DnsResolutionFailed,
}

impl UrlRejectReason {
    #[must_use]
    pub(crate) const fn message(self) -> &'static str {
        match self {
            Self::EmptyOrTooLong => "MCP URL must be non-empty and at most 2048 bytes",
            Self::ControlChars => "MCP URL contains forbidden control characters",
            Self::SecretLike => "MCP URL appears to contain embedded secret material",
            Self::InvalidSyntax => "MCP URL is not a valid absolute URL",
            Self::UserinfoForbidden => "MCP URL must not include embedded credentials",
            Self::FragmentForbidden => "MCP URL must not include a fragment",
            Self::SchemeNotAllowed => {
                "MCP URL must use https:// (or http:// only for explicit localhost/127.0.0.1)"
            }
            Self::HostMissing => "MCP URL must specify a host",
            Self::PrivateOrBlockedHost => {
                "MCP URL targets a blocked private, link-local, metadata, or non-routable host"
            }
            Self::DnsResolutionFailed => "MCP URL host could not be resolved safely",
        }
    }
}

/// True when `host` is an explicit loopback host (`localhost`, `127.0.0.1`,
/// or `::1`). Note that `127.0.0.2` and `0.0.0.0` are intentionally excluded.
#[must_use]
pub(crate) fn is_explicit_localhost(host: &Host<&str>) -> bool {
    match host {
        Host::Domain(d) => d.eq_ignore_ascii_case("localhost"),
        Host::Ipv4(ip) => *ip == Ipv4Addr::LOCALHOST,
        Host::Ipv6(ip) => *ip == Ipv6Addr::LOCALHOST,
    }
}

/// True when `ip` is a public, globally routable unicast address (rejects
/// private, link-local, metadata, CGNAT, loopback, unspecified, broadcast,
/// multicast, documentation, and IPv4-mapped blocked IPv6 ranges).
#[must_use]
pub(crate) fn is_public_routable_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => is_public_ipv4(v4),
        IpAddr::V6(v6) => is_public_ipv6(v6),
    }
}

fn is_public_ipv4(ip: Ipv4Addr) -> bool {
    let [a, b, c, _] = ip.octets();
    if ip.is_private()
        || ip.is_loopback()
        || ip.is_link_local()
        || ip.is_broadcast()
        || ip.is_documentation()
        || ip.is_multicast()
        || ip.is_unspecified()
    {
        return false;
    }
    // 0.0.0.0/8 ("this network")
    if a == 0 {
        return false;
    }
    // 100.64.0.0/10 (Carrier-Grade NAT / Tailscale / shared address space)
    if a == 100 && (64..=127).contains(&b) {
        return false;
    }
    // 192.0.0.0/24 (IETF protocol assignments)
    if a == 192 && b == 0 && c == 0 {
        return false;
    }
    // 198.18.0.0/15 (benchmarking)
    if a == 198 && (b == 18 || b == 19) {
        return false;
    }
    // 240.0.0.0/4 (reserved)
    if a >= 240 {
        return false;
    }
    true
}

fn is_public_ipv6(ip: Ipv6Addr) -> bool {
    if ip.is_loopback() || ip.is_unspecified() || ip.is_multicast() {
        return false;
    }
    if let Some(mapped_v4) = ip.to_ipv4_mapped() {
        return is_public_ipv4(mapped_v4);
    }
    let seg0 = ip.segments()[0];
    // Unique local (fc00::/7)
    if (seg0 & 0xfe00) == 0xfc00 {
        return false;
    }
    // Link-local (fe80::/10)
    if (seg0 & 0xffc0) == 0xfe80 {
        return false;
    }
    // Site-local deprecated (fec0::/10)
    if (seg0 & 0xffc0) == 0xfec0 {
        return false;
    }
    // Documentation (2001:db8::/32)
    if seg0 == 0x2001 && ip.segments()[1] == 0x0db8 {
        return false;
    }
    true
}

fn is_blocked_metadata_domain(domain: &str) -> bool {
    let lower = domain.trim_end_matches('.').to_ascii_lowercase();
    lower == "metadata"
        || lower == "metadata.google.internal"
        || lower == "metadata.goog"
        || matches!(
            lower.rsplit('.').next(),
            Some("internal" | "local" | "localhost")
        )
}

/// Validate a user-supplied MCP HTTP or OAuth URL syntactically and against
/// SSRF rules (`https://` for public hosts, or explicit `localhost`/`127.0.0.1`/`[::1]`).
///
/// # Errors
/// Returns a fixed-vocabulary [`UrlRejectReason`] (never echoing the URL).
pub(crate) fn validate_mcp_url(raw: &str) -> Result<Url, UrlRejectReason> {
    let trimmed = raw.trim();
    if trimmed.is_empty() || trimmed.len() > MAX_MCP_URL_LEN {
        return Err(UrlRejectReason::EmptyOrTooLong);
    }
    if trimmed.chars().any(char::is_control) {
        return Err(UrlRejectReason::ControlChars);
    }
    if contains_secret(trimmed) {
        return Err(UrlRejectReason::SecretLike);
    }

    let parsed = Url::parse(trimmed).map_err(|_| UrlRejectReason::InvalidSyntax)?;
    if !parsed.username().is_empty() || parsed.password().is_some() {
        return Err(UrlRejectReason::UserinfoForbidden);
    }
    if parsed.fragment().is_some() {
        return Err(UrlRejectReason::FragmentForbidden);
    }

    let Some(host) = parsed.host() else {
        return Err(UrlRejectReason::HostMissing);
    };

    let localhost = is_explicit_localhost(&host);

    match parsed.scheme() {
        "https" => {}
        "http" if localhost => {}
        _ => return Err(UrlRejectReason::SchemeNotAllowed),
    }

    if !localhost {
        match host {
            Host::Domain(domain) => {
                if domain.is_empty() || is_blocked_metadata_domain(domain) {
                    return Err(UrlRejectReason::PrivateOrBlockedHost);
                }
            }
            Host::Ipv4(ip) => {
                if !is_public_ipv4(ip) {
                    return Err(UrlRejectReason::PrivateOrBlockedHost);
                }
            }
            Host::Ipv6(ip) => {
                if !is_public_ipv6(ip) {
                    return Err(UrlRejectReason::PrivateOrBlockedHost);
                }
            }
        }
    }

    Ok(parsed)
}

/// Perform DNS resolution for a validated URL and confirm every resolved
/// socket address obeys the SSRF policy (prevents DNS rebinding to metadata
/// or RFC1918 addresses). Explicit localhost URLs skip external DNS checks.
///
/// Returns resolved addresses so callers can pin them if needed.
///
/// # Errors
/// Returns [`UrlRejectReason::PrivateOrBlockedHost`] if any resolved IP is
/// non-public, or [`UrlRejectReason::DnsResolutionFailed`] if lookup fails.
pub(crate) fn verify_resolved_host(url: &Url) -> Result<Vec<SocketAddr>, UrlRejectReason> {
    let Some(host) = url.host() else {
        return Err(UrlRejectReason::HostMissing);
    };
    let port = url
        .port_or_known_default()
        .ok_or(UrlRejectReason::InvalidSyntax)?;

    if is_explicit_localhost(&host) {
        let addr = match host {
            Host::Ipv6(ip) => SocketAddr::new(IpAddr::V6(ip), port),
            _ => SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), port),
        };
        return Ok(vec![addr]);
    }

    let host_str = match host {
        Host::Domain(d) => d.to_string(),
        Host::Ipv4(ip) => ip.to_string(),
        Host::Ipv6(ip) => ip.to_string(),
    };
    let addrs: Vec<SocketAddr> = (host_str.as_str(), port)
        .to_socket_addrs()
        .map_err(|_| UrlRejectReason::DnsResolutionFailed)?
        .collect();
    if addrs.is_empty() {
        return Err(UrlRejectReason::DnsResolutionFailed);
    }
    for addr in &addrs {
        if !is_public_routable_ip(addr.ip()) {
            return Err(UrlRejectReason::PrivateOrBlockedHost);
        }
    }
    Ok(addrs)
}

/// Build a hardened `reqwest::blocking::Client` for MCP HTTP / OAuth calls:
/// - `rustls` TLS (crate feature),
/// - redirects strictly disabled (blocks redirect SSRF & Bearer token leak),
/// - bounded connect and request timeouts,
/// - pins verified DNS answers for non-localhost domain targets so DNS cannot
///   rebind between check and connect.
///
/// # Errors
/// Returns an error message (secret-free) if the client cannot be built.
pub(crate) fn build_hardened_http_client(
    url: &Url,
    timeout: Duration,
) -> Result<reqwest::blocking::Client, UrlRejectReason> {
    let resolved = verify_resolved_host(url)?;
    let mut builder = reqwest::blocking::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(DEFAULT_CONNECT_TIMEOUT)
        .timeout(timeout)
        .user_agent("Nexora-MCP/2.0");

    if let Some(Host::Domain(domain)) = url.host() {
        if !domain.eq_ignore_ascii_case("localhost") {
            builder = builder.resolve_to_addrs(domain, &resolved);
        }
    }

    builder
        .build()
        .map_err(|_| UrlRejectReason::DnsResolutionFailed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_https_public_and_explicit_localhost() {
        assert!(validate_mcp_url("https://mcp.example.com/v1").is_ok());
        assert!(validate_mcp_url("http://127.0.0.1:8765/mcp").is_ok());
        assert!(validate_mcp_url("http://localhost:3000/mcp").is_ok());
        assert!(validate_mcp_url("http://[::1]:3000/mcp").is_ok());
        assert!(validate_mcp_url("https://localhost:8443/mcp").is_ok());
    }

    #[test]
    fn rejects_ssrf_schemes_userinfo_and_private_ranges() {
        let bad = [
            "",
            "http://example.com/mcp",
            "ftp://example.com/mcp",
            "file:///etc/passwd",
            "javascript:alert(1)",
            "https://user:pass@mcp.example.com/mcp",
            "https://mcp.example.com/mcp#frag",
            "https://169.254.169.254/latest/meta-data",
            "http://169.254.169.254/latest/meta-data",
            "https://10.0.0.1/mcp",
            "https://172.16.0.1/mcp",
            "https://192.168.1.1/mcp",
            "https://0.0.0.0/mcp",
            "http://0.0.0.0:8080/mcp",
            "http://127.0.0.2:8080/mcp",
            "https://100.64.0.1/mcp",
            "https://[::ffff:169.254.169.254]/mcp",
            "https://[fe80::1]/mcp",
            "https://[fc00::1]/mcp",
            "https://metadata.google.internal/computeMetadata/v1",
            "https://mcp.example.com/\nheader-inject",
            "https://mcp.example.com/?k=sk-live-secret-1234567890",
        ];
        for raw in bad {
            assert!(
                validate_mcp_url(raw).is_err(),
                "expected URL to be rejected: {raw}"
            );
        }
    }
}
