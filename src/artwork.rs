use std::io::Read;

use anyhow::{bail, Context, Result};
use image::imageops::FilterType;

const MAX_DOWNLOAD_BYTES: u64 = 10 * 1024 * 1024;

/// Fetches artwork bytes from an MPRIS artUrl: `file://` paths (browser
/// caches), `http(s)://` URLs (Spotify CDN), or a bare filesystem path.
pub fn load(url: &str) -> Result<Vec<u8>> {
    if let Some(path) = url.strip_prefix("file://") {
        let path = percent_decode(path);
        std::fs::read(&path).with_context(|| format!("failed to read {path}"))
    } else if url.starts_with("http://") || url.starts_with("https://") {
        let response = ureq::get(url)
            .call()
            .with_context(|| format!("failed to fetch {url}"))?;
        let mut bytes = Vec::new();
        response
            .into_reader()
            .take(MAX_DOWNLOAD_BYTES)
            .read_to_end(&mut bytes)
            .context("failed to read artwork body")?;
        Ok(bytes)
    } else if url.starts_with('/') {
        std::fs::read(url).with_context(|| format!("failed to read {url}"))
    } else {
        bail!("unsupported artUrl scheme: {url}");
    }
}

/// Decodes image bytes and scales them to a size x size raw RGB888 buffer.
/// Non-square art is center-cropped.
pub fn rgb888_scaled(bytes: &[u8], size: u32) -> Result<Vec<u8>> {
    let img = image::load_from_memory(bytes).context("failed to decode artwork")?;
    let img = img.resize_to_fill(size, size, FilterType::Lanczos3);
    Ok(img.to_rgb8().into_raw())
}

fn percent_decode(s: &str) -> String {
    let mut out = Vec::with_capacity(s.len());
    let mut bytes = s.bytes();
    while let Some(b) = bytes.next() {
        if b == b'%' {
            let hi = bytes.next();
            let lo = bytes.next();
            match (hi, lo) {
                (Some(h), Some(l)) => {
                    let hex = [h, l];
                    match u8::from_str_radix(std::str::from_utf8(&hex).unwrap_or(""), 16) {
                        Ok(v) => out.push(v),
                        Err(_) => {
                            out.push(b'%');
                            out.push(h);
                            out.push(l);
                        }
                    }
                }
                (Some(h), None) => {
                    out.push(b'%');
                    out.push(h);
                }
                (None, _) => out.push(b'%'),
            }
        } else {
            out.push(b);
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{ImageFormat, RgbImage};
    use std::io::Cursor;

    fn png_bytes(width: u32, height: u32) -> Vec<u8> {
        let img = RgbImage::from_fn(width, height, |x, _| image::Rgb([x as u8, 0, 255]));
        let mut buf = Cursor::new(Vec::new());
        img.write_to(&mut buf, ImageFormat::Png).unwrap();
        buf.into_inner()
    }

    #[test]
    fn scales_square_image_to_rgb888() {
        let rgb = rgb888_scaled(&png_bytes(300, 300), 64).unwrap();
        assert_eq!(rgb.len(), 64 * 64 * 3);
    }

    #[test]
    fn crops_non_square_image() {
        let rgb = rgb888_scaled(&png_bytes(400, 200), 64).unwrap();
        assert_eq!(rgb.len(), 64 * 64 * 3);
    }

    #[test]
    fn rejects_garbage_bytes() {
        assert!(rgb888_scaled(b"not an image", 64).is_err());
    }

    #[test]
    fn loads_file_url_with_percent_encoding() {
        let dir = std::env::temp_dir().join("pixoo-nowplaying-test");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("album art.png");
        std::fs::write(&path, b"data").unwrap();
        let url = format!("file://{}/album%20art.png", dir.display());
        assert_eq!(load(&url).unwrap(), b"data");
        std::fs::remove_file(&path).unwrap();
    }

    #[test]
    fn rejects_unknown_scheme() {
        assert!(load("ftp://example.com/a.png").is_err());
    }

    #[test]
    fn percent_decode_passthrough_and_escapes() {
        assert_eq!(percent_decode("/plain/path.png"), "/plain/path.png");
        assert_eq!(percent_decode("/a%20b/%E3%81%82.png"), "/a b/あ.png");
        assert_eq!(percent_decode("/broken%2"), "/broken%2");
    }
}
