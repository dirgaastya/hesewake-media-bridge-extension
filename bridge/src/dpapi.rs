//! Windows data protection (DPAPI): bytes only this Windows user can read back, on this PC.

use std::ffi::c_void;
use windows::core::w;
use windows::Win32::Foundation::{LocalFree, HLOCAL};
use windows::Win32::Security::Cryptography::{CryptProtectData, CryptUnprotectData, CRYPTPROTECT_UI_FORBIDDEN, CRYPT_INTEGER_BLOB};

/// Mixed into the key, so another program of the same user does not open it by accident.
const ENTROPY: &[u8] = b"hesewake-media-bridge identity v1";

fn blob(data: &[u8]) -> CRYPT_INTEGER_BLOB {
    CRYPT_INTEGER_BLOB { cbData: data.len() as u32, pbData: data.as_ptr() as *mut u8 }
}

/// Take what Windows allocated for `out`, copy it and free it.
unsafe fn take(out: CRYPT_INTEGER_BLOB) -> Vec<u8> {
    let v = std::slice::from_raw_parts(out.pbData, out.cbData as usize).to_vec();
    let _ = LocalFree(HLOCAL(out.pbData as *mut c_void));
    v
}

pub fn protect(data: &[u8]) -> Option<Vec<u8>> {
    unsafe {
        let (input, entropy) = (blob(data), blob(ENTROPY));
        let mut out = CRYPT_INTEGER_BLOB::default();
        CryptProtectData(&input, w!("Hesewake Media Bridge"), Some(&entropy), None, None, CRYPTPROTECT_UI_FORBIDDEN, &mut out).ok()?;
        Some(take(out))
    }
}

pub fn unprotect(data: &[u8]) -> Option<Vec<u8>> {
    if data.is_empty() {
        return None;
    }
    unsafe {
        let (input, entropy) = (blob(data), blob(ENTROPY));
        let mut out = CRYPT_INTEGER_BLOB::default();
        CryptUnprotectData(&input, None, Some(&entropy), None, None, CRYPTPROTECT_UI_FORBIDDEN, &mut out).ok()?;
        Some(take(out))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn what_is_protected_comes_back_and_is_not_the_same_bytes() {
        let secret = b"a private key, more or less".to_vec();
        let p = protect(&secret).unwrap();
        assert_ne!(p, secret);
        assert!(!p.windows(secret.len()).any(|w| w == &secret[..]));
        assert_eq!(unprotect(&p), Some(secret));
    }

    #[test]
    fn damage_and_nonsense_are_refused() {
        let mut p = protect(b"hello").unwrap();
        assert_eq!(unprotect(&[]), None);
        assert_eq!(unprotect(b"not from Windows"), None);
        let last = p.len() - 1;
        p[last] ^= 0xff;
        assert_eq!(unprotect(&p), None, "a changed byte is noticed");
    }
}
