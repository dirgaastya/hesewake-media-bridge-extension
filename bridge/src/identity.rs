//! The server's TLS identity: a self-signed certificate that the phone pins by its fingerprint. It is made once and kept in
//! `identity.bin`, protected by Windows (DPAPI: only this Windows user can open it). A missing, damaged or foreign file
//! makes a new identity, and phones paired with the old one have to pair again (their pin no longer matches).

use crate::wire::fingerprint;
use std::path::Path;

#[derive(Clone)]
pub struct Identity {
    pub cert_der: Vec<u8>,
    /// PKCS#8.
    pub key_der: Vec<u8>,
    pub fingerprint: [u8; 32],
}

impl Identity {
    pub fn generate() -> Option<Identity> {
        let ck = rcgen::generate_simple_self_signed(vec!["hesewake-media-bridge".to_string()]).ok()?;
        let cert_der = ck.cert.der().to_vec();
        let fingerprint = fingerprint(&cert_der);
        Some(Identity { cert_der, key_der: ck.key_pair.serialize_der(), fingerprint })
    }

    /// `cert length (u32, little-endian) | certificate | key`
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut out = (self.cert_der.len() as u32).to_le_bytes().to_vec();
        out.extend_from_slice(&self.cert_der);
        out.extend_from_slice(&self.key_der);
        out
    }

    pub fn from_bytes(b: &[u8]) -> Option<Identity> {
        let n = u32::from_le_bytes(b.get(..4)?.try_into().ok()?) as usize;
        let cert_der = b.get(4..4usize.checked_add(n)?)?.to_vec();
        let key_der = b.get(4 + n..)?.to_vec();
        if cert_der.is_empty() || key_der.is_empty() {
            return None;
        }
        let fingerprint = fingerprint(&cert_der);
        Some(Identity { cert_der, key_der, fingerprint })
    }

    /// The identity kept in `dir/identity.bin`, or a new one (written there) if there is none that works.
    pub fn load_or_create(dir: &Path) -> Option<Identity> {
        let file = dir.join("identity.bin");
        if let Some(id) = std::fs::read(&file).ok().and_then(|b| crate::dpapi::unprotect(&b)).and_then(|b| Identity::from_bytes(&b)) {
            // a file that opens but does not make a TLS config is no good either
            if crate::tls::server_config(&id).is_some() {
                return Some(id);
            }
        }
        let id = Identity::generate()?;
        std::fs::create_dir_all(dir).ok()?;
        std::fs::write(&file, crate::dpapi::protect(&id.to_bytes())?).ok()?;
        Some(id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("hesewake-bridge-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        d
    }

    #[test]
    fn a_new_identity_has_the_fingerprint_of_its_certificate() {
        let id = Identity::generate().unwrap();
        assert_eq!(id.fingerprint, fingerprint(&id.cert_der));
        assert!(crate::tls::server_config(&id).is_some(), "it makes a TLS configuration");
        let other = Identity::generate().unwrap();
        assert_ne!(id.fingerprint, other.fingerprint, "every identity is its own");
    }

    #[test]
    fn bytes_roundtrip_and_refuse_nonsense() {
        let id = Identity::generate().unwrap();
        let back = Identity::from_bytes(&id.to_bytes()).unwrap();
        assert_eq!((back.cert_der, back.key_der, back.fingerprint), (id.cert_der.clone(), id.key_der.clone(), id.fingerprint));
        for bad in [&[][..], &[1, 2], &[255, 255, 255, 255, 1], &[4, 0, 0, 0, 1, 2, 3, 4], &[0, 0, 0, 0, 9]] {
            assert!(Identity::from_bytes(bad).is_none(), "{bad:?}");
        }
    }

    #[test]
    fn it_is_kept_between_runs_and_the_file_is_not_readable_as_it_is() {
        let dir = scratch("keep");
        let first = Identity::load_or_create(&dir).unwrap();
        let second = Identity::load_or_create(&dir).unwrap();
        assert_eq!(first.fingerprint, second.fingerprint, "the same identity the next time");
        let raw = std::fs::read(dir.join("identity.bin")).unwrap();
        assert!(!raw.windows(first.key_der.len().min(32)).any(|w| w == &first.key_der[..first.key_der.len().min(32)]), "the private key is not in the file in the clear");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_damaged_file_gives_a_new_identity() {
        let dir = scratch("damaged");
        let first = Identity::load_or_create(&dir).unwrap();
        std::fs::write(dir.join("identity.bin"), b"this was not written by Windows").unwrap();
        let second = Identity::load_or_create(&dir).unwrap();
        assert_ne!(first.fingerprint, second.fingerprint);
        assert_eq!(Identity::load_or_create(&dir).unwrap().fingerprint, second.fingerprint, "and the new one is kept");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
