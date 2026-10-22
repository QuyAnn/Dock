//! Shared ureq Agent with a real TLS backend.
//!
//! `ureq` with `default-features = false` + `native-tls` does **not** wire TLS by itself —
//! `AgentBuilder` / `ureq::get` use a no-op connector until `.tls_connector(...)` is set.
//! Without that, every HTTPS call fails with:
//! `Unknown Scheme: cannot make HTTPS request because no TLS backend is configured`.
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use ureq::native_tls::TlsConnector;

const TIMEOUT: Duration = Duration::from_secs(15);

fn tls() -> Arc<TlsConnector> {
    static TLS: OnceLock<Arc<TlsConnector>> = OnceLock::new();
    TLS.get_or_init(|| {
        Arc::new(TlsConnector::new().expect("native-tls TlsConnector (SChannel) must initialize"))
    })
    .clone()
}

/// Default HTTPS agent (follows redirects). Used by weather and similar GETs.
pub fn agent() -> ureq::Agent {
    ureq::AgentBuilder::new()
        .tls_connector(tls())
        .timeout(TIMEOUT)
        .build()
}

/// No redirects: a bearer token must never be forwarded to another host.
pub fn agent_no_redirect() -> ureq::Agent {
    ureq::AgentBuilder::new()
        .tls_connector(tls())
        .timeout(TIMEOUT)
        .redirects(0)
        .build()
}

/// True when the configured agent is willing to attempt HTTPS (TLS backend present).
/// Network / DNS / HTTP errors still return Ok(true); only the "no TLS backend" failure is false.
pub fn https_tls_ready() -> bool {
    match agent().get("https://example.com/").timeout(Duration::from_secs(5)).call() {
        Err(ureq::Error::Transport(e)) => {
            let msg = e.to_string();
            !msg.contains("no TLS backend") && !msg.contains("Unknown Scheme")
        }
        _ => true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_tls_backend_is_configured() {
        assert!(
            https_tls_ready(),
            "ureq Agent must have a TLS connector; HTTPS must not fail with 'no TLS backend'"
        );
    }
}
