//! Validate complete image copies before publishing any pasteboard representation.
use anyhow::{bail, ensure, Result};
use image::{ImageEncoder, ImageReader};
use std::future::Future;
use std::io::{Cursor, Write};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

pub const MAX_IMAGE_BYTES: usize = 50 * 1024 * 1024;
const MAX_IMAGE_FILES: usize = 16;

pub fn is_image_name(name: &str) -> bool {
    name.rsplit_once('.').is_some_and(|(_, extension)| {
        matches!(
            extension.to_ascii_lowercase().as_str(),
            "png" | "jpg" | "jpeg" | "bmp" | "tif" | "tiff" | "gif" | "webp"
        )
    })
}

#[derive(Clone, Debug)]
pub struct CopyTicket {
    epoch: Arc<AtomicU64>,
    generation: u64,
    expected_cc: i64,
}
impl CopyTicket {
    pub fn new(epoch: Arc<AtomicU64>, expected_cc: i64) -> Self {
        let generation = epoch.load(Ordering::Acquire);
        Self {
            epoch,
            generation,
            expected_cc,
        }
    }
    pub fn is_current(&self) -> bool {
        self.epoch.load(Ordering::Acquire) == self.generation
    }
    pub async fn superseded(&self) {
        while self.is_current() {
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
    }
    pub fn may_publish(&self, current_cc: i64) -> bool {
        self.is_current() && self.expected_cc == current_cc
    }
}

#[derive(Clone, Copy)]
pub struct ImageSpec {
    pub index: i32,
    pub size: Option<u64>,
}

struct LimitedPng(Vec<u8>);
impl Write for LimitedPng {
    fn write(&mut self, data: &[u8]) -> std::io::Result<usize> {
        if self.0.len().saturating_add(data.len()) > MAX_IMAGE_BYTES {
            return Err(std::io::Error::other(
                "normalized image exceeds clipboard limit",
            ));
        }
        self.0.extend_from_slice(data);
        Ok(data.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

pub fn normalize_image(bytes: &[u8]) -> Result<Vec<u8>> {
    ensure!(
        !bytes.is_empty() && bytes.len() <= MAX_IMAGE_BYTES,
        "image byte limit exceeded"
    );
    let dimensions = ImageReader::new(Cursor::new(bytes))
        .with_guessed_format()?
        .into_dimensions()?;
    ensure!(
        dimensions.0 > 0
            && dimensions.1 > 0
            && u64::from(dimensions.0) * u64::from(dimensions.1) <= 16 * 1024 * 1024,
        "decoded image pixel limit exceeded"
    );
    let mut reader = ImageReader::new(Cursor::new(bytes)).with_guessed_format()?;
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(8192);
    limits.max_image_height = Some(8192);
    limits.max_alloc = Some(128 * 1024 * 1024);
    reader.limits(limits);
    let image = reader.decode()?;
    // Decode even PNG input: a signature alone does not establish completeness.
    if image::guess_format(bytes)? == image::ImageFormat::Png {
        return Ok(bytes.to_vec());
    }
    let rgba = image.to_rgba8();
    let mut png = LimitedPng(Vec::new());
    image::codecs::png::PngEncoder::new(&mut png).write_image(
        &rgba,
        rgba.width(),
        rgba.height(),
        image::ExtendedColorType::Rgba8,
    )?;
    Ok(png.0)
}

/// The caller can publish only the successful complete result. No placeholders
/// escape this boundary. Decode work never blocks the async network worker.
pub async fn prepare_images<F, Fut>(
    specs: Vec<ImageSpec>,
    ticket: &CopyTicket,
    mut fetch: F,
) -> Result<Vec<Vec<u8>>>
where
    F: FnMut(ImageSpec, u64) -> Fut,
    Fut: Future<Output = Result<Vec<u8>>>,
{
    ensure!(
        !specs.is_empty() && specs.len() <= MAX_IMAGE_FILES,
        "image file count limit exceeded"
    );
    let mut remaining = MAX_IMAGE_BYTES as u64;
    let mut output_size = 0usize;
    let mut images = Vec::new();
    for spec in specs {
        ensure!(ticket.is_current(), "clipboard copy superseded");
        if spec.size.is_some_and(|size| size == 0 || size > remaining) {
            bail!("image file byte limit exceeded");
        }
        let bytes = fetch(spec, remaining).await?;
        ensure!(
            bytes.len() as u64 <= remaining
                && spec.size.is_none_or(|size| size == bytes.len() as u64),
            "image download length mismatch"
        );
        remaining -= bytes.len() as u64;
        ensure!(ticket.is_current(), "clipboard copy superseded");
        let png = tokio::task::spawn_blocking(move || normalize_image(&bytes)).await??;
        output_size = output_size.saturating_add(png.len());
        ensure!(
            output_size <= MAX_IMAGE_BYTES && ticket.is_current(),
            "image copy superseded or normalized byte limit exceeded"
        );
        images.push(png);
    }
    Ok(images)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn png() -> Vec<u8> {
        let mut out = Vec::new();
        image::codecs::png::PngEncoder::new(&mut out)
            .write_image(&[13, 98, 171, 255], 1, 1, image::ExtendedColorType::Rgba8)
            .unwrap();
        out
    }
    fn ticket() -> CopyTicket {
        CopyTicket::new(Arc::new(AtomicU64::new(1)), 42)
    }
    #[test]
    fn rejects_all_zero_and_truncated_images_but_preserves_valid_pixels() {
        assert!(normalize_image(&vec![0; 1727266]).is_err());
        assert!(normalize_image(&png()[..25]).is_err());
        let image = image::load_from_memory(&normalize_image(&png()).unwrap())
            .unwrap()
            .to_rgba8();
        assert_eq!(image.get_pixel(0, 0).0, [13, 98, 171, 255]);
    }
    #[test]
    fn jpeg_file_is_validated_and_normalized_to_png() {
        let mut jpeg = Cursor::new(Vec::new());
        image::DynamicImage::new_rgb8(2, 2)
            .write_to(&mut jpeg, image::ImageFormat::Jpeg)
            .unwrap();
        let png = normalize_image(jpeg.get_ref()).unwrap();
        assert!(png.starts_with(b"\x89PNG\r\n\x1a\n"));
        assert_eq!(image::load_from_memory(&png).unwrap().width(), 2);
    }
    #[test]
    fn local_copy_or_generation_change_blocks_publication() {
        let t = ticket();
        assert!(t.may_publish(42));
        assert!(!t.may_publish(43));
        t.epoch.fetch_add(1, Ordering::Release);
        assert!(!t.may_publish(42));
        assert!(is_image_name("Screenshot.PNG"));
        assert!(!is_image_name("photo.png.txt"));
    }
    #[tokio::test]
    async fn no_result_is_available_until_download_finishes_and_decodes() {
        let (release, wait) = tokio::sync::oneshot::channel();
        let t = ticket();
        let worker = tokio::spawn(async move {
            let mut wait = Some(wait);
            prepare_images(
                vec![ImageSpec {
                    index: 7,
                    size: None,
                }],
                &t,
                |spec, _| {
                    assert_eq!(spec.index, 7);
                    let wait = wait.take().unwrap();
                    async move {
                        wait.await?;
                        Ok(png())
                    }
                },
            )
            .await
        });
        tokio::task::yield_now().await;
        assert!(!worker.is_finished());
        release.send(()).unwrap();
        assert_eq!(worker.await.unwrap().unwrap().len(), 1);
    }
    #[tokio::test]
    async fn failure_short_read_and_superseded_copy_never_return_publishable_images() {
        let spec = ImageSpec {
            index: 0,
            size: Some(png().len() as u64 + 1),
        };
        assert!(
            prepare_images(vec![spec], &ticket(), |_, _| async { Ok(png()) })
                .await
                .is_err()
        );
        assert!(prepare_images(
            vec![ImageSpec { size: None, ..spec }],
            &ticket(),
            |_, _| async { anyhow::bail!("download failed") }
        )
        .await
        .is_err());
        let t = ticket();
        let epoch = t.epoch.clone();
        assert!(
            prepare_images(vec![ImageSpec { size: None, ..spec }], &t, |_, _| {
                epoch.fetch_add(1, Ordering::Release);
                async { Ok(png()) }
            })
            .await
            .is_err()
        );
    }
    #[tokio::test]
    async fn oversized_hints_and_batches_are_rejected_before_fetch() {
        let spec = ImageSpec {
            index: 0,
            size: Some(MAX_IMAGE_BYTES as u64 + 1),
        };
        assert!(prepare_images(vec![spec], &ticket(), |_, _| async {
            panic!("must not fetch")
        })
        .await
        .is_err());
        assert!(prepare_images(vec![spec; 17], &ticket(), |_, _| async {
            panic!("must not fetch")
        })
        .await
        .is_err());
    }
}
