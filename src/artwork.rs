use std::io::{Cursor, Read};

use anyhow::{bail, Context, Result};
use image::imageops::FilterType;
use image::ImageReader;
use url::Url;

const MAX_DOWNLOAD_BYTES: u64 = 10 * 1024 * 1024;
const FETCH_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(15);
/// Decode-time cap on source dimensions, so a crafted tiny file cannot
/// expand into a huge pixel buffer.
const MAX_SOURCE_DIM: u32 = 16384;

/// Fetches artwork bytes from an MPRIS artUrl: `file://` URLs (browser
/// caches, local players), `http(s)://` URLs (Spotify CDN), or a bare
/// filesystem path.
pub fn load(url: &str) -> Result<Vec<u8>> {
    if url.starts_with("file://") {
        // Url::to_file_path handles percent-decoding into raw OS bytes
        // (non-UTF-8 paths are legal on Linux) and the localhost authority.
        let parsed = Url::parse(url).with_context(|| format!("invalid artUrl {url}"))?;
        let path = parsed
            .to_file_path()
            .ok()
            .with_context(|| format!("unsupported file URL: {url}"))?;
        std::fs::read(&path).with_context(|| format!("failed to read {}", path.display()))
    } else if url.starts_with("http://") || url.starts_with("https://") {
        let agent = ureq::AgentBuilder::new().timeout(FETCH_TIMEOUT).build();
        let response = agent
            .get(url)
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
/// Non-square art is center-cropped BEFORE resizing: crop-then-scale never
/// materializes the huge intermediate that scale-to-fill would allocate for
/// extreme aspect ratios.
pub fn rgb888_scaled(bytes: &[u8], size: u32) -> Result<Vec<u8>> {
    let mut reader = ImageReader::new(Cursor::new(bytes))
        .with_guessed_format()
        .context("failed to sniff artwork format")?;
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(MAX_SOURCE_DIM);
    limits.max_image_height = Some(MAX_SOURCE_DIM);
    reader.limits(limits);
    let img = reader.decode().context("failed to decode artwork")?;
    let (w, h) = (img.width(), img.height());
    let side = w.min(h).max(1);
    let img = img.crop_imm((w - side) / 2, (h - side) / 2, side, side);
    let img = img.resize_exact(size, size, FilterType::Lanczos3);
    Ok(img.to_rgb8().into_raw())
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{ImageFormat, RgbImage};

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
    fn extreme_aspect_ratio_does_not_blow_up() {
        // Crop-first keeps this cheap; scale-to-fill would have allocated a
        // 64 x 640000 intermediate.
        let rgb = rgb888_scaled(&png_bytes(1, 10000), 64).unwrap();
        assert_eq!(rgb.len(), 64 * 64 * 3);
    }

    #[test]
    fn oversized_dimensions_are_rejected_not_allocated() {
        assert!(rgb888_scaled(&png_bytes(1, 20000), 64).is_err());
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
    fn loads_file_url_with_non_utf8_path() {
        use std::ffi::OsStr;
        use std::os::unix::ffi::OsStrExt;
        let dir = std::env::temp_dir().join("pixoo-nowplaying-test");
        std::fs::create_dir_all(&dir).unwrap();
        // Latin-1 0xFC ("ü") — a legal Linux filename that is not UTF-8.
        let path = dir.join(OsStr::from_bytes(b"M\xFCsik.png"));
        std::fs::write(&path, b"data").unwrap();
        let url = format!("file://{}/M%FCsik.png", dir.display());
        assert_eq!(load(&url).unwrap(), b"data");
        std::fs::remove_file(&path).unwrap();
    }

    #[test]
    fn loads_file_url_with_localhost_authority() {
        let dir = std::env::temp_dir().join("pixoo-nowplaying-test");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("local.png");
        std::fs::write(&path, b"data").unwrap();
        let url = format!("file://localhost{}/local.png", dir.display());
        assert_eq!(load(&url).unwrap(), b"data");
        std::fs::remove_file(&path).unwrap();
    }

    #[test]
    fn rejects_unknown_scheme() {
        assert!(load("ftp://example.com/a.png").is_err());
    }
}
