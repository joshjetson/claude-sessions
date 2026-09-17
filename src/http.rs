//! The one place an HTTP agent is built.
//!
//! Two clients talk to the outside world over HTTPS — Odoo's JSON-RPC and the
//! Optics API — and both used to build their own [`ureq::Agent`] from the same
//! recipe. That duplication cost a release: ureq 3 defaults its TLS provider to
//! rustls *whether or not the rustls feature is on*, and
//! "The setting is never picked up automatically" — so an agent built without
//! naming the provider **panics on the first `https://` request** with
//! `provider is Rustls but feature is not enabled`. Nothing catches it in a
//! unit test, because every test here drives a stub transport rather than a
//! socket. It is one line, it has to be on every agent, and so there is now
//! exactly one agent builder.
//!
//! The crate uses native-tls deliberately: it links the platform's own TLS
//! stack, so `cargo install claude-sessions` needs no cmake, no nasm, and no
//! vendored certificate bundle, and a corporate root certificate installed in
//! the system keychain simply works.

use std::time::Duration;

use ureq::tls::{TlsConfig, TlsProvider};

/// A pooled agent with a global timeout, talking the TLS this crate is built
/// with.
///
/// `status_as_error` is a parameter because the two callers genuinely differ:
/// Odoo puts the useful message in the body of a non-2xx response, so the
/// status must not short-circuit reading it, while the Optics client wants the
/// status.
pub fn agent(timeout: Duration, status_as_error: bool) -> ureq::Agent {
    let config = ureq::Agent::config_builder()
        .timeout_global(Some(timeout))
        .http_status_as_error(status_as_error)
        .tls_config(
            TlsConfig::builder()
                .provider(TlsProvider::NativeTls)
                .build(),
        )
        .build();
    config.into()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The regression this module exists for. An agent whose provider is the
    /// default panics on the first HTTPS request — which means the board, the
    /// deploy tab and the coverage badges all die at once, in a release build,
    /// on a machine that has credentials. A unit test cannot make the request,
    /// but it can assert the setting that decides it.
    #[test]
    fn every_agent_names_the_tls_provider_this_crate_is_built_with() {
        let agent = agent(Duration::from_secs(5), false);
        assert_eq!(
            agent.config().tls_config().provider(),
            TlsProvider::NativeTls
        );
    }

    /// The same thing, proved end to end against a real TLS endpoint. Ignored
    /// by default because it needs the network; run it with
    /// `cargo test --lib http:: -- --ignored` after touching anything here.
    #[test]
    #[ignore = "needs the network"]
    fn a_real_https_request_completes_instead_of_panicking() {
        let response = agent(Duration::from_secs(20), false)
            .get("https://static.crates.io/")
            .call();
        // Any answer at all means the handshake happened. The panic this guards
        // against would have killed the thread before a result existed.
        assert!(response.is_ok(), "{response:?}");
    }

    #[test]
    fn the_status_policy_is_the_callers_to_choose() {
        assert!(!agent(Duration::from_secs(5), false)
            .config()
            .http_status_as_error());
        assert!(agent(Duration::from_secs(5), true)
            .config()
            .http_status_as_error());
    }
}
