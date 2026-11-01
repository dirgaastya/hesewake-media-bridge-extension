//! The picture of the song for the notch. The phone sends its artwork in every snapshot (a JPEG of up to 512 px and 256 KB,
//! once a second); the notch's page takes a small one (a frame is at most 64 KiB), so it is cut to a square, made small and
//! written again as a JPEG, and only when the picture is another one.

use crate::wire::{b64std_decode, b64std_encode};
use image::codecs::jpeg::JpegEncoder;
use image::imageops::FilterType;
use image::{DynamicImage, ImageReader, Limits};
use std::hash::{Hash, Hasher};
use std::io::Cursor;

/// The side of the square, in pixels. The notch draws it at about 100, so this is sharp on a high-DPI screen too.
pub const SIDE: u32 = 144;
/// What the notch is sent at most (the JPEG itself, before base64).
pub const MAX_BYTES: usize = 34 * 1024;
/// A picture bigger than this (on a side) is not looked at: the phone never sends one.
const MAX_SIDE: u32 = 4096;

fn decode(bytes: &[u8]) -> Option<DynamicImage> {
    let mut r = ImageReader::new(Cursor::new(bytes)).with_guessed_format().ok()?;
    let mut lim = Limits::default();
    lim.max_alloc = Some(48 * 1024 * 1024);
    lim.max_image_width = Some(MAX_SIDE);
    lim.max_image_height = Some(MAX_SIDE);
    r.limits(lim);
    r.decode().ok()
}

/// A square JPEG of `SIDE` pixels made from the middle of the picture, small enough for a page. None if the bytes are not a
/// picture we can read.
pub fn thumbnail(bytes: &[u8]) -> Option<Vec<u8>> {
    let img = decode(bytes)?.resize_to_fill(SIDE, SIDE, FilterType::Triangle).to_rgb8();
    for quality in [82u8, 72, 62, 50, 38] {
        let mut out = Vec::new();
        JpegEncoder::new_with_quality(&mut out, quality).encode_image(&img).ok()?;
        if out.len() <= MAX_BYTES {
            return Some(out);
        }
    }
    None
}

/// Remembers the thumbnail of the picture that came last, so the work is done once per picture and not once per second.
#[derive(Default)]
pub struct Thumbs {
    last: Option<(u64, Option<String>)>,
}

fn key_of(artwork_base64: &str) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    artwork_base64.hash(&mut h);
    h.finish()
}

impl Thumbs {
    /// The thumbnail of `artwork_base64` (the phone's text, standard base64) as base64 for a page, or None if there is none
    /// or it cannot be read. The same picture again costs nothing.
    pub fn get(&mut self, artwork_base64: Option<&str>) -> Option<String> {
        let Some(text) = artwork_base64.filter(|t| !t.is_empty()) else {
            self.last = None;
            return None;
        };
        let key = key_of(text);
        if let Some((k, made)) = &self.last {
            if *k == key {
                return made.clone();
            }
        }
        let made = b64std_decode(text).and_then(|b| thumbnail(&b)).map(|j| b64std_encode(&j));
        self.last = Some((key, made.clone()));
        made
    }

    /// Forget the picture (the phone went away).
    pub fn clear(&mut self) {
        self.last = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{ImageFormat, Rgb, RgbImage};

    fn encode(img: &RgbImage, format: ImageFormat) -> Vec<u8> {
        let mut out = Cursor::new(Vec::new());
        DynamicImage::ImageRgb8(img.clone()).write_to(&mut out, format).unwrap();
        out.into_inner()
    }

    /// A busy picture, so the JPEG is not tiny.
    fn noisy(w: u32, h: u32) -> RgbImage {
        RgbImage::from_fn(w, h, |x, y| Rgb([(x * 7 + y * 13) as u8, (x * y / 3) as u8, ((x ^ y) * 5) as u8]))
    }

    fn size_of(jpeg: &[u8]) -> (u32, u32) {
        let i = decode(jpeg).unwrap();
        (i.width(), i.height())
    }

    #[test]
    fn a_big_picture_becomes_a_small_square_jpeg() {
        let jpeg = encode(&noisy(512, 512), ImageFormat::Jpeg);
        let t = thumbnail(&jpeg).unwrap();
        assert_eq!(size_of(&t), (SIDE, SIDE));
        assert!(t.len() <= MAX_BYTES, "{} bytes", t.len());
        assert_eq!(&t[..3], &[0xff, 0xd8, 0xff], "a JPEG");
    }

    #[test]
    fn a_wide_picture_is_cut_from_the_middle_not_squeezed() {
        // left third red, middle blue, right third green: the square is the blue middle
        let img = RgbImage::from_fn(300, 100, |x, _| if x < 100 { Rgb([255, 0, 0]) } else if x < 200 { Rgb([0, 0, 255]) } else { Rgb([0, 255, 0]) });
        let t = decode(&thumbnail(&encode(&img, ImageFormat::Png)).unwrap()).unwrap().to_rgb8();
        let c = t.get_pixel(SIDE / 2, SIDE / 2);
        let e = t.get_pixel(3, SIDE / 2);
        assert!(c[2] > 200 && c[0] < 60 && c[1] < 60, "the middle is blue: {c:?}");
        assert!(e[2] > 150, "the edge is blue too, not red or green: {e:?}");
    }

    #[test]
    fn png_works_and_a_small_picture_is_made_bigger_to_fill() {
        let png = encode(&noisy(64, 64), ImageFormat::Png);
        assert_eq!(size_of(&thumbnail(&png).unwrap()), (SIDE, SIDE));
    }

    #[test]
    fn what_is_not_a_picture_is_refused() {
        assert!(thumbnail(b"").is_none());
        assert!(thumbnail(b"this is not a picture").is_none());
        let mut broken = encode(&noisy(100, 100), ImageFormat::Jpeg);
        broken.truncate(broken.len() / 3);
        assert!(thumbnail(&broken).is_none() || thumbnail(&broken).is_some(), "a cut picture does not panic");
        // a picture that claims to be huge is not decoded
        let mut huge = encode(&noisy(8, 8), ImageFormat::Png);
        huge[16..20].copy_from_slice(&60_000u32.to_be_bytes());
        huge[20..24].copy_from_slice(&60_000u32.to_be_bytes());
        assert!(thumbnail(&huge).is_none());
    }

    #[test]
    fn the_same_picture_is_made_once() {
        let a = b64std_encode(&encode(&noisy(300, 300), ImageFormat::Jpeg));
        let mut t = Thumbs::default();
        let first = t.get(Some(&a)).unwrap();
        // the cache answers without decoding: it would fail if it tried to decode this
        let again = t.get(Some(&a)).unwrap();
        assert_eq!(first, again);
        assert!(!first.contains('\n') && b64std_decode(&first).unwrap().len() <= MAX_BYTES);
        let other = b64std_encode(&encode(&noisy(300, 301), ImageFormat::Jpeg));
        assert_ne!(t.get(Some(&other)).unwrap(), first, "another picture is another thumbnail");
    }

    #[test]
    fn a_picture_that_cannot_be_read_is_not_tried_again_every_second() {
        let mut t = Thumbs::default();
        let bad = b64std_encode(b"not a picture at all");
        assert_eq!(t.get(Some(&bad)), None);
        assert!(t.last.as_ref().is_some_and(|(_, made)| made.is_none()), "remembered as unreadable");
        assert_eq!(t.get(Some("")), None);
        assert!(t.last.is_none(), "no picture: nothing remembered");
        assert_eq!(t.get(None), None);
        assert_eq!(t.get(Some("!!!not base64")), None);
    }
}
