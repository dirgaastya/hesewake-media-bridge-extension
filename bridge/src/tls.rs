//! TLS for the phone's connection: the server's certificate (pinned by the phone) and nothing else. There is no client
//! certificate; the phone proves itself with the token it was given when it paired.

use crate::identity::Identity;
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
use rustls::ServerConfig;
use std::sync::Arc;

pub fn server_config(id: &Identity) -> Option<Arc<ServerConfig>> {
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let cfg = ServerConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .ok()?
        .with_no_client_auth()
        .with_single_cert(vec![CertificateDer::from(id.cert_der.clone())], PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(id.key_der.clone())))
        .ok()?;
    Some(Arc::new(cfg))
}
