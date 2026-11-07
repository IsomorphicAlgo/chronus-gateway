//! Access control (Tranche 4, `Methodology.md` D-051).
//!
//! The gateway's API is read-only, and since Tranche 3 it never transmits
//! (`docs/TELECOMMAND.md`), so the risks are confidentiality (who can read telemetry), integrity of
//! what it **accepts** (spoofed UDP), and availability (too many clients). This module holds the
//! pieces that address them:
//!
//! | Piece | Protects |
//! |---|---|
//! | [`ApiTokens`] | every HTTP route except `GET /health`: bearer tokens, compared in constant time |
//! | [`origin_allowed`] | the WebSocket against cross-site hijacking from a malicious page in an operator's browser |
//! | `max_ws_clients` (in [`crate::config::SecurityConfig`]) | the process against WebSocket exhaustion |
//! | [`SourceFilter`] | the UDP telemetry and receiver ports against senders outside the allowed networks |
//! | [`check_bind_policy`] | against serving an unauthenticated API to the network by accident |
//!
//! TLS is deliberately **not** built in: it is terminated by a reverse proxy (Caddy, nginx), which
//! keeps a C-compiled crypto stack out of the default build. `docs/DEPLOYMENT.md` shows the setup.

use std::net::{IpAddr, SocketAddr};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use base64::Engine;
use serde::Serialize;

use crate::config::{IpNet, MIN_TOKEN_LEN, SecurityConfig};
use crate::diagnose::{Diagnosis, Severity};

/// Environment variable holding API tokens (comma- or whitespace-separated).
pub const TOKENS_ENV: &str = "CHRONUS_API_TOKENS";

/// The configured API tokens. Empty means authentication is off.
#[derive(Default)]
pub struct ApiTokens {
    tokens: Vec<Vec<u8>>,
}

impl std::fmt::Debug for ApiTokens {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Never print token material.
        f.debug_struct("ApiTokens")
            .field("count", &self.tokens.len())
            .finish()
    }
}

/// Why the API tokens could not be loaded.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum TokenError {
    /// The tokens file could not be read.
    #[error("cannot read the API tokens file {path}: {source}")]
    Read {
        /// The configured file.
        path: PathBuf,
        /// The I/O error.
        #[source]
        source: std::io::Error,
    },
    /// A token is too short to be safe.
    #[error(
        "an API token from {origin} is {len} characters; at least {MIN_TOKEN_LEN} are required"
    )]
    TooShort {
        /// Where the token came from (file path or the environment variable).
        origin: String,
        /// Its length.
        len: usize,
    },
}

impl ApiTokens {
    /// No tokens: authentication off.
    #[must_use]
    pub fn disabled() -> Self {
        Self::default()
    }

    /// Tokens given directly (tests and embedders).
    ///
    /// # Errors
    ///
    /// [`TokenError::TooShort`] for a token under [`MIN_TOKEN_LEN`] characters.
    pub fn from_tokens<I, S>(tokens: I) -> Result<Self, TokenError>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        let mut out = Self::default();
        for t in tokens {
            out.push(t.as_ref(), "the caller")?;
        }
        Ok(out)
    }

    /// Loads tokens from `cfg.api_tokens_file` and the [`TOKENS_ENV`] environment variable.
    ///
    /// # Errors
    ///
    /// [`TokenError`] when the file cannot be read or a token is too short.
    pub fn load(cfg: &SecurityConfig) -> Result<Self, TokenError> {
        let env = std::env::var(TOKENS_ENV).ok();
        Self::load_from(cfg.api_tokens_file.as_deref(), env.as_deref())
    }

    fn load_from(file: Option<&Path>, env: Option<&str>) -> Result<Self, TokenError> {
        let mut out = Self::default();
        if let Some(path) = file {
            let text = std::fs::read_to_string(path).map_err(|source| TokenError::Read {
                path: path.to_path_buf(),
                source,
            })?;
            for line in text.lines().map(str::trim) {
                if line.is_empty() || line.starts_with('#') {
                    continue;
                }
                out.push(line, &path.display().to_string())?;
            }
        }
        if let Some(env) = env {
            for t in env.split(|c: char| c == ',' || c.is_whitespace()) {
                if !t.is_empty() {
                    out.push(t, TOKENS_ENV)?;
                }
            }
        }
        Ok(out)
    }

    fn push(&mut self, token: &str, origin: &str) -> Result<(), TokenError> {
        let len = token.chars().count();
        if len < MIN_TOKEN_LEN {
            return Err(TokenError::TooShort {
                origin: origin.to_string(),
                len,
            });
        }
        self.tokens.push(token.as_bytes().to_vec());
        Ok(())
    }

    /// Whether any token is configured (authentication on).
    #[must_use]
    pub fn is_enabled(&self) -> bool {
        !self.tokens.is_empty()
    }

    /// How many tokens are configured.
    #[must_use]
    pub fn len(&self) -> usize {
        self.tokens.len()
    }

    /// `true` when no token is configured.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.tokens.is_empty()
    }

    /// Whether `presented` matches a configured token. Every configured token is compared, each
    /// in constant time, so the time taken does not reveal which bytes matched.
    ///
    /// # Examples
    ///
    /// ```
    /// use chronus_gateway::security::ApiTokens;
    ///
    /// let t = "0123456789abcdef0123456789abcdef";
    /// let tokens = ApiTokens::from_tokens([t]).unwrap();
    /// assert!(tokens.verify(t));
    /// assert!(!tokens.verify("0123456789abcdef0123456789abcdeX"));
    /// ```
    #[must_use]
    pub fn verify(&self, presented: &str) -> bool {
        let p = presented.as_bytes();
        let mut ok = false;
        for t in &self.tokens {
            ok |= constant_time_eq(t, p);
        }
        ok
    }
}

/// Compares two byte strings without an early exit on the first difference.
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b) {
        diff |= x ^ y;
    }
    // `black_box` keeps the optimizer from turning the fold back into an early-exit compare.
    std::hint::black_box(diff) == 0
}

/// A new random API token: 32 bytes from the operating system's CSPRNG, base64url (43 chars).
///
/// # Errors
///
/// When the operating system cannot provide random bytes.
pub fn generate_token() -> Result<String, getrandom::Error> {
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes)?;
    Ok(base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes))
}

/// Pulls the presented token out of an `Authorization: Bearer` header value or an
/// `access_token` query parameter (RFC 6750 §2.1 and §2.3).
#[must_use]
pub fn presented_token<'a>(
    authorization: Option<&'a str>,
    query: Option<&'a str>,
) -> Option<&'a str> {
    if let Some(h) = authorization {
        let mut parts = h.trim().splitn(2, ' ');
        if parts
            .next()
            .is_some_and(|s| s.eq_ignore_ascii_case("bearer"))
        {
            return parts.next().map(str::trim).filter(|t| !t.is_empty());
        }
    }
    query?
        .split('&')
        .find_map(|kv| kv.strip_prefix("access_token="))
        .filter(|t| !t.is_empty())
}

/// Lower-cases and validates an origin (`scheme://host[:port]`), dropping a trailing `/`.
/// Returns `None` for anything else (paths, missing scheme, non-HTTP schemes).
///
/// # Examples
///
/// ```
/// use chronus_gateway::security::normalize_origin;
///
/// assert_eq!(normalize_origin("HTTPS://Ops.Example.org/").as_deref(), Some("https://ops.example.org"));
/// assert_eq!(normalize_origin("http://10.0.0.5:5174").as_deref(), Some("http://10.0.0.5:5174"));
/// assert!(normalize_origin("ops.example.org").is_none());
/// assert!(normalize_origin("https://x.org/dashboard").is_none());
/// ```
#[must_use]
pub fn normalize_origin(origin: &str) -> Option<String> {
    let o = origin.trim().trim_end_matches('/').to_ascii_lowercase();
    let rest = o
        .strip_prefix("https://")
        .or_else(|| o.strip_prefix("http://"))?;
    if rest.is_empty() || rest.contains(['/', '?', '#', '@', ' ']) {
        return None;
    }
    Some(o)
}

/// Whether a host name or literal is this computer.
fn is_loopback_host(host: &str) -> bool {
    let host = host.trim_start_matches('[').trim_end_matches(']');
    host == "localhost"
        || host.ends_with(".localhost")
        || host.parse::<IpAddr>().is_ok_and(|ip| ip.is_loopback())
}

/// Host part of a normalized origin (without the port).
fn origin_host(origin: &str) -> Option<&str> {
    let rest = origin.split_once("://")?.1;
    if let Some(v6) = rest.strip_prefix('[') {
        return v6.split_once(']').map(|(h, _)| h);
    }
    Some(rest.rsplit_once(':').map_or(rest, |(h, _)| h))
}

/// Whether a WebSocket request carrying `origin` may connect.
///
/// - No `Origin` header: a non-browser client (Open MCT's server side, scripts, tests) — allowed;
///   tokens are what protect against those.
/// - A page served from this computer (`localhost`, `127.0.0.0/8`, `::1`) — allowed.
/// - Otherwise only origins listed in `allowed` (or `"*"`).
///
/// # Examples
///
/// ```
/// use chronus_gateway::security::origin_allowed;
///
/// let allowed = vec!["https://ops.example.org".to_string()];
/// assert!(origin_allowed(None, &allowed));
/// assert!(origin_allowed(Some("http://127.0.0.1:5174"), &allowed));
/// assert!(origin_allowed(Some("https://OPS.example.org"), &allowed));
/// assert!(!origin_allowed(Some("https://evil.example.com"), &allowed));
/// ```
#[must_use]
pub fn origin_allowed(origin: Option<&str>, allowed: &[String]) -> bool {
    let Some(origin) = origin else {
        return true;
    };
    if allowed.iter().any(|a| a == "*") {
        return true;
    }
    // Browsers send the literal `null` for sandboxed or file:// pages: never trusted.
    let Some(o) = normalize_origin(origin) else {
        return false;
    };
    if origin_host(&o).is_some_and(is_loopback_host) {
        return true;
    }
    allowed
        .iter()
        .filter_map(|a| normalize_origin(a))
        .any(|a| a == o)
}

/// Security counters (exposed as `security` in `GET /api/v1/chronus/metrics`).
#[derive(Debug, Default)]
#[non_exhaustive]
pub struct SecurityStats {
    /// Requests rejected for a missing or wrong token.
    pub auth_failures: AtomicU64,
    /// WebSocket upgrades rejected for their `Origin`.
    pub ws_rejected_origin: AtomicU64,
    /// WebSocket upgrades rejected because `max_ws_clients` were connected.
    pub ws_rejected_capacity: AtomicU64,
    /// Telemetry datagrams dropped by `udp_allowed_sources`.
    pub udp_rejected: AtomicU64,
    /// Receiver datagrams dropped by `rf_allowed_sources`.
    pub rf_rejected: AtomicU64,
    /// WebSocket slots in use (reserved before the upgrade completes).
    pub(crate) ws_slots: AtomicU64,
}

/// Serializable [`SecurityStats`] snapshot.
#[derive(Debug, Clone, Default, Serialize)]
#[non_exhaustive]
pub struct SecuritySnapshot {
    /// Whether API tokens are required.
    pub auth_enabled: bool,
    /// Requests rejected for a missing or wrong token.
    pub auth_failures: u64,
    /// WebSocket upgrades rejected for their `Origin`.
    pub ws_rejected_origin: u64,
    /// WebSocket upgrades rejected at capacity.
    pub ws_rejected_capacity: u64,
    /// Telemetry datagrams from senders outside `udp_allowed_sources`.
    pub udp_rejected: u64,
    /// Receiver datagrams from senders outside `rf_allowed_sources`.
    pub rf_rejected: u64,
}

impl SecurityStats {
    /// A `Relaxed` snapshot.
    pub fn snapshot(&self, auth_enabled: bool) -> SecuritySnapshot {
        let l = |a: &AtomicU64| a.load(Ordering::Relaxed);
        SecuritySnapshot {
            auth_enabled,
            auth_failures: l(&self.auth_failures),
            ws_rejected_origin: l(&self.ws_rejected_origin),
            ws_rejected_capacity: l(&self.ws_rejected_capacity),
            udp_rejected: l(&self.udp_rejected),
            rf_rejected: l(&self.rf_rejected),
        }
    }
}

/// Which UDP port a [`SourceFilter`] guards.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum UdpPort {
    /// The telemetry port (`ingest.bind_addr`).
    Telemetry,
    /// The receiver-data port (`rf.bind_addr`).
    Rf,
}

/// Drops datagrams from senders outside an allow-list (UDP cannot carry a token).
#[derive(Debug, Clone)]
pub struct SourceFilter {
    nets: Option<Arc<[IpNet]>>,
    stats: Arc<SecurityStats>,
    port: UdpPort,
}

impl SourceFilter {
    /// A filter that accepts everything.
    #[must_use]
    pub fn allow_all() -> Self {
        Self {
            nets: None,
            stats: Arc::new(SecurityStats::default()),
            port: UdpPort::Telemetry,
        }
    }

    /// A filter for `port` accepting only `nets` (`None` = everything); rejections are counted
    /// in `stats`.
    #[must_use]
    pub fn new(nets: Option<Vec<IpNet>>, stats: Arc<SecurityStats>, port: UdpPort) -> Self {
        Self {
            nets: nets.map(Arc::from),
            stats,
            port,
        }
    }

    /// Whether a datagram from `from` is accepted. Rejections are counted and the first one is
    /// logged.
    pub fn allows(&self, from: SocketAddr) -> bool {
        let Some(nets) = &self.nets else {
            return true;
        };
        if nets.iter().any(|n| n.contains(from.ip())) {
            return true;
        }
        let counter = match self.port {
            UdpPort::Telemetry => &self.stats.udp_rejected,
            UdpPort::Rf => &self.stats.rf_rejected,
        };
        if counter.fetch_add(1, Ordering::Relaxed) == 0 {
            tracing::warn!(
                sender = %from,
                port = ?self.port,
                "datagram from a sender outside the allowed sources was dropped (further drops are only counted)"
            );
        }
        false
    }
}

/// Refuses to serve the API to the network without authentication (`CG-137`), unless the
/// operator said so explicitly.
#[must_use]
pub fn check_bind_policy(
    http_bind: SocketAddr,
    tokens: &ApiTokens,
    cfg: &SecurityConfig,
) -> Option<Diagnosis> {
    if http_bind.ip().is_loopback() || tokens.is_enabled() || cfg.allow_unauthenticated_remote {
        return None;
    }
    Some(
        Diagnosis::new(
            "CG-137",
            Severity::Error,
            format!(
                "Refusing to serve the dashboard API on {http_bind} without authentication."
            ),
            "That address is reachable from other computers, and no API token is configured, so \
             anyone on the network could read the telemetry stream and history.",
        )
        .fix("Create a token with `chronus-gateway --generate-token`, save it in a file, and set `security.api_tokens_file` (or put it in the CHRONUS_API_TOKENS environment variable).")
        .fix("Or listen only on this computer: `ingest.http_bind = \"127.0.0.1:8080\"`, and put a reverse proxy in front (docs/DEPLOYMENT.md).")
        .fix("If a proxy in front of the gateway already authenticates users, set `security.allow_unauthenticated_remote = true`."),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    const T: &str = "0123456789abcdef0123456789abcdef";

    #[test]
    fn tokens_load_from_file_and_env_and_reject_short_ones() {
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("tokens.txt");
        std::fs::write(&f, format!("# operators\n{T}\n\n  {T}X  \n")).unwrap();
        let env = format!("{}A,{}B", T, T);
        let t = ApiTokens::load_from(Some(&f), Some(&env)).unwrap();
        assert_eq!(t.len(), 4);
        assert!(t.verify(T) && t.verify(&format!("{T}X")) && t.verify(&format!("{T}B")));
        assert!(!t.verify("") && !t.verify(&T[1..]));

        std::fs::write(&f, "short\n").unwrap();
        let err = ApiTokens::load_from(Some(&f), None).unwrap_err();
        assert!(matches!(err, TokenError::TooShort { len: 5, .. }));
        let err = ApiTokens::load_from(None, Some("abc")).unwrap_err();
        assert!(err.to_string().contains(TOKENS_ENV));
        let err = ApiTokens::load_from(Some(&dir.path().join("missing")), None).unwrap_err();
        assert!(matches!(err, TokenError::Read { .. }));
        assert!(!ApiTokens::load_from(None, None).unwrap().is_enabled());
    }

    #[test]
    fn debug_never_prints_tokens() {
        let t = ApiTokens::from_tokens([T]).unwrap();
        let shown = format!("{t:?}");
        assert!(!shown.contains(T) && shown.contains("count: 1"));
    }

    #[test]
    fn generated_tokens_are_long_unique_and_url_safe() {
        let a = generate_token().unwrap();
        let b = generate_token().unwrap();
        assert_eq!(a.len(), 43);
        assert_ne!(a, b);
        assert!(
            a.chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        );
        assert!(ApiTokens::from_tokens([&a]).is_ok());
    }

    #[test]
    fn presented_token_reads_header_then_query() {
        assert_eq!(presented_token(Some("Bearer abc"), None), Some("abc"));
        assert_eq!(presented_token(Some("bearer   abc "), None), Some("abc"));
        assert_eq!(presented_token(Some("Basic abc"), None), None);
        assert_eq!(presented_token(None, Some("x=1&access_token=q")), Some("q"));
        assert_eq!(
            presented_token(Some("Bearer h"), Some("access_token=q")),
            Some("h")
        );
        assert_eq!(presented_token(None, Some("access_token=")), None);
        assert_eq!(presented_token(None, None), None);
    }

    #[test]
    fn origins_loopback_listed_and_wildcard() {
        let none: Vec<String> = Vec::new();
        for ok in [
            "http://localhost:5173",
            "http://127.0.0.1:5174",
            "http://[::1]:8080",
            "http://app.localhost",
        ] {
            assert!(origin_allowed(Some(ok), &none), "{ok}");
        }
        for bad in [
            "https://evil.example",
            "null",
            "file://",
            "http://127.0.0.1.evil.example",
        ] {
            assert!(!origin_allowed(Some(bad), &none), "{bad}");
        }
        assert!(origin_allowed(
            Some("https://evil.example"),
            &["*".to_string()]
        ));
        let listed = vec!["https://ops.example.org/".to_string()];
        assert!(origin_allowed(Some("https://ops.example.org"), &listed));
        assert!(
            !origin_allowed(Some("http://ops.example.org"), &listed),
            "scheme matters"
        );
        assert!(
            !origin_allowed(Some("https://ops.example.org:444"), &listed),
            "port matters"
        );
    }

    #[test]
    fn source_filter_counts_rejections_per_port() {
        let stats = Arc::new(SecurityStats::default());
        let f = SourceFilter::new(
            Some(vec!["10.0.0.0/8".parse().unwrap(), "::1".parse().unwrap()]),
            Arc::clone(&stats),
            UdpPort::Rf,
        );
        assert!(f.allows("10.1.2.3:5000".parse().unwrap()));
        assert!(f.allows("[::1]:5000".parse().unwrap()));
        assert!(
            f.allows("[::ffff:10.0.0.1]:5000".parse().unwrap()),
            "v4-mapped"
        );
        assert!(!f.allows("192.168.0.1:5000".parse().unwrap()));
        assert_eq!(stats.snapshot(false).rf_rejected, 1);
        assert_eq!(stats.snapshot(false).udp_rejected, 0);
        assert!(SourceFilter::allow_all().allows("8.8.8.8:1".parse().unwrap()));
    }

    #[test]
    fn ipnet_edges() {
        let any: IpNet = "0.0.0.0/0".parse().unwrap();
        assert!(any.contains("203.0.113.9".parse().unwrap()));
        assert!(!any.contains("::1".parse().unwrap()), "families do not mix");
        let v6: IpNet = "2001:db8::/32".parse().unwrap();
        assert!(v6.contains("2001:db8:1::5".parse().unwrap()));
        assert!(!v6.contains("2001:db9::1".parse().unwrap()));
        assert!("::/129".parse::<IpNet>().is_err());
        assert_eq!(
            "10.0.0.0/8".parse::<IpNet>().unwrap().to_string(),
            "10.0.0.0/8"
        );
    }

    #[test]
    fn bind_policy_refuses_only_unauthenticated_remote() {
        let cfg = SecurityConfig::default();
        let none = ApiTokens::disabled();
        let some = ApiTokens::from_tokens([T]).unwrap();
        let remote: SocketAddr = "0.0.0.0:8080".parse().unwrap();
        let local: SocketAddr = "127.0.0.1:8080".parse().unwrap();
        assert!(check_bind_policy(local, &none, &cfg).is_none());
        assert!(check_bind_policy(remote, &some, &cfg).is_none());
        let d = check_bind_policy(remote, &none, &cfg).unwrap();
        assert_eq!(d.code, "CG-137");
        let mut cfg = cfg;
        cfg.allow_unauthenticated_remote = true;
        assert!(check_bind_policy(remote, &none, &cfg).is_none());
    }
}
