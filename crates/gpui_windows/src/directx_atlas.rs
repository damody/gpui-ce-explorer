use collections::FxHashMap;
use etagere::BucketedAtlasAllocator;
use parking_lot::Mutex;
use windows::Win32::Graphics::{
    Direct3D11::{
        D3D11_BIND_SHADER_RESOURCE, D3D11_BOX, D3D11_FORMAT_SUPPORT_SHADER_SAMPLE,
        D3D11_FORMAT_SUPPORT_TEXTURE2D, D3D11_TEXTURE2D_DESC, D3D11_USAGE_DEFAULT, ID3D11Device,
        ID3D11DeviceContext, ID3D11ShaderResourceView, ID3D11Texture2D,
    },
    Dxgi::Common::*,
};

use gpui::{
    AtlasBackend, AtlasKey, AtlasState, AtlasTextureId, AtlasTextureKind, AtlasTextureList,
    AtlasTile, Bounds, DevicePixels, PlatformAtlas, Point, Size,
};

pub(crate) struct DirectXAtlas(Mutex<AtlasState<DirectXAtlasTextures>>);

struct DirectXAtlasTextures {
    device: ID3D11Device,
    device_context: ID3D11DeviceContext,
    monochrome_textures: AtlasTextureList<DirectXAtlasTexture>,
    polychrome_textures: AtlasTextureList<DirectXAtlasTexture>,
    subpixel_textures: AtlasTextureList<DirectXAtlasTexture>,
    bc7_icon_textures: AtlasTextureList<DirectXAtlasTexture>,
    bc7_thumbnail_textures: AtlasTextureList<DirectXAtlasTexture>,
    bc7_residency: Bc7Residency,
}

#[derive(Default)]
struct Bc7Residency {
    costs: FxHashMap<AtlasKey, u64>,
    last_used: FxHashMap<AtlasKey, u64>,
    clock: u64,
}

struct DirectXAtlasTexture {
    id: AtlasTextureId,
    bytes_per_pixel: u32,
    bc7: bool,
    allocator: BucketedAtlasAllocator,
    texture: ID3D11Texture2D,
    view: [Option<ID3D11ShaderResourceView>; 1],
    live_atlas_keys: u32,
}

impl DirectXAtlas {
    pub(crate) fn new(device: &ID3D11Device, device_context: &ID3D11DeviceContext) -> Self {
        let required =
            D3D11_FORMAT_SUPPORT_TEXTURE2D.0 as u32 | D3D11_FORMAT_SUPPORT_SHADER_SAMPLE.0 as u32;
        let supported = unsafe { device.CheckFormatSupport(DXGI_FORMAT_BC7_UNORM) }
            .is_ok_and(|flags| flags & required == required);
        gpui::record_compressed_gpu_capability(supported);
        DirectXAtlas(Mutex::new(AtlasState::new(DirectXAtlasTextures {
            device: device.clone(),
            device_context: device_context.clone(),
            monochrome_textures: Default::default(),
            polychrome_textures: Default::default(),
            subpixel_textures: Default::default(),
            bc7_icon_textures: Default::default(),
            bc7_thumbnail_textures: Default::default(),
            bc7_residency: Default::default(),
        })))
    }

    /// Returns the view backing `id`, or `None` once every tile in it has been
    /// removed. A scene can still reference such a texture when a cached view
    /// replays a paint from before the image was dropped, so callers must skip
    /// those sprites rather than assume the texture exists.
    pub(crate) fn get_texture_view(
        &self,
        id: AtlasTextureId,
    ) -> Option<[Option<ID3D11ShaderResourceView>; 1]> {
        let lock = self.0.lock();
        let texture = lock.backend.texture(id)?;
        Some(texture.view.clone())
    }

    pub(crate) fn handle_device_lost(
        &self,
        device: &ID3D11Device,
        device_context: &ID3D11DeviceContext,
    ) {
        let mut lock = self.0.lock();
        lock.clear(|textures| {
            textures.device = device.clone();
            textures.device_context = device_context.clone();
            textures.monochrome_textures = AtlasTextureList::default();
            textures.polychrome_textures = AtlasTextureList::default();
            textures.subpixel_textures = AtlasTextureList::default();
            textures.bc7_icon_textures = AtlasTextureList::default();
            textures.bc7_thumbnail_textures = AtlasTextureList::default();
            textures.bc7_residency = Bc7Residency::default();
            textures.publish_bc7_stats();
        });
    }
}

impl PlatformAtlas for DirectXAtlas {
    fn get_or_insert_with<'a>(
        &self,
        key: AtlasKey,
        build: &mut dyn FnMut() -> anyhow::Result<
            Option<(Size<DevicePixels>, std::borrow::Cow<'a, [u8]>)>,
        >,
    ) -> anyhow::Result<Option<AtlasTile>> {
        self.0.lock().get_or_insert_with(key, build)
    }

    fn get_or_insert_bc7(
        &self,
        key: &AtlasKey,
        size: Size<DevicePixels>,
        padded_size: Size<DevicePixels>,
        row_pitch: u32,
        blocks: &[u8],
    ) -> anyhow::Result<Option<AtlasTile>> {
        if !matches!(
            key.texture_kind(),
            AtlasTextureKind::Bc7Icon | AtlasTextureKind::Bc7Thumbnail
        ) || padded_size.width.0 <= 0
            || padded_size.height.0 <= 0
            || padded_size.width.0 % 4 != 0
            || padded_size.height.0 % 4 != 0
            || row_pitch != (padded_size.width.0 as u32 / 4) * 16
            || blocks.len() != row_pitch as usize * (padded_size.height.0 as usize / 4)
        {
            anyhow::bail!("invalid BC7 block layout");
        }
        let mut lock = self.0.lock();
        let kind = key.texture_kind();
        let owner = DirectXAtlasTextures::bc7_kind(kind)
            .ok_or_else(|| anyhow::anyhow!("invalid BC7 cache kind"))?;
        if blocks.len() as u64 > gpui::compressed_gpu_cache_limit(owner) {
            anyhow::bail!("BC7 raster exceeds its GPU cache budget");
        }
        // GPUI's icon/thumbnail shader path already preserves encoded display values and its
        // existing polychrome atlas is UNORM. Using an sRGB SRV here would linearize the sample a
        // second time and visibly darken Shell assets.
        let format_support = unsafe {
            lock.backend
                .device
                .CheckFormatSupport(DXGI_FORMAT_BC7_UNORM)?
        };
        let required =
            D3D11_FORMAT_SUPPORT_TEXTURE2D.0 as u32 | D3D11_FORMAT_SUPPORT_SHADER_SAMPLE.0 as u32;
        if format_support & required != required {
            anyhow::bail!("D3D11 adapter does not support BC7 sampling");
        }
        let limit = gpui::compressed_gpu_cache_limit(owner);
        while lock.backend.bc7_residency.bytes(kind) > limit {
            let Some(victim) = lock.backend.bc7_residency.least_recent(kind) else {
                break;
            };
            Self::remove_key(&mut lock, &victim, true);
        }
        if let Some(tile) = lock.get(key) {
            lock.backend.bc7_residency.promote(key);
            return Ok(Some(tile));
        }
        while lock
            .backend
            .bc7_residency
            .bytes(kind)
            .saturating_add(blocks.len() as u64)
            > limit
        {
            let Some(victim) = lock.backend.bc7_residency.least_recent(kind) else {
                anyhow::bail!("BC7 GPU cache budget is full");
            };
            Self::remove_key(&mut lock, &victim, true);
        }
        let tile = lock
            .backend
            .allocate_bc7(size, padded_size, kind)
            .ok_or_else(|| anyhow::anyhow!("failed to allocate BC7 atlas tile"))?;
        let textures = &lock.backend;
        let texture = textures
            .texture(tile.texture_id)
            .ok_or_else(|| anyhow::anyhow!("allocated tile refers to a missing texture"))?;
        texture.upload_bc7(
            &textures.device_context,
            tile.bounds.origin,
            padded_size,
            row_pitch,
            blocks,
        );
        gpui::record_compressed_gpu_upload(owner);
        lock.insert_tile(key.clone(), tile);
        lock.backend
            .bc7_residency
            .insert(key.clone(), blocks.len() as u64);
        lock.backend.publish_bc7_stats();
        Ok(Some(tile))
    }

    fn remove(&self, key: &AtlasKey) {
        Self::remove_key(&mut self.0.lock(), key, false);
    }
}

impl DirectXAtlas {
    fn remove_key(state: &mut AtlasState<DirectXAtlasTextures>, key: &AtlasKey, evicted: bool) {
        state.backend.bc7_residency.remove(key);
        if evicted && let Some(owner) = DirectXAtlasTextures::bc7_kind(key.texture_kind()) {
            gpui::record_compressed_gpu_eviction(owner);
        }
        state.remove(key);
        state.backend.publish_bc7_stats();
    }
}

impl AtlasBackend for DirectXAtlasTextures {
    fn insert(
        &mut self,
        kind: AtlasTextureKind,
        size: Size<DevicePixels>,
        bytes: &[u8],
    ) -> anyhow::Result<AtlasTile> {
        let tile = self
            .allocate(size, kind)
            .ok_or_else(|| anyhow::anyhow!("failed to allocate"))?;
        let texture = self
            .texture(tile.texture_id)
            .ok_or_else(|| anyhow::anyhow!("allocated tile refers to a missing texture"))?;
        texture.upload(&self.device_context, tile.bounds, bytes);
        Ok(tile)
    }

    fn remove(&mut self, tile: AtlasTile) {
        let id = tile.texture_id;

        let textures = match id.kind {
            AtlasTextureKind::Monochrome => &mut self.monochrome_textures,
            AtlasTextureKind::Polychrome => &mut self.polychrome_textures,
            AtlasTextureKind::Subpixel => &mut self.subpixel_textures,
            AtlasTextureKind::Bc7Icon => &mut self.bc7_icon_textures,
            AtlasTextureKind::Bc7Thumbnail => &mut self.bc7_thumbnail_textures,
        };

        let Some(texture_slot) = textures.textures.get_mut(id.index as usize) else {
            return;
        };

        if let Some(mut texture) = texture_slot.take() {
            texture.allocator.deallocate(tile.tile_id.into());
            texture.decrement_ref_count();
            if texture.is_unreferenced() {
                textures.free_list.push(texture.id.index as usize);
            } else {
                *texture_slot = Some(texture);
            }
        }
    }
}

impl DirectXAtlasTextures {
    fn bc7_kind(kind: AtlasTextureKind) -> Option<gpui::CompressedRasterKind> {
        match kind {
            AtlasTextureKind::Bc7Icon => Some(gpui::CompressedRasterKind::Icon),
            AtlasTextureKind::Bc7Thumbnail => Some(gpui::CompressedRasterKind::Thumbnail),
            _ => None,
        }
    }

    fn publish_bc7_stats(&self) {
        for (kind, owner) in [
            (AtlasTextureKind::Bc7Icon, gpui::CompressedRasterKind::Icon),
            (
                AtlasTextureKind::Bc7Thumbnail,
                gpui::CompressedRasterKind::Thumbnail,
            ),
        ] {
            let bytes = self.bc7_residency.bytes(kind);
            let entries = self.bc7_residency.entries(kind);
            gpui::record_compressed_gpu_cache(owner, bytes, entries);
        }
    }

    fn allocate(
        &mut self,
        size: Size<DevicePixels>,
        texture_kind: AtlasTextureKind,
    ) -> Option<AtlasTile> {
        {
            let textures = match texture_kind {
                AtlasTextureKind::Monochrome => &mut self.monochrome_textures,
                AtlasTextureKind::Polychrome => &mut self.polychrome_textures,
                AtlasTextureKind::Subpixel => &mut self.subpixel_textures,
                AtlasTextureKind::Bc7Icon => &mut self.bc7_icon_textures,
                AtlasTextureKind::Bc7Thumbnail => &mut self.bc7_thumbnail_textures,
            };

            if let Some(tile) = textures
                .iter_mut()
                .rev()
                .find_map(|texture| texture.allocate(size))
            {
                return Some(tile);
            }
        }

        let texture = self.push_texture(size, texture_kind)?;
        texture.allocate(size)
    }

    fn allocate_bc7(
        &mut self,
        logical_size: Size<DevicePixels>,
        padded_size: Size<DevicePixels>,
        kind: AtlasTextureKind,
    ) -> Option<AtlasTile> {
        let textures = match kind {
            AtlasTextureKind::Bc7Icon => &mut self.bc7_icon_textures,
            AtlasTextureKind::Bc7Thumbnail => &mut self.bc7_thumbnail_textures,
            _ => return None,
        };
        if let Some((texture, allocation)) = textures.iter_mut().rev().find_map(|texture| {
            texture
                .allocator
                .allocate(device_size_to_etagere(padded_size))
                .map(|allocation| (texture, allocation))
        }) {
            return Some(texture.tile_from_allocation(allocation, logical_size));
        }
        let texture = self.push_texture(padded_size, kind)?;
        let allocation = texture
            .allocator
            .allocate(device_size_to_etagere(padded_size))?;
        Some(texture.tile_from_allocation(allocation, logical_size))
    }

    fn push_texture(
        &mut self,
        min_size: Size<DevicePixels>,
        kind: AtlasTextureKind,
    ) -> Option<&mut DirectXAtlasTexture> {
        const DEFAULT_ATLAS_SIZE: Size<DevicePixels> = Size {
            width: DevicePixels(1024),
            height: DevicePixels(1024),
        };
        // Max texture size for DirectX. See:
        // https://learn.microsoft.com/en-us/windows/win32/direct3d11/overviews-direct3d-11-resources-limits
        const MAX_ATLAS_SIZE: Size<DevicePixels> = Size {
            width: DevicePixels(16384),
            height: DevicePixels(16384),
        };
        let size = if matches!(
            kind,
            AtlasTextureKind::Bc7Icon | AtlasTextureKind::Bc7Thumbnail
        ) {
            min_size.min(&MAX_ATLAS_SIZE)
        } else {
            min_size.min(&MAX_ATLAS_SIZE).max(&DEFAULT_ATLAS_SIZE)
        };
        let pixel_format;
        let bind_flag;
        let bytes_per_pixel;
        match kind {
            AtlasTextureKind::Monochrome => {
                pixel_format = DXGI_FORMAT_R8_UNORM;
                bind_flag = D3D11_BIND_SHADER_RESOURCE;
                bytes_per_pixel = 1;
            }
            AtlasTextureKind::Polychrome => {
                pixel_format = DXGI_FORMAT_B8G8R8A8_UNORM;
                bind_flag = D3D11_BIND_SHADER_RESOURCE;
                bytes_per_pixel = 4;
            }
            AtlasTextureKind::Subpixel => {
                pixel_format = DXGI_FORMAT_R8G8B8A8_UNORM;
                bind_flag = D3D11_BIND_SHADER_RESOURCE;
                bytes_per_pixel = 4;
            }
            AtlasTextureKind::Bc7Icon | AtlasTextureKind::Bc7Thumbnail => {
                pixel_format = DXGI_FORMAT_BC7_UNORM;
                bind_flag = D3D11_BIND_SHADER_RESOURCE;
                bytes_per_pixel = 0;
            }
        }
        let texture_desc = D3D11_TEXTURE2D_DESC {
            Width: size.width.0 as u32,
            Height: size.height.0 as u32,
            MipLevels: 1,
            ArraySize: 1,
            Format: pixel_format,
            SampleDesc: DXGI_SAMPLE_DESC {
                Count: 1,
                Quality: 0,
            },
            Usage: D3D11_USAGE_DEFAULT,
            BindFlags: bind_flag.0 as u32,
            CPUAccessFlags: 0,
            MiscFlags: 0,
        };
        let mut texture: Option<ID3D11Texture2D> = None;
        unsafe {
            // This only returns None if the device is lost, which we will recreate later.
            // So it's ok to return None here.
            self.device
                .CreateTexture2D(&texture_desc, None, Some(&mut texture))
                .ok()?;
        }
        let texture = texture.unwrap();

        let texture_list = match kind {
            AtlasTextureKind::Monochrome => &mut self.monochrome_textures,
            AtlasTextureKind::Polychrome => &mut self.polychrome_textures,
            AtlasTextureKind::Subpixel => &mut self.subpixel_textures,
            AtlasTextureKind::Bc7Icon => &mut self.bc7_icon_textures,
            AtlasTextureKind::Bc7Thumbnail => &mut self.bc7_thumbnail_textures,
        };
        let index = texture_list.free_list.pop();
        let view = unsafe {
            let mut view = None;
            self.device
                .CreateShaderResourceView(&texture, None, Some(&mut view))
                .ok()?;
            [view]
        };
        let atlas_texture = DirectXAtlasTexture {
            id: AtlasTextureId {
                index: index.unwrap_or(texture_list.textures.len()) as u32,
                kind,
            },
            bytes_per_pixel,
            bc7: matches!(
                kind,
                AtlasTextureKind::Bc7Icon | AtlasTextureKind::Bc7Thumbnail
            ),
            allocator: etagere::BucketedAtlasAllocator::new(device_size_to_etagere(size)),
            texture,
            view,
            live_atlas_keys: 0,
        };
        if let Some(ix) = index {
            texture_list.textures[ix] = Some(atlas_texture);
            texture_list.textures.get_mut(ix).unwrap().as_mut()
        } else {
            texture_list.textures.push(Some(atlas_texture));
            texture_list.textures.last_mut().unwrap().as_mut()
        }
    }

    fn texture(&self, id: AtlasTextureId) -> Option<&DirectXAtlasTexture> {
        let textures = match id.kind {
            AtlasTextureKind::Monochrome => &self.monochrome_textures,
            AtlasTextureKind::Polychrome => &self.polychrome_textures,
            AtlasTextureKind::Subpixel => &self.subpixel_textures,
            AtlasTextureKind::Bc7Icon => &self.bc7_icon_textures,
            AtlasTextureKind::Bc7Thumbnail => &self.bc7_thumbnail_textures,
        };
        textures.textures.get(id.index as usize)?.as_ref()
    }
}

impl DirectXAtlasTexture {
    fn tile_from_allocation(
        &mut self,
        allocation: etagere::Allocation,
        logical_size: Size<DevicePixels>,
    ) -> AtlasTile {
        self.live_atlas_keys += 1;
        AtlasTile {
            texture_id: self.id,
            tile_id: allocation.id.into(),
            bounds: Bounds {
                origin: etagere_point_to_device(allocation.rectangle.min),
                size: logical_size,
            },
            padding: 0,
        }
    }

    fn allocate(&mut self, size: Size<DevicePixels>) -> Option<AtlasTile> {
        let allocation = self.allocator.allocate(device_size_to_etagere(size))?;
        Some(self.tile_from_allocation(allocation, size))
    }

    fn upload(
        &self,
        device_context: &ID3D11DeviceContext,
        bounds: Bounds<DevicePixels>,
        bytes: &[u8],
    ) {
        debug_assert!(!self.bc7);
        // `UpdateSubresource` reads `row_pitch * height` bytes from `bytes` based on the
        // `D3D11_BOX` below. If the caller hands us a slice shorter than that, the driver would
        // over-read past the end of the source buffer (potentially by multiple megabytes), so bail
        // out instead. This is a first-insert path rather than a per-frame one, so the check is
        // effectively free.
        let row_bytes = bounds.size.width.to_bytes(self.bytes_per_pixel as u8) as usize;
        let expected = row_bytes * bounds.size.height.0.max(0) as usize;
        if bytes.len() < expected {
            log::error!(
                "DirectXAtlasTexture::upload: source slice is {} bytes but the {}x{} region \
                 requires {} bytes; skipping upload to avoid a driver over-read",
                bytes.len(),
                bounds.size.width.0,
                bounds.size.height.0,
                expected,
            );
            return;
        }
        unsafe {
            device_context.UpdateSubresource(
                &self.texture,
                0,
                Some(&D3D11_BOX {
                    left: bounds.left().0 as u32,
                    top: bounds.top().0 as u32,
                    front: 0,
                    right: bounds.right().0 as u32,
                    bottom: bounds.bottom().0 as u32,
                    back: 1,
                }),
                bytes.as_ptr() as _,
                bounds.size.width.to_bytes(self.bytes_per_pixel as u8),
                0,
            );
        }
    }

    fn upload_bc7(
        &self,
        device_context: &ID3D11DeviceContext,
        origin: Point<DevicePixels>,
        padded_size: Size<DevicePixels>,
        row_pitch: u32,
        blocks: &[u8],
    ) {
        debug_assert!(self.bc7);
        unsafe {
            device_context.UpdateSubresource(
                &self.texture,
                0,
                Some(&D3D11_BOX {
                    left: origin.x.0 as u32,
                    top: origin.y.0 as u32,
                    front: 0,
                    right: (origin.x.0 + padded_size.width.0) as u32,
                    bottom: (origin.y.0 + padded_size.height.0) as u32,
                    back: 1,
                }),
                blocks.as_ptr().cast(),
                row_pitch,
                0,
            );
        }
    }

    fn decrement_ref_count(&mut self) {
        self.live_atlas_keys -= 1;
    }

    fn is_unreferenced(&mut self) -> bool {
        self.live_atlas_keys == 0
    }
}

fn device_size_to_etagere(size: Size<DevicePixels>) -> etagere::Size {
    etagere::Size::new(size.width.into(), size.height.into())
}

fn etagere_point_to_device(value: etagere::Point) -> Point<DevicePixels> {
    Point {
        x: DevicePixels::from(value.x),
        y: DevicePixels::from(value.y),
    }
}

impl Bc7Residency {
    fn insert(&mut self, key: AtlasKey, bytes: u64) {
        self.costs.insert(key.clone(), bytes);
        self.promote(&key);
    }

    fn promote(&mut self, key: &AtlasKey) {
        self.clock = self.clock.wrapping_add(1);
        self.last_used.insert(key.clone(), self.clock);
    }

    fn least_recent(&self, kind: AtlasTextureKind) -> Option<AtlasKey> {
        self.last_used
            .iter()
            .filter(|(key, _)| key.texture_kind() == kind)
            .min_by_key(|(_, last_used)| **last_used)
            .map(|(key, _)| key.clone())
    }

    fn bytes(&self, kind: AtlasTextureKind) -> u64 {
        self.costs
            .iter()
            .filter(|(key, _)| key.texture_kind() == kind)
            .map(|(_, bytes)| *bytes)
            .fold(0_u64, u64::saturating_add)
    }

    fn entries(&self, kind: AtlasTextureKind) -> u64 {
        self.costs
            .keys()
            .filter(|key| key.texture_kind() == kind)
            .count() as u64
    }

    fn remove(&mut self, key: &AtlasKey) -> Option<u64> {
        self.last_used.remove(key);
        self.costs.remove(key)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{CompressedRasterKind, ImageId, RenderImageParams};
    use std::borrow::Cow;
    use windows::Win32::{
        Foundation::HMODULE,
        Graphics::{
            Direct3D::D3D_DRIVER_TYPE_WARP,
            Direct3D11::{D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_SDK_VERSION, D3D11CreateDevice},
        },
    };

    fn create_atlas() -> Option<DirectXAtlas> {
        let mut device: Option<ID3D11Device> = None;
        let mut device_context: Option<ID3D11DeviceContext> = None;
        unsafe {
            D3D11CreateDevice(
                None,
                D3D_DRIVER_TYPE_WARP,
                HMODULE::default(),
                D3D11_CREATE_DEVICE_BGRA_SUPPORT,
                None,
                D3D11_SDK_VERSION,
                Some(&mut device),
                None,
                Some(&mut device_context),
            )
        }
        .ok()?;
        Some(DirectXAtlas::new(&device?, &device_context?))
    }

    fn make_image_key(image_id: usize) -> AtlasKey {
        AtlasKey::Image(RenderImageParams {
            image_id: ImageId(image_id),
            frame_index: 0,
            compressed_bc7_srgb: None,
        })
    }

    fn insert_tile(atlas: &DirectXAtlas, key: AtlasKey, size: Size<DevicePixels>) -> AtlasTile {
        atlas
            .get_or_insert_with(key, &mut || {
                let byte_count = (size.width.0 as usize) * (size.height.0 as usize) * 4;
                Ok(Some((size, Cow::Owned(vec![0u8; byte_count]))))
            })
            .expect("allocation should succeed")
            .expect("callback returns Some")
    }

    fn bc7_key(id: usize, kind: CompressedRasterKind) -> AtlasKey {
        AtlasKey::Image(RenderImageParams {
            image_id: ImageId(id),
            frame_index: 0,
            compressed_bc7_srgb: Some(kind),
        })
    }

    #[test]
    fn lru_selection_promotes_hits_and_never_crosses_ownership_kinds() {
        let icon_old = bc7_key(1, CompressedRasterKind::Icon);
        let icon_new = bc7_key(2, CompressedRasterKind::Icon);
        let thumbnail = bc7_key(3, CompressedRasterKind::Thumbnail);
        let mut residency = Bc7Residency::default();
        residency.insert(icon_old.clone(), 16);
        residency.insert(thumbnail.clone(), 64);
        residency.insert(icon_new.clone(), 32);
        assert!(
            residency
                .least_recent(AtlasTextureKind::Bc7Icon)
                .is_some_and(|key| key == icon_old)
        );
        residency.promote(&icon_old);
        assert!(
            residency
                .least_recent(AtlasTextureKind::Bc7Icon)
                .is_some_and(|key| key == icon_new)
        );
        assert!(
            residency
                .least_recent(AtlasTextureKind::Bc7Thumbnail)
                .is_some_and(|key| key == thumbnail)
        );
        assert_eq!(residency.bytes(AtlasTextureKind::Bc7Icon), 48);
        assert_eq!(residency.bytes(AtlasTextureKind::Bc7Thumbnail), 64);
        assert_eq!(residency.remove(&icon_new), Some(32));
        assert_eq!(residency.bytes(AtlasTextureKind::Bc7Icon), 16);
        assert_eq!(residency.entries(AtlasTextureKind::Bc7Icon), 1);
    }

    #[test]
    fn test_remove_deallocates_tile_space_for_reuse() {
        let Some(atlas) = create_atlas() else {
            return;
        };

        let small = Size {
            width: DevicePixels(64),
            height: DevicePixels(64),
        };
        let big = Size {
            width: DevicePixels(700),
            height: DevicePixels(700),
        };

        let keeper_key = make_image_key(1);
        let big_key_a = make_image_key(2);
        let big_key_b = make_image_key(3);

        let keeper_tile = insert_tile(&atlas, keeper_key, small);
        let tile_a = insert_tile(&atlas, big_key_a.clone(), big);
        assert_eq!(keeper_tile.texture_id, tile_a.texture_id);

        atlas.remove(&big_key_a);

        let tile_b = insert_tile(&atlas, big_key_b, big);
        assert_eq!(tile_b.texture_id, keeper_tile.texture_id);
    }
}
