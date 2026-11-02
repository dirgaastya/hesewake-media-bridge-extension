//! The invitation a phone can scan instead of looking for this PC: where it is, and the fingerprint of its certificate
//! (MEDIA_BRIDGE_ANDROID_NOTCH_PLAN.md). It is shown as a QR code while pairing is open. It carries no secret: the phone
//! still has to match the six digits and both sides still have to approve.

use crate::wire::{b64_encode, MAX_TEXT};
use qrcode::{Color, EcLevel, QrCode};
use serde_json::json;
use std::net::Ipv4Addr;

pub const PREFIX: &str = "hesewake-media:1:";

/// `hesewake-media:1:` and base64url (no padding) of `{"host":..,"port":..,"fingerprint":"<64 hex>"}`.
pub fn payload(host: &str, port: u16, fingerprint: &[u8; 32]) -> String {
    let hex: String = fingerprint.iter().map(|b| format!("{b:02x}")).collect();
    // key order and no spaces, as the phone's reference writes it
    let body = format!("{{\"host\":{},\"port\":{},\"fingerprint\":\"{}\"}}", json!(host), port, hex);
    format!("{PREFIX}{}", b64_encode(body.as_bytes()))
}

/// The QR code of `text` as rows of `0` and `1`, the form a page's `matrix` takes. None if it does not fit a code.
pub fn matrix(text: &str) -> Option<(usize, Vec<String>)> {
    if text.len() > MAX_TEXT {
        return None;
    }
    let code = QrCode::with_error_correction_level(text.as_bytes(), EcLevel::M).ok()?;
    let size = code.width();
    let colors = code.to_colors();
    let rows = colors.chunks(size).map(|row| row.iter().map(|c| if *c == Color::Dark { '1' } else { '0' }).collect()).collect();
    Some((size, rows))
}

/// This PC's address on the network the phone can reach: a real adapter's, not a tunnel's (see `net`).
pub fn lan_address() -> Option<Ipv4Addr> {
    crate::net::primary()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_invitation_is_what_the_phones_reference_writes() {
        let fp: [u8; 32] = std::array::from_fn(|i| i as u8);
        assert_eq!(
            payload("192.168.1.10", 12345, &fp),
            "hesewake-media:1:eyJob3N0IjoiMTkyLjE2OC4xLjEwIiwicG9ydCI6MTIzNDUsImZpbmdlcnByaW50IjoiMDAwMTAyMDMwNDA1MDYwNzA4MDkwYTBiMGMwZDBlMGYxMDExMTIxMzE0MTUxNjE3MTgxOTFhMWIxYzFkMWUxZiJ9"
        );
    }

    #[test]
    fn it_is_valid_json_inside() {
        let p = payload("10.0.0.5", 4000, &[0xab; 32]);
        let inner = crate::wire::b64_decode(p.strip_prefix(PREFIX).unwrap()).unwrap();
        let v: serde_json::Value = serde_json::from_slice(&inner).unwrap();
        assert_eq!((v["host"].as_str(), v["port"].as_u64(), v["fingerprint"].as_str().map(str::len)), (Some("10.0.0.5"), Some(4000), Some(64)));
        assert_eq!(v["fingerprint"].as_str().unwrap(), "ab".repeat(32), "lower-case hex");
    }

    #[test]
    fn the_matrix_is_a_square_of_zeros_and_ones() {
        let (size, rows) = matrix(&payload("192.168.1.10", 12345, &[7; 32])).unwrap();
        assert!((21..=100).contains(&size), "{size}");
        assert_eq!(rows.len(), size);
        assert!(rows.iter().all(|r| r.len() == size && r.bytes().all(|c| c == b'0' || c == b'1')));
        assert!(matrix(&"x".repeat(MAX_TEXT + 1)).is_none());
    }

    /// The real test of a QR code: another program reads it back to the text it was made from.
    #[test]
    fn a_qr_reader_gets_the_invitation_back() {
        let text = payload("192.168.1.10", 12345, &std::array::from_fn(|i| (i * 7) as u8));
        let (size, rows) = matrix(&text).unwrap();
        let scale = 6usize;
        let quiet = 4 * scale;
        let side = size * scale + 2 * quiet;
        let dark = |x: usize, y: usize| {
            if x < quiet || y < quiet || x >= side - quiet || y >= side - quiet {
                return false;
            }
            rows[(y - quiet) / scale].as_bytes()[(x - quiet) / scale] == b'1'
        };
        let mut img = rqrr::PreparedImage::prepare_from_greyscale(side, side, |x, y| if dark(x, y) { 0 } else { 255 });
        let grids = img.detect_grids();
        assert_eq!(grids.len(), 1, "one code is found");
        let (_, content) = grids[0].decode().expect("it decodes");
        assert_eq!(content, text);
    }

    #[test]
    fn the_address_is_a_real_one_when_there_is_a_network() {
        // no network is not a failure of the code; with one, the answer is a usable address
        if let Some(a) = lan_address() {
            assert!(!a.is_loopback() && !a.is_unspecified() && !a.is_multicast(), "{a}");
        }
    }
}
