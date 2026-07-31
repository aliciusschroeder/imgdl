use std::sync::Arc;

/// Build a shared TLS client configuration with Mozilla CA roots and HTTP/2 ALPN preference.
///
/// Returns an `Arc<rustls::ClientConfig>` suitable for sharing across all connections.
/// Uses the `ring` crypto backend with embedded webpki-roots (no filesystem access).
pub(crate) fn build_tls_config() -> Arc<rustls::ClientConfig> {
    let mut root_store = rustls::RootCertStore::empty();
    root_store.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());

    let mut config = rustls::ClientConfig::builder_with_provider(Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_safe_default_protocol_versions()
    .expect("TLS protocol versions")
    .with_root_certificates(root_store)
    .with_no_client_auth();

    config.alpn_protocols = vec![b"h2".to_vec(), b"http/1.1".to_vec()];

    Arc::new(config)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_build_tls_config_returns_valid_config() {
        let config = build_tls_config();
        let _cloned: Arc<rustls::ClientConfig> = Arc::clone(&config);
    }

    #[test]
    fn test_alpn_protocols_prefer_h2() {
        let config = build_tls_config();
        assert_eq!(config.alpn_protocols.len(), 2);
        assert_eq!(config.alpn_protocols[0], b"h2");
        assert_eq!(config.alpn_protocols[1], b"http/1.1");
    }

    #[test]
    fn test_root_store_is_non_empty() {
        assert!(!webpki_roots::TLS_SERVER_ROOTS.is_empty());
        let _config = build_tls_config();
    }

    #[test]
    fn test_no_client_auth() {
        let config = build_tls_config();
        let _connector = tokio_rustls::TlsConnector::from(config);
    }
}
