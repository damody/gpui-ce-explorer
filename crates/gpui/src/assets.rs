use crate::{DevicePixels, Pixels, Result, SharedString, Size, size};
use smallvec::SmallVec;

use image::{Delay, Frame};
use std::{
    borrow::Cow,
    fmt,
    hash::Hash,
    sync::atomic::{AtomicU64, AtomicUsize, Ordering::SeqCst},
};

static BC7_ICON_GPU_LIMIT: AtomicU64 = AtomicU64::new(32 * 1024 * 1024);
static BC7_THUMBNAIL_GPU_LIMIT: AtomicU64 = AtomicU64::new(128 * 1024 * 1024);
static BC7_ICON_GPU_USED: AtomicU64 = AtomicU64::new(0);
static BC7_THUMBNAIL_GPU_USED: AtomicU64 = AtomicU64::new(0);
static BC7_ICON_GPU_ENTRIES: AtomicU64 = AtomicU64::new(0);
static BC7_THUMBNAIL_GPU_ENTRIES: AtomicU64 = AtomicU64::new(0);
static BC7_ICON_GPU_UPLOADS: AtomicU64 = AtomicU64::new(0);
static BC7_THUMBNAIL_GPU_UPLOADS: AtomicU64 = AtomicU64::new(0);
static BC7_ICON_GPU_EVICTIONS: AtomicU64 = AtomicU64::new(0);
static BC7_THUMBNAIL_GPU_EVICTIONS: AtomicU64 = AtomicU64::new(0);
static BC7_GPU_CAPABILITY: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(0);

/// Current compressed GPU cache usage for one ownership class.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct CompressedGpuCacheStats {
    /// Bytes currently owned by GPU resources.
    pub bytes: u64,
    /// Independent byte budget for this content kind.
    pub limit_bytes: u64,
    /// Number of resident GPU entries.
    pub entries: u64,
    /// Successful direct BC7 block-row uploads.
    pub uploads: u64,
    /// LRU evictions caused by this ownership class exceeding its byte limit.
    pub evictions: u64,
    /// `None` before adapter discovery, otherwise native BC7 sampling support.
    pub supported: Option<bool>,
}

/// Updates independent BC7 GPU limits. Reductions are enforced on the next atlas access.
pub fn set_compressed_gpu_cache_limits(icon_bytes: u64, thumbnail_bytes: u64) {
    BC7_ICON_GPU_LIMIT.store(icon_bytes.max(1024 * 1024), SeqCst);
    BC7_THUMBNAIL_GPU_LIMIT.store(thumbnail_bytes.max(1024 * 1024), SeqCst);
}

/// Returns independent icon and thumbnail BC7 GPU cache statistics.
pub fn compressed_gpu_cache_stats() -> (CompressedGpuCacheStats, CompressedGpuCacheStats) {
    let supported = match BC7_GPU_CAPABILITY.load(SeqCst) {
        1 => Some(true),
        2 => Some(false),
        _ => None,
    };
    (
        CompressedGpuCacheStats {
            bytes: BC7_ICON_GPU_USED.load(SeqCst),
            limit_bytes: BC7_ICON_GPU_LIMIT.load(SeqCst),
            entries: BC7_ICON_GPU_ENTRIES.load(SeqCst),
            uploads: BC7_ICON_GPU_UPLOADS.load(SeqCst),
            evictions: BC7_ICON_GPU_EVICTIONS.load(SeqCst),
            supported,
        },
        CompressedGpuCacheStats {
            bytes: BC7_THUMBNAIL_GPU_USED.load(SeqCst),
            limit_bytes: BC7_THUMBNAIL_GPU_LIMIT.load(SeqCst),
            entries: BC7_THUMBNAIL_GPU_ENTRIES.load(SeqCst),
            uploads: BC7_THUMBNAIL_GPU_UPLOADS.load(SeqCst),
            evictions: BC7_THUMBNAIL_GPU_EVICTIONS.load(SeqCst),
            supported,
        },
    )
}

#[doc(hidden)]
pub fn record_compressed_gpu_capability(supported: bool) {
    BC7_GPU_CAPABILITY.store(if supported { 1 } else { 2 }, SeqCst);
}

#[doc(hidden)]
pub fn compressed_gpu_cache_limit(kind: CompressedRasterKind) -> u64 {
    match kind {
        CompressedRasterKind::Icon => BC7_ICON_GPU_LIMIT.load(SeqCst),
        CompressedRasterKind::Thumbnail => BC7_THUMBNAIL_GPU_LIMIT.load(SeqCst),
    }
}

#[doc(hidden)]
pub fn record_compressed_gpu_cache(kind: CompressedRasterKind, bytes: u64, entries: u64) {
    match kind {
        CompressedRasterKind::Icon => {
            BC7_ICON_GPU_USED.store(bytes, SeqCst);
            BC7_ICON_GPU_ENTRIES.store(entries, SeqCst);
        }
        CompressedRasterKind::Thumbnail => {
            BC7_THUMBNAIL_GPU_USED.store(bytes, SeqCst);
            BC7_THUMBNAIL_GPU_ENTRIES.store(entries, SeqCst);
        }
    }
}

#[doc(hidden)]
pub fn record_compressed_gpu_upload(kind: CompressedRasterKind) {
    match kind {
        CompressedRasterKind::Icon => BC7_ICON_GPU_UPLOADS.fetch_add(1, SeqCst),
        CompressedRasterKind::Thumbnail => BC7_THUMBNAIL_GPU_UPLOADS.fetch_add(1, SeqCst),
    };
}

#[doc(hidden)]
pub fn record_compressed_gpu_eviction(kind: CompressedRasterKind) {
    match kind {
        CompressedRasterKind::Icon => BC7_ICON_GPU_EVICTIONS.fetch_add(1, SeqCst),
        CompressedRasterKind::Thumbnail => BC7_THUMBNAIL_GPU_EVICTIONS.fetch_add(1, SeqCst),
    };
}

/// A source of assets for this app to use.
pub trait AssetSource: 'static + Send + Sync {
    /// Load the given asset from the source path.
    fn load(&self, path: &str) -> Result<Option<Cow<'static, [u8]>>>;

    /// List the assets at the given path.
    fn list(&self, path: &str) -> Result<Vec<SharedString>>;
}

impl AssetSource for () {
    fn load(&self, _path: &str) -> Result<Option<Cow<'static, [u8]>>> {
        Ok(None)
    }

    fn list(&self, _path: &str) -> Result<Vec<SharedString>> {
        Ok(vec![])
    }
}

/// A unique identifier for the image cache
#[derive(Copy, Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub struct ImageId(pub usize);

#[derive(PartialEq, Eq, Hash, Clone)]
#[expect(missing_docs)]
pub struct RenderImageParams {
    pub image_id: ImageId,
    pub frame_index: usize,
    pub compressed_bc7_srgb: Option<CompressedRasterKind>,
}

/// Ownership class for independent compressed-raster budgets.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum CompressedRasterKind {
    /// Shell icons.
    Icon,
    /// Shell thumbnails.
    Thumbnail,
}

/// Immutable block-compressed raster data. The current Windows backend accepts BC7 UNORM.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CompressedRaster {
    /// Independent cache ownership class.
    pub kind: CompressedRasterKind,
    /// Logical image width, excluding block padding.
    pub width: u32,
    /// Logical image height, excluding block padding.
    pub height: u32,
    /// Four-pixel-aligned BC7 width.
    pub padded_width: u32,
    /// Four-pixel-aligned BC7 height.
    pub padded_height: u32,
    /// Bytes in one complete BC7 block row.
    pub row_pitch: u32,
    /// Complete BC7 block rows.
    pub blocks: std::sync::Arc<[u8]>,
}

/// A cached and processed image, in BGRA format
pub struct RenderImage {
    /// The ID associated with this image
    pub id: ImageId,
    /// The scale factor of this image on render.
    pub(crate) scale_factor: f32,
    data: SmallVec<[Frame; 1]>,
    compressed: Option<CompressedRaster>,
}

impl PartialEq for RenderImage {
    fn eq(&self, other: &Self) -> bool {
        self.id == other.id
    }
}

impl Eq for RenderImage {}

impl RenderImage {
    /// Create a new image from the given data.
    pub fn new(data: impl Into<SmallVec<[Frame; 1]>>) -> Self {
        static NEXT_ID: AtomicUsize = AtomicUsize::new(0);

        Self {
            id: ImageId(NEXT_ID.fetch_add(1, SeqCst)),
            scale_factor: 1.0,
            data: data.into(),
            compressed: None,
        }
    }

    /// Creates an immutable BC7 image after validating complete 4x4 block rows.
    pub fn new_bc7_srgb(raster: CompressedRaster) -> Option<Self> {
        static NEXT_ID: AtomicUsize = AtomicUsize::new(0x4000_0000);
        let padded_width = raster.width.checked_add(3)? & !3;
        let padded_height = raster.height.checked_add(3)? & !3;
        let row_pitch = padded_width.checked_div(4)?.checked_mul(16)?;
        let expected = usize::try_from(row_pitch)
            .ok()?
            .checked_mul(usize::try_from(padded_height / 4).ok()?)?;
        if raster.width == 0
            || raster.height == 0
            || raster.padded_width != padded_width
            || raster.padded_height != padded_height
            || raster.row_pitch != row_pitch
            || raster.blocks.len() != expected
        {
            return None;
        }
        Some(Self {
            id: ImageId(NEXT_ID.fetch_add(1, SeqCst)),
            scale_factor: 1.0,
            data: SmallVec::new(),
            compressed: Some(raster),
        })
    }

    /// Returns the compressed raster when this image bypasses RGBA atlas admission.
    pub fn compressed_raster(&self) -> Option<&CompressedRaster> {
        self.compressed.as_ref()
    }

    /// Convert this image into a byte slice.
    pub fn as_bytes(&self, frame_index: usize) -> Option<&[u8]> {
        if let Some(compressed) = &self.compressed {
            return (frame_index == 0).then_some(compressed.blocks.as_ref());
        }
        self.data
            .get(frame_index)
            .map(|frame| frame.buffer().as_raw().as_slice())
    }

    /// Get the size of this image, in pixels.
    pub fn size(&self, frame_index: usize) -> Size<DevicePixels> {
        if let Some(compressed) = &self.compressed {
            return if frame_index == 0 {
                size(compressed.width.into(), compressed.height.into())
            } else {
                Size::default()
            };
        }
        self.data
            .get(frame_index)
            .map(|frame| {
                let (width, height) = frame.buffer().dimensions();
                size(width.into(), height.into())
            })
            .unwrap_or_default()
    }

    /// Get the size of this image, in pixels for display, adjusted for the scale factor.
    pub(crate) fn render_size(&self, frame_index: usize) -> Size<Pixels> {
        self.size(frame_index)
            .map(|v| (v.0 as f32 / self.scale_factor).into())
    }

    /// Get the delay of this frame from the previous
    pub fn delay(&self, frame_index: usize) -> Delay {
        self.data
            .get(frame_index)
            .map(|frame| frame.delay())
            .unwrap_or(Delay::from_numer_denom_ms(100, 1))
    }

    /// Get the number of frames for this image.
    pub fn frame_count(&self) -> usize {
        if self.compressed.is_some() {
            1
        } else {
            self.data.len()
        }
    }
}

impl fmt::Debug for RenderImage {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ImageData")
            .field("id", &self.id)
            .field("size", &self.data.first().map(|f| f.buffer().dimensions()))
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compressed_gpu_instrumentation_is_independent_by_kind() {
        let (icon_before, thumbnail_before) = compressed_gpu_cache_stats();
        record_compressed_gpu_upload(CompressedRasterKind::Icon);
        record_compressed_gpu_eviction(CompressedRasterKind::Thumbnail);
        let (icon_after, thumbnail_after) = compressed_gpu_cache_stats();
        assert_eq!(icon_after.uploads, icon_before.uploads + 1);
        assert_eq!(icon_after.evictions, icon_before.evictions);
        assert_eq!(thumbnail_after.uploads, thumbnail_before.uploads);
        assert_eq!(thumbnail_after.evictions, thumbnail_before.evictions + 1);
    }
    use smallvec::SmallVec;

    #[test]
    fn empty_render_image_does_not_panic() {
        let image = RenderImage::new(SmallVec::new());
        assert_eq!(image.frame_count(), 0);
        assert_eq!(image.size(0), Size::default());
        assert_eq!(image.as_bytes(0), None);
        assert_eq!(image.render_size(0), Size::default());
        assert_eq!(image.delay(0), Delay::from_numer_denom_ms(100, 1));
        let _ = format!("{image:?}");
    }

    #[test]
    fn bc7_render_image_preserves_logical_size_and_complete_block_rows() {
        let image = RenderImage::new_bc7_srgb(CompressedRaster {
            kind: CompressedRasterKind::Thumbnail,
            width: 5,
            height: 7,
            padded_width: 8,
            padded_height: 8,
            row_pitch: 32,
            blocks: vec![0_u8; 64].into(),
        })
        .expect("valid BC7 raster");
        assert_eq!(image.size(0), size(5.into(), 7.into()));
        assert_eq!(image.as_bytes(0).map(<[u8]>::len), Some(64));
        assert_eq!(image.frame_count(), 1);
        assert!(
            RenderImage::new_bc7_srgb(CompressedRaster {
                kind: CompressedRasterKind::Icon,
                width: 5,
                height: 7,
                padded_width: 8,
                padded_height: 8,
                row_pitch: 32,
                blocks: vec![0_u8; 63].into(),
            })
            .is_none()
        );
    }
}
