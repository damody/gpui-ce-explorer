# Scroll Layers Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Scroll-only frames composite a scroll container's content from cached GPU tiles instead of rebuilding it, pixel-identically, on Linux/wgpu.

**Architecture:** The core (`crates/gpui/src/fast/layers/`) paints a scroll container's children into a layer `Scene` in content space, diffs it into 512 px tiles, and on scroll-only frames carries the content's non-scene records (hitboxes translated) and inserts tile quads (polychrome sprites with reserved texture ids) into the main scene. The wgpu renderer (`crates/gpui_wgpu/src/fast/layers/`) rasterizes dirty or missing tiles into textures cleared with a baked background, and binds them when it meets a tile quad.

**Tech Stack:** Rust 2024, GPUI (gpui-fast fork), wgpu, taffy. Tests: `cargo test -p gpui --features test-support`, `cargo test -p gpui_wgpu`, `gpui_perf --headless --verify`.

**Spec:** `docs/superpowers/specs/2026-09-30-scroll-layers-design.md` (read it before any task; section numbers below refer to it).

## Global Constraints

- gpui-fast's code lives in `crates/<crate>/src/fast/`; upstream files get only one-line hooks that name `crate::fast::layers::…` by full path; no `use crate::fast::…` outside `fast/`; no glob imports anywhere in `fast/`. Run `script/check-upstream` before every commit; it must report 0 violations.
- New files only under `crates/gpui/src/fast/layers/`, `crates/gpui/src/fast/tests/layers*.rs`, `crates/gpui_wgpu/src/fast/layers/`, `crates/gpui_perf/`, `docs/`.
- The only new public API is `Scene.layers` and the types/functions in `fast/layers/scene.rs` exported from `gpui.rs` (approved exception, spec §11). Test-only API is `#[cfg(any(test, feature = "test-support"))]`.
- Layers are compiled in only under `cfg(target_os = "linux")`; elsewhere every layer entry point is a no-op and today's path runs.
- Tile size 512 device px. Overscan one viewport extent per side along scrolled axes. Repaint margin: a quarter of the overscan. Tile budget 64 MB per window. Promotion after 2 consecutive scrolled frames. Demotion when content changed on > 8 of the last 16 frames or over budget; re-promotion after 60 stable frames; drop after 120 frames not composited.
- Output with layers must be byte-identical to output without layers on wgpu; anything that cannot guarantee that falls back to today's path.
- Positions observed by application code are always window coordinates and current when observed (spec §7).
- Never commit to `main`. Each stream works on its own branch off `scroll-layers` and merges back through a PR into `scroll-layers`; `scroll-layers` merges into `main` through one PR after M8.

## Review Focus

1. **Fractional scroll deltas from trackpads** (e.g. 0.37 px per event at scale 1.25): content must land on the same pixels with and without layers — pinned by the snapping test in Task 3.3 and the fractional-wheel oracle history in Task 7.2.
2. **Click right after a scroll without moving the mouse**: the click must hit the element now under the pointer and listeners must see current positions — pinned by `click_after_scroll_hits_current_element` in Task 5.2.
3. **Window resize or scale-factor change while a layer is live**: layer dropped, tiles released, next frame correct — pinned by `resize_drops_layers` in Task 4.3 and the renderer test in Task 2.4.
4. **Theme switch (background colour change) while scrolled**: tiles repainted with the new colour — pinned by `background_change_repaints_layer` in Task 3.4.
5. **Content shorter than the viewport, or scrolled to either end**: overscan clamped to content, no tiles outside content — pinned by `overscan_is_clamped_to_content` in Task 3.2.

---

## File Structure

**Core, `crates/gpui/src/fast/layers/`** (new):

| File | Owner stream | Responsibility |
|---|---|---|
| `mod.rs` | M1 | `WindowLayers` per-window state, `Layer`, entry points called from hooks, Linux gate |
| `scene.rs` | M1 | Public contract: `LayerKey`, `TileCoord`, `SceneLayers`, `LayerFrame`, tile texture ids, `tile_scene`, primitive translation |
| `record.rs` | M3 | `LayerRecord`: content scene, painted region, translation at paint, prepaint/paint ranges, recorded dependencies |
| `paint.rs` | M3 | Painting a container's children into the layer (scene swap), cull/clip split, composite insertion |
| `background.rs` | M3 | Background baking query over the main scene |
| `tiles.rs` | M3 | Tile hashing and diffing |
| `invalidate.rs` | M4 | `note_scrolled`, offset-read log, scroll-only decision |
| `policy.rs` | M4 | Eligibility, promotion, demotion, drop |
| `reuse.rs` | M5 | Carrying non-scene records on scroll-only frames |
| `input.rs` | M5 | Hitbox translation/clipping, rebuild-before-input, tooltips, getter translation |
| `lists.rs` | M6 | `uniform_list` / `list` partial row rendering |

**Core tests, `crates/gpui/src/fast/tests/`**: `layers.rs` (M3/M4/M5 unit + integration, one `mod` per stream inside), `layers_oracle.rs` (M7), `layers_lists.rs` (M6).

**Renderer, `crates/gpui_wgpu/src/fast/layers/`** (new, M2): `mod.rs`, `tile_cache.rs`, `raster.rs`, `composite.rs`, `tests.rs`. Existing gpui-fast files M2 edits: `crates/gpui_wgpu/src/fast/frame.rs`, `crates/gpui_wgpu/src/fast/mod.rs`.

**Upstream hooks** (one line each, owner in brackets): `scene.rs` field + `Default` [M1]; `gpui.rs` exports [M1]; `window.rs` `paint_glyph`/`paint_emoji` quantization [M1], `insert_hitbox` mask [M5], `dispatch_event` [M5], `Frame::clear` [M1]; `elements/div.rs` overflow-mask sites and children prepaint/paint [M3], scroll-offset application [M3], `paint_scroll_listener` and `ScrollHandle` getters/setters [M4]; `elements/list.rs`, `elements/uniform_list.rs` scroll listener/getters [M4], row rendering and item origins [M6].

---

## M1 — Contract (one agent, sequential; everything else waits for it)

### Task 1.1: Layer contract types and tile texture ids

**Files:**
- Create: `crates/gpui/src/fast/layers/mod.rs`, `crates/gpui/src/fast/layers/scene.rs`
- Modify: `crates/gpui/src/fast/mod.rs` (add `pub(crate) mod layers;`), `crates/gpui/src/gpui.rs` (exports)
- Test: `crates/gpui/src/fast/tests/layers.rs` (create, add `mod layers;` in `fast/tests/mod.rs`)

**Interfaces:**
- Produces (all `pub`, exported from `gpui.rs` as `pub use fast::layers::scene::{LayerKey, TileCoord, SceneLayers, LayerFrame, LAYER_TILE_TEXTURE_BASE, layer_tile_texture_id, decode_layer_tile};`):
  - `#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)] pub struct LayerKey(pub u32);` — `u32` in `0..0x0100_0000`.
  - `#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)] pub struct TileCoord { pub x: i32, pub y: i32 }` — each in `-2048..2048`.
  - `pub const LAYER_TILE_TEXTURE_BASE: u32 = 0xF000_0000;`
  - `pub fn layer_tile_texture_id(layer: LayerKey) -> AtlasTextureId` and the tile packed into `TileId` via `pub fn layer_tile_id(tile: TileCoord) -> TileId`; `pub fn decode_layer_tile(texture: AtlasTextureId, tile: TileId) -> Option<(LayerKey, TileCoord)>`.

  (One texture id per layer, the tile in `TileId`, so the batch iterator — which groups polychrome sprites by `(order, texture_id, tile_id)` — keeps each tile its own sprite and a layer's tiles adjacent.)

- [ ] **Step 1: Write the failing test** in `crates/gpui/src/fast/tests/layers.rs`:

```rust
//! Tests of scroll layers.

use crate::{
    AtlasTextureId, AtlasTextureKind, LayerKey, TileCoord, decode_layer_tile, layer_tile_id,
    layer_tile_texture_id,
};

#[test]
fn layer_tile_ids_round_trip_and_never_collide_with_the_atlas() {
    for layer in [0, 1, 77, 0x00FF_FFFF] {
        for tile in [(0, 0), (-1, 3), (2047, -2048), (-2048, 2047)] {
            let key = LayerKey(layer);
            let coord = TileCoord { x: tile.0, y: tile.1 };
            let texture = layer_tile_texture_id(key);
            assert_eq!(texture.kind, AtlasTextureKind::Polychrome);
            assert!(texture.index >= crate::LAYER_TILE_TEXTURE_BASE);
            assert_eq!(decode_layer_tile(texture, layer_tile_id(coord)), Some((key, coord)));
        }
    }
    let atlas = AtlasTextureId { index: 3, kind: AtlasTextureKind::Polychrome };
    assert_eq!(decode_layer_tile(atlas, layer_tile_id(TileCoord { x: 0, y: 0 })), None);
    let mono = AtlasTextureId { index: crate::LAYER_TILE_TEXTURE_BASE, kind: AtlasTextureKind::Monochrome };
    assert_eq!(decode_layer_tile(mono, layer_tile_id(TileCoord { x: 0, y: 0 })), None);
}
```

- [ ] **Step 2: Run it to verify it fails**

Run: `cargo test -p gpui --features test-support --lib fast::tests::layers`
Expected: compile error, `LayerKey` not found.

- [ ] **Step 3: Implement** `crates/gpui/src/fast/layers/scene.rs`:

```rust
//! What the core hands a renderer about scroll layers: the tiles it
//! composites, as polychrome sprites whose texture id lies in a range no
//! atlas allocates, and the content those tiles are rasterized from.

use crate::{AtlasTextureId, AtlasTextureKind, TileId};

/// A live scroll layer, stable while it lives.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct LayerKey(pub u32);

/// A tile of a layer's content space, in units of the tile size.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct TileCoord {
    pub x: i32,
    pub y: i32,
}

/// The first texture index of layer tiles; atlases allocate indices from 0.
pub const LAYER_TILE_TEXTURE_BASE: u32 = 0xF000_0000;
const LAYER_KEY_LIMIT: u32 = 0x0100_0000;
const TILE_COORD_BIAS: i32 = 2048;

/// The texture id the tiles of `layer` are composited with.
pub fn layer_tile_texture_id(layer: LayerKey) -> AtlasTextureId {
    debug_assert!(layer.0 < LAYER_KEY_LIMIT);
    AtlasTextureId {
        index: LAYER_TILE_TEXTURE_BASE + layer.0,
        kind: AtlasTextureKind::Polychrome,
    }
}

/// The tile id a composited `tile` carries.
pub fn layer_tile_id(tile: TileCoord) -> TileId {
    debug_assert!((-TILE_COORD_BIAS..TILE_COORD_BIAS).contains(&tile.x));
    debug_assert!((-TILE_COORD_BIAS..TILE_COORD_BIAS).contains(&tile.y));
    let x = (tile.x + TILE_COORD_BIAS) as u32;
    let y = (tile.y + TILE_COORD_BIAS) as u32;
    TileId((x << 16) | y)
}

/// The layer and tile a composited sprite stands for, if it is one.
pub fn decode_layer_tile(texture: AtlasTextureId, tile: TileId) -> Option<(LayerKey, TileCoord)> {
    if texture.kind != AtlasTextureKind::Polychrome
        || texture.index < LAYER_TILE_TEXTURE_BASE
        || texture.index - LAYER_TILE_TEXTURE_BASE >= LAYER_KEY_LIMIT
    {
        return None;
    }
    let layer = LayerKey(texture.index - LAYER_TILE_TEXTURE_BASE);
    let x = (tile.0 >> 16) as i32 - TILE_COORD_BIAS;
    let y = (tile.0 & 0xFFFF) as i32 - TILE_COORD_BIAS;
    Some((layer, TileCoord { x, y }))
}
```

`crates/gpui/src/fast/layers/mod.rs`:

```rust
//! Scroll layers: a scroll container's content painted once into cached
//! tiles and composited at the scroll offset on frames where only the
//! offset changed. See docs/superpowers/specs/2026-09-30-scroll-layers-design.md.

pub mod scene;
```

In `crates/gpui/src/fast/mod.rs` add `pub(crate) mod layers;` next to the other modules. In `crates/gpui/src/gpui.rs`, next to the existing `pub use fast::stats::LayoutStats;` line, add (one line):

```rust
pub use fast::layers::scene::{LAYER_TILE_TEXTURE_BASE, LayerFrame, LayerKey, SceneLayers, TileCoord, decode_layer_tile, layer_tile_id, layer_tile_texture_id};
```

(`LayerFrame` and `SceneLayers` are added in Task 1.2; add them to the export then, or declare empty structs now and fill them in 1.2.) `mod layers` in `fast/mod.rs` must be `pub(crate)` and `scene` `pub`, so the re-export reaches them. Check `script/check-upstream` passes: the `gpui.rs` line is a `pub use fast::…` export, which the script allows.

- [ ] **Step 4: Run the test to verify it passes**

Run: `cargo test -p gpui --features test-support --lib fast::tests::layers`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
script/check-upstream
git add crates/gpui/src/fast/layers crates/gpui/src/fast/mod.rs crates/gpui/src/gpui.rs crates/gpui/src/fast/tests
git commit -m "gpui: give scroll layer tiles texture ids no atlas allocates"
```

### Task 1.2: `Scene.layers`, `LayerFrame` and `tile_scene`

**Files:**
- Modify: `crates/gpui/src/fast/layers/scene.rs`, `crates/gpui/src/scene.rs` (one field; `Scene` derives `Default`, so the field only needs `Default`)
- Test: `crates/gpui/src/fast/tests/layers.rs`

**Interfaces:**
- Produces:
  - `#[derive(Default)] pub struct SceneLayers { pub frames: Vec<LayerFrame> }` with `pub fn clear(&mut self)`.
  - `pub struct LayerFrame { pub key: LayerKey, pub generation: u64, pub background: Rgba, pub tile_size: u32, pub content: Rc<Scene>, pub dirty_tiles: Vec<TileCoord> }`.
  - `impl LayerFrame { pub fn tile_bounds(&self, tile: TileCoord) -> Bounds<ScaledPixels>; pub fn tile_scene(&self, tile: TileCoord) -> Scene }` — `tile_scene` returns a finished (sorted) scene of the content primitives that intersect the tile, translated by `-tile_bounds.origin`.
  - `pub(crate) fn translate_primitive(primitive: &Primitive, delta: Point<ScaledPixels>) -> Primitive` (used by M3 to store content in content space).
  - Hook in `scene.rs`: `pub layers: crate::fast::layers::scene::SceneLayers,` in `Scene`, and `Scene::clear` calls `crate::fast::layers::scene::SceneLayers::clear(&mut self.layers);` (one line).

- [ ] **Step 1: Write the failing tests:**

```rust
use crate::{
    Bounds, ContentMask, Hsla, LayerFrame, Point, Quad, Scene, ScaledPixels, Shadow, Size,
    point, px, size,
};
use std::rc::Rc;

fn sp(x: f32, y: f32, w: f32, h: f32) -> Bounds<ScaledPixels> {
    Bounds { origin: point(ScaledPixels(x), ScaledPixels(y)), size: size(ScaledPixels(w), ScaledPixels(h)) }
}

fn quad(bounds: Bounds<ScaledPixels>) -> Quad {
    Quad {
        bounds,
        content_mask: ContentMask { bounds: sp(-10_000., -10_000., 20_000., 20_000.) },
        background: Hsla::red().into(),
        ..Default::default()
    }
}

fn layer(content: Scene) -> LayerFrame {
    LayerFrame {
        key: crate::LayerKey(1),
        generation: 1,
        background: crate::rgba(0xffffffff),
        tile_size: 512,
        content: Rc::new(content),
        dirty_tiles: Vec::new(),
    }
}

#[test]
fn a_tile_scene_holds_the_primitives_over_the_tile_in_its_own_space() {
    let mut content = Scene::default();
    content.insert_primitive(quad(sp(10., 10., 20., 20.)));      // tile (0,0) only
    content.insert_primitive(quad(sp(500., 100., 40., 10.)));    // tiles (0,0) and (1,0)
    content.insert_primitive(quad(sp(600., 700., 10., 10.)));    // tile (1,1) only
    content.insert_primitive(quad(sp(-30., -30., 10., 10.)));    // tile (-1,-1) only
    content.finish();
    let layer = layer(content);

    let t10 = layer.tile_scene(crate::TileCoord { x: 1, y: 0 });
    assert_eq!(t10.quads.len(), 1);
    assert_eq!(t10.quads[0].bounds, sp(500. - 512., 100., 40., 10.));
    assert_eq!(t10.quads[0].content_mask.bounds.origin, point(ScaledPixels(-10_000. - 512.), ScaledPixels(-10_000.)));

    let t00 = layer.tile_scene(crate::TileCoord { x: 0, y: 0 });
    assert_eq!(t00.quads.len(), 2);
    assert!(t00.quads[0].order < t00.quads[1].order, "drawing order kept");

    let tneg = layer.tile_scene(crate::TileCoord { x: -1, y: -1 });
    assert_eq!(tneg.quads.len(), 1);
    assert_eq!(tneg.quads[0].bounds, sp(-30. + 512., -30. + 512., 10., 10.));
}

#[test]
fn translation_moves_every_position_a_primitive_carries() {
    let delta = point(ScaledPixels(7.), ScaledPixels(-3.));
    let shadow = Shadow {
        bounds: sp(1., 2., 3., 4.),
        element_bounds: sp(5., 6., 7., 8.),
        content_mask: ContentMask { bounds: sp(0., 0., 100., 100.) },
        ..Default::default()
    };
    let crate::scene::Primitive::Shadow(moved) =
        crate::fast::layers::scene::translate_primitive(&shadow.into(), delta)
    else { unreachable!() };
    assert_eq!(moved.bounds, sp(8., -1., 3., 4.));
    assert_eq!(moved.element_bounds, sp(12., 3., 7., 8.));
    assert_eq!(moved.content_mask.bounds, sp(7., -3., 100., 100.));
    // Paths move every vertex and its mask; sprites adjust a rotation's translation.
    // (Add one assertion block per primitive kind: Quad, Path, Underline,
    // MonochromeSprite with a non-identity rotation, SubpixelSprite, PolychromeSprite.)
}
```

Complete the second test with one assertion block per remaining kind; for a sprite with `rotation_scale = [[0., -1.], [1., 0.]]` and `translation = [t0, t1]`, the expected translation is `t + (I − R)·delta`.

- [ ] **Step 2: Run to verify they fail** (`LayerFrame` missing).

- [ ] **Step 3: Implement** in `fast/layers/scene.rs`:

```rust
use crate::{
    Bounds, Point, Rgba, ScaledPixels, Scene, point, size,
    scene::{PaintOperation, Primitive, TransformationMatrix},
};
use std::rc::Rc;

#[derive(Default)]
pub struct SceneLayers {
    pub frames: Vec<LayerFrame>,
}

impl SceneLayers {
    pub fn clear(&mut self) {
        self.frames.clear();
    }
}

pub struct LayerFrame {
    pub key: LayerKey,
    pub generation: u64,
    pub background: Rgba,
    pub tile_size: u32,
    pub content: Rc<Scene>,
    pub dirty_tiles: Vec<TileCoord>,
}

impl LayerFrame {
    pub fn tile_bounds(&self, tile: TileCoord) -> Bounds<ScaledPixels> {
        let s = self.tile_size as f32;
        Bounds {
            origin: point(ScaledPixels(tile.x as f32 * s), ScaledPixels(tile.y as f32 * s)),
            size: size(ScaledPixels(s), ScaledPixels(s)),
        }
    }

    pub fn tile_scene(&self, tile: TileCoord) -> Scene {
        let bounds = self.tile_bounds(tile);
        let delta = point(-bounds.origin.x, -bounds.origin.y);
        let mut scene = Scene::default();
        for operation in &self.content.paint_operations {
            match operation {
                PaintOperation::Primitive(primitive) => {
                    let clipped = primitive.bounds().intersect(&primitive.content_mask().bounds);
                    if clipped.intersects(&bounds) {
                        scene.insert_primitive(translate_primitive(primitive, delta));
                    }
                }
                PaintOperation::StartLayer(layer_bounds) => {
                    scene.push_layer(layer_bounds.translate(delta))
                }
                PaintOperation::EndLayer => scene.pop_layer(),
            }
        }
        scene.finish();
        scene
    }
}

pub(crate) fn translate_primitive(primitive: &Primitive, delta: Point<ScaledPixels>) -> Primitive {
    let mut primitive = primitive.clone();
    let mv = |b: &mut Bounds<ScaledPixels>| b.origin = b.origin + delta;
    match &mut primitive {
        Primitive::Shadow(p) => { mv(&mut p.bounds); mv(&mut p.element_bounds); mv(&mut p.content_mask.bounds); }
        Primitive::Quad(p) => { mv(&mut p.bounds); mv(&mut p.content_mask.bounds); }
        Primitive::Path(p) => {
            mv(&mut p.bounds);
            mv(&mut p.content_mask.bounds);
            for v in &mut p.vertices {
                v.xy_position = v.xy_position + delta;
                mv(&mut v.content_mask.bounds);
            }
        }
        Primitive::Underline(p) => { mv(&mut p.bounds); mv(&mut p.content_mask.bounds); }
        Primitive::MonochromeSprite(p) => {
            mv(&mut p.bounds); mv(&mut p.content_mask.bounds);
            translate_transformation(&mut p.transformation, delta);
        }
        Primitive::SubpixelSprite(p) => {
            mv(&mut p.bounds); mv(&mut p.content_mask.bounds);
            translate_transformation(&mut p.transformation, delta);
        }
        Primitive::PolychromeSprite(p) => { mv(&mut p.bounds); mv(&mut p.content_mask.bounds); }
        Primitive::Surface(p) => { mv(&mut p.bounds); mv(&mut p.content_mask.bounds); }
    }
    primitive
}

/// A transformation about a point moves with it: `t' = t + (I − R)·delta`.
fn translate_transformation(m: &mut TransformationMatrix, delta: Point<ScaledPixels>) {
    let r = m.rotation_scale;
    let (dx, dy) = (delta.x.0, delta.y.0);
    m.translation[0] += dx - (r[0][0] * dx + r[0][1] * dy);
    m.translation[1] += dy - (r[1][0] * dx + r[1][1] * dy);
}
```

Required visibility bumps (hooks): `PaintOperation`, `Primitive::bounds`, `Primitive::content_mask`, `Scene::push_layer`/`pop_layer`/`insert_primitive`/`paint_operations` must be reachable from `fast` (`pub(crate)` where they are private). If `Bounds::translate` does not exist, add a free helper in `scene.rs` of `fast/layers` instead of touching upstream geometry. Verify the sprite transformation is applied to window-space positions in `shaders.wgsl` (`to_device_position_transformed`); if it is applied about the sprite origin instead, leave `transformation` untouched and change the test accordingly.

Hook in `crates/gpui/src/scene.rs`: add the field `pub layers: crate::fast::layers::scene::SceneLayers,` at the end of `Scene`, and in `Scene::clear` one line `crate::fast::layers::scene::SceneLayers::clear(&mut self.layers);`. Update the gpui.rs export to include `LayerFrame, SceneLayers`.

- [ ] **Step 4: Run tests, verify pass.** Also `cargo build -p gpui_wgpu -p gpui_perf` (the new public field must not break struct literals elsewhere; `Scene` is only built with `Default`).

- [ ] **Step 5: Commit** — `gpui: carry scroll layer content in the scene`.

### Task 1.3: Translation-invariant glyph quantization

**Files:**
- Modify: `crates/gpui/src/fast/glyphs.rs` (add `quantize_origin`, use it in `paint_glyph_in_run`), `crates/gpui/src/window.rs` (`paint_glyph` ~4598-4608 and `paint_emoji` ~4700: replace the quantization expressions with one call each)
- Test: `crates/gpui/src/fast/tests/layers.rs`

**Interfaces:**
- Produces: `pub(crate) fn quantize_origin(origin: Point<ScaledPixels>) -> (Point<ScaledPixels>, Point<u8>)` returning `(integer_origin, subpixel_variant)`; `pub(crate) fn quantize_emoji_origin(origin: Point<ScaledPixels>) -> Point<ScaledPixels>`.

- [ ] **Step 1: Failing tests:**

```rust
fn old_quantize(x: f32, y: f32) -> (f32, f32, u8) {
    use crate::text_system::{SUBPIXEL_VARIANTS_X as VX, SUBPIXEL_VARIANTS_Y as VY};
    let qx = crate::util::round_half_toward_zero(x * VX as f32) / VX as f32;
    let qy = crate::util::round_half_toward_zero(y * VY as f32) / VY as f32;
    (qx.trunc(), qy.trunc(), (qx.fract() * VX as f32) as u8)
}

#[test]
fn glyph_quantization_is_unchanged_on_screen() {
    for i in 0..20_000 {
        let x = i as f32 * 0.0137;
        let y = i as f32 * 0.0291;
        let (origin, variant) = crate::fast::glyphs::quantize_origin(point(ScaledPixels(x), ScaledPixels(y)));
        let (ox, oy, v) = old_quantize(x, y);
        assert_eq!((origin.x.0, origin.y.0, variant.x), (ox, oy, v), "at ({x}, {y})");
    }
}

#[test]
fn glyph_quantization_moves_with_whole_pixel_shifts() {
    for i in 0..5_000 {
        let x = -300. + i as f32 * 0.0731;
        let y = -300. + i as f32 * 0.0519;
        let (a, va) = crate::fast::glyphs::quantize_origin(point(ScaledPixels(x), ScaledPixels(y)));
        let (b, vb) = crate::fast::glyphs::quantize_origin(point(ScaledPixels(x + 1024.), ScaledPixels(y + 1024.)));
        assert_eq!(va, vb, "variant at ({x}, {y})");
        assert_eq!((b.x.0 - a.x.0, b.y.0 - a.y.0), (1024., 1024.), "origin at ({x}, {y})");
    }
}
```

- [ ] **Step 2: Run, verify fail.**

- [ ] **Step 3: Implement** in `fast/glyphs.rs`:

```rust
/// A glyph's whole-pixel origin and subpixel variant: upstream's rounding
/// for non-negative coordinates, extended so that a whole-pixel shift moves
/// the result by exactly that shift everywhere, negative coordinates included.
pub(crate) fn quantize_origin(origin: Point<ScaledPixels>) -> (Point<ScaledPixels>, Point<u8>) {
    let (x, vx) = quantize_axis(origin.x.0, SUBPIXEL_VARIANTS_X);
    let (y, vy) = quantize_axis(origin.y.0, SUBPIXEL_VARIANTS_Y);
    (point(ScaledPixels(x), ScaledPixels(y)), point(vx, vy))
}

fn quantize_axis(value: f32, variants: u8) -> (f32, u8) {
    let whole = value.floor();
    let steps = round_half_toward_zero((value - whole) * variants as f32) as i32; // 0..=variants
    let carry = steps / variants as i32;
    (whole + carry as f32, (steps % variants as i32) as u8)
}

pub(crate) fn quantize_emoji_origin(origin: Point<ScaledPixels>) -> Point<ScaledPixels> {
    origin.map(|c| {
        let whole = c.0.floor();
        ScaledPixels(whole + round_half_toward_zero(c.0 - whole))
    })
}
```

Note: `round_half_toward_zero` of a value in `[0, variants)` equals upstream's rounding on the positive side, and `floor` equals `trunc` there, so on-screen results are unchanged (the first test proves it). In `window.rs`, replace the `quantized_origin` / `subpixel_variant` / `integer_origin` computation in `paint_glyph` with `let (integer_origin, subpixel_variant) = crate::fast::glyphs::quantize_origin(glyph_origin.scale(scale_factor));` (keep variable names the rest of the function uses) and the emoji rounding in `paint_emoji` with `crate::fast::glyphs::quantize_emoji_origin(...)`. Use `quantize_origin` in `LineGlyphPainter::paint_glyph_in_run` too.

- [ ] **Step 4: Run the two tests and the whole suite** (`cargo test -p gpui --features test-support`), verify pass.

- [ ] **Step 5: Commit** — `gpui: quantize glyph origins the same way at every whole-pixel offset`.

### Task 1.4: `WindowLayers` skeleton, Linux gate, stats, stream modules

**Files:**
- Modify: `crates/gpui/src/fast/layers/mod.rs`; create empty stream modules `record.rs`, `paint.rs`, `background.rs`, `tiles.rs`, `invalidate.rs`, `policy.rs`, `reuse.rs`, `input.rs`, `lists.rs` (each with a module doc comment and the struct below); modify `crates/gpui/src/window.rs` (one field `pub(crate) fast_layers: crate::fast::layers::WindowLayers,` and its `Default` init; `Frame::clear` needs nothing), `crates/gpui/src/fast/stats.rs` (fields)
- Test: `crates/gpui/src/fast/tests/layers.rs`

**Interfaces (the skeleton every stream fills in; names are final):**

```rust
// fast/layers/mod.rs
pub mod scene;
pub(crate) mod background;   // M3
pub(crate) mod input;        // M5
pub(crate) mod invalidate;   // M4
pub(crate) mod lists;        // M6
pub(crate) mod paint;        // M3
pub(crate) mod policy;       // M4
pub(crate) mod record;       // M3
pub(crate) mod reuse;        // M5
pub(crate) mod tiles;        // M3

use crate::{GlobalElementId, Window};
use collections::FxHashMap;

/// Whether scroll layers are compiled in (spec §5.1: Linux/wgpu only in v1).
pub(crate) const COMPILED: bool = cfg!(target_os = "linux");

/// A window's scroll layers, by the scroll container's global id.
#[derive(Default)]
pub(crate) struct WindowLayers {
    pub(crate) layers: FxHashMap<GlobalElementId, Layer>,
    pub(crate) next_key: u32,
    /// Off in tests that compare against today's path; on by default where compiled.
    pub(crate) enabled: bool,
    pub(crate) scrolls: invalidate::ScrollLog,      // M4
    pub(crate) painting: Option<paint::Painting>,   // M3: the layer being painted, if any
}

pub(crate) struct Layer {
    pub(crate) key: scene::LayerKey,
    pub(crate) record: Option<record::LayerRecord>, // M3
    pub(crate) policy: policy::LayerPolicy,          // M4
    pub(crate) input: input::LayerInput,             // M5
    pub(crate) rows: lists::LayerRows,               // M6
    pub(crate) last_composited_frame: u64,
}

/// Whether layers may be used in `window` this frame (spec §6.5, first bullet).
pub(crate) fn active(window: &Window, cx: &crate::App) -> bool {
    COMPILED
        && window.fast_layers.enabled
        && window.retained_state.view_retention
        && !window.refreshing
        && !cx.has_active_drag()
        && !window.a11y.is_active()
        && !window.is_inspector_picking(cx)
}
```

Each stream module starts as, e.g. `record.rs`: `//! A layer's recorded content (M3).` + `#[derive(Default)] pub(crate) struct LayerRecord {}`; `paint.rs`: `pub(crate) struct Painting {}`; `invalidate.rs`: `#[derive(Default)] pub(crate) struct ScrollLog {}`; `policy.rs`: `#[derive(Default)] pub(crate) struct LayerPolicy {}`; `input.rs`: `#[derive(Default)] pub(crate) struct LayerInput {}`; `lists.rs`: `#[derive(Default)] pub(crate) struct LayerRows {}`; `background.rs`, `tiles.rs`, `reuse.rs`: doc comment only. `enabled` defaults to `COMPILED` unless `GPUI_SCROLL_LAYERS=0` (read once in `WindowLayers::default`).

Test-only switch on `Window` (in `fast/layers/mod.rs`, `impl Window`): `#[cfg(any(test, feature = "test-support"))] pub fn set_scroll_layers(&mut self, enabled: bool)` (sets `enabled`, clears `layers`, calls `self.refresh()`).

Stats (`fast/stats.rs`, `LayoutStats`, with doc comments like their neighbours): `pub layer_frames_composited: u64, pub layer_frames_repainted: u64, pub tiles_dirtied: u64, pub layer_rebuilds_for_input: u64, pub layers_demoted: u64`. Counting sites are added by the streams that own them.

- [ ] **Step 1: Failing test:**

```rust
#[crate::test]
fn scroll_layers_are_on_where_compiled_and_can_be_turned_off(cx: &mut crate::TestAppContext) {
    let window = cx.add_window(|_, _| crate::Empty);
    cx.update_window(window.into(), |_, window, _| {
        assert_eq!(window.fast_layers.enabled, crate::fast::layers::COMPILED);
        window.set_scroll_layers(false);
        assert!(!window.fast_layers.enabled);
    }).unwrap();
}
```

(If `Empty` is not a `Render` view, use a trivial `struct V; impl Render for V`.)

- [ ] **Step 2–4:** run (fail), implement, run (pass), plus full `cargo test -p gpui --features test-support` and `script/check-upstream`.

- [ ] **Step 5: Commit** — `gpui: add the per-window scroll layer state every stream builds on`.

### Task 1.5: Document the API exception

**Files:** Modify `docs/upstream-sync.md` ("Where our API differs from upstream's"): add a bullet — `Scene` has a public `layers` field, and `gpui` exports `LayerKey`, `TileCoord`, `SceneLayers`, `LayerFrame`, `LAYER_TILE_TEXTURE_BASE`, `layer_tile_texture_id`, `layer_tile_id` and `decode_layer_tile`, because renderers in other crates read scroll layers from the scene; polychrome sprites with texture indices at or above `LAYER_TILE_TEXTURE_BASE` are layer tiles, not atlas textures.

- [ ] Commit — `docs: note the scroll layer API renderers read`. Open PR `scroll-layers-m1` → `scroll-layers`; after merge all streams branch from `scroll-layers`.

---

## M2 — Renderer (stream R; after M1)

Branch `scroll-layers-renderer`. Owns `crates/gpui_wgpu/src/fast/layers/*` and edits to `crates/gpui_wgpu/src/fast/frame.rs`, `fast/mod.rs`. Reads the explorer facts in spec §5.6.

### Task 2.1: Surfaceless render harness (test infrastructure)

**Files:** Create `crates/gpui_wgpu/src/fast/layers/tests.rs` (behind `#[cfg(test)]`), modify `crates/gpui_wgpu/src/fast/frame.rs` so the draw path can target any `TextureView` without a `wgpu::Surface`.

**Interfaces:**
- Produces (test-only): `struct Harness` with `fn new() -> Option<Harness>` (None when no adapter; tests then return early with a printed skip), `fn render(&mut self, scene: &Scene, size: Size<DevicePixels>, clear: Rgba) -> Vec<u8>` (RGBA8 readback) that runs **the same** pipelines, globals and `fast::frame` recording as the on-screen path.
- Refactor: extract from `fast::frame::record` the parts that need the renderer into a `FrameTarget<'a> { device, queue, pipelines, bind_group_layouts, globals, atlas, instance_data, ... }` borrowed view built by `WgpuRenderer` (on-screen) or by `Harness` (tests). Pipelines are built by the existing `create_pipelines`; if it takes `&self`, give it a free-function form in `fast/` that the renderer's method forwards to (hook).

- [ ] Step 1: Write `harness_renders_a_quad_at_the_right_pixels` — render one red quad at (4,4,8,8) over a white clear into 32×32; assert pixel (8,8) is red and (1,1) white.
- [ ] Step 2: Run `cargo test -p gpui_wgpu fast::layers::tests`, verify fail.
- [ ] Step 3: Implement the refactor and harness (device from `wgpu::Instance::request_adapter(compatible_surface: None)`, requesting `DUAL_SOURCE_BLENDING` if the adapter has it; target texture `Bgra8Unorm` (same selection as the surface path), `RENDER_ATTACHMENT | COPY_SRC`; `copy_texture_to_buffer` with 256-byte row alignment; `map_async` + `device.poll(Wait)`).
- [ ] Step 4: Run the test and the existing `gpui_wgpu` tests; run `gpui_perf --release` showcase briefly to confirm on-screen drawing is unchanged.
- [ ] Step 5: Commit — `gpui_wgpu: let the frame recorder draw into any texture, for pixel tests`.

### Task 2.2: Tile cache and raster passes

**Files:** Create `fast/layers/mod.rs`, `tile_cache.rs`, `raster.rs`; modify `fast/frame.rs` (call raster before `begin_main_pass`), `fast/mod.rs`.

**Interfaces:**
- Consumes: `gpui::{SceneLayers, LayerFrame, LayerKey, TileCoord, decode_layer_tile}`.
- Produces: `pub(crate) struct TileCache` (field `layers: TileCache` inside `crate::fast::frame::FrameState`) with `fn texture(&self, layer: LayerKey, tile: TileCoord) -> Option<&wgpu::TextureView>`, `fn begin_frame(&mut self, layers: &SceneLayers, composited: impl Iterator<Item=(LayerKey, TileCoord)>) -> Vec<(usize /*frame index*/, TileCoord)>` returning the tiles to rasterize (dirty ∪ composited-but-missing, invalidating tiles whose `generation` changed and are listed dirty), `fn evict_to_budget(&mut self, budget_bytes: u64)`, `fn clear(&mut self)`.
- `raster::rasterize(target: &mut FrameTarget, encoder, frame: &LayerFrame, tile: TileCoord, view: &wgpu::TextureView)`: clear to `frame.background` (convert `Rgba` to the surface's non-sRGB bytes exactly as the main pass writes colours), bind tile globals (`viewport_size = [tile, tile]`, same gamma and `premultiplied_alpha` as the frame; own uniform buffer and bind group 0, cached in `TileCache`), draw `frame.tile_scene(tile)` batches through the same batch loop the main pass uses, with a tile-sized path intermediate (+MSAA with the same sample count) owned by `TileCache`.

- [ ] Step 1: Tests in `tests.rs`:
  - `a_rasterized_tile_equals_the_same_content_drawn_directly` — build a content scene with a quad, a bordered rounded quad, a drop shadow, a gradient quad, an underline (wavy), a path, a monochrome glyph sprite and a subpixel glyph sprite (upload glyph tiles into the harness atlas, or skip the sprite cases when no text system is available in tests: then use a polychrome sprite from a small atlas image), spread over four tiles; render it directly with `Harness::render` at 1024×1024 over the background colour; rasterize the four tiles and assemble them; compare bytes exactly.
  - `tiles_of_negative_coordinates_rasterize_like_positive_ones` — same content shifted by (−512,−512), tiles (−1,−1)…(0,0).
  - `a_changed_generation_rerasterizes_only_dirty_tiles` (TileCache unit test with a counter).
- [ ] Step 2–4: fail, implement, pass.
- [ ] Step 5: Commit — `gpui_wgpu: rasterize scroll layer tiles into cached textures`.

### Task 2.3: Compositing tile quads

**Files:** Create `fast/layers/composite.rs`; modify `fast/frame.rs` where polychrome batches are drawn (~frame.rs:427: before `atlas.get_texture_info(texture_id)`, check `gpui::decode_layer_tile(texture_id, first_sprite.tile.tile_id)`).

**Interfaces:** `composite::texture_for_batch(cache: &TileCache, texture: AtlasTextureId, sprites: &[PolychromeSprite]) -> Option<&wgpu::TextureView>`; because tile id is per sprite, split a polychrome batch into runs of equal `tile_id` when the texture decodes to a layer (the batch iterator already breaks on `(texture_id, tile_id)`; assert that in debug).

- [ ] Step 1: Test `a_composited_layer_equals_direct_drawing` — main scene: background quad, a polychrome tile sprite per tile at translation (37, −91) clipped by a viewport mask (40, 30, 600, 400), then a quad painted after (the scrollbar stand-in); the layer's content scene as in 2.2; compare with a direct render of background + content translated and clipped + the later quad. Byte-exact.
- [ ] Step 2–4: fail, implement, pass. Missing tile at composite time (not in cache and not rasterized, which 2.2 prevents) must draw nothing rather than panic; debug-assert.
- [ ] Step 5: Commit — `gpui_wgpu: draw scroll layer tiles where the scene composites them`.

### Task 2.4: Budget, eviction, resize and device loss

**Files:** `tile_cache.rs`, `fast/frame.rs`, the renderer's resize/device-recovery paths (existing gpui-fast hook points only; add one-line hooks calling `crate::fast::layers::TileCache::clear` if none exists).

- [ ] Tests: `tiles_are_evicted_least_recently_composited_first_within_the_budget`; `resize_releases_every_tile` (unit test on `TileCache` driven through the same method the resize path calls).
- [ ] Implement 64 MB default budget (`tile_size² × 4` bytes per tile), LRU by last composited frame, never evicting a tile composited this frame; `clear()` on resize, scale change and device recovery.
- [ ] Commit — `gpui_wgpu: keep scroll layer tiles within a memory budget`.

---

## M3 — Paint (stream P; after M1)

Branch `scroll-layers-paint`. Owns `fast/layers/{record,paint,background,tiles}.rs`, the div overflow-mask and children prepaint/paint hooks, and scroll-offset snapping in div.

### Task 3.1: Tile hashing and diffing

**Files:** `fast/layers/tiles.rs`; tests in `fast/tests/layers.rs` (`mod paint`).

**Interfaces:** `pub(crate) fn tile_hashes(content: &Scene, tile_size: u32, region: Bounds<ScaledPixels>) -> FxHashMap<TileCoord, u64>` (hash of the translated primitives over each tile in drawing order, including tiles with no primitives, which hash to a fixed empty value); `pub(crate) fn dirty_tiles(old: &FxHashMap<TileCoord, u64>, new: &FxHashMap<TileCoord, u64>) -> Vec<TileCoord>` (sorted; tiles in `new` whose hash differs or that are absent from `old`).

- [ ] Tests: identical scenes → no dirty tiles; a colour change in one quad → exactly the tiles that quad covers; a primitive moved within one tile → that tile; an added primitive spanning two tiles → both.
- [ ] Implement with `std::hash::Hash` over each translated primitive's fields (`Quad`, `Shadow` etc. are `Copy`/`Clone`; hash their bytes via a small `hash_primitive(&Primitive, &mut FxHasher)` that feeds every field, f32s by `to_bits()`), tile keyed by `translate_primitive(p, −tile_origin)`.
- [ ] Commit — `gpui: find the scroll layer tiles a repaint changed`.

### Task 3.2: Painting a scroll container's children into a layer

**Files:** `fast/layers/record.rs`, `fast/layers/paint.rs`; hooks in `crates/gpui/src/elements/div.rs`: in `Div::prepaint` around the children loop inside `with_element_offset(scroll_offset, …)` (~2029) and in `Div::paint` children loop (~2083), and at the two `with_content_mask(style.overflow_mask(...))` sites (~2392, ~2555).

**Interfaces:**
- `record::LayerRecord { pub(crate) content: Rc<Scene>, pub(crate) generation: u64, pub(crate) painted_region: Bounds<Pixels>, pub(crate) viewport: Bounds<Pixels>, pub(crate) scroll_offset: Point<Pixels> /* snapped, at paint */, pub(crate) prepaint_range: Range<PrepaintStateIndex>, pub(crate) paint_range: Range<PaintIndex>, pub(crate) tile_hashes: FxHashMap<TileCoord, u64>, pub(crate) dirty_tiles: Vec<TileCoord>, pub(crate) background: Rgba, pub(crate) hovers: Rc<[(HitboxId, bool)]>, pub(crate) dependencies: RenderDependencies }`.
- `paint::Painting { pub(crate) id: GlobalElementId, pub(crate) viewport: Bounds<Pixels>, pub(crate) painted_region: Bounds<Pixels>, pub(crate) scene: Scene, pub(crate) scroll_offset: Point<Pixels> }`.
- Hook functions (called from div.rs, each one line):
  - `crate::fast::layers::paint::overflow_mask(window, id, mask: Option<ContentMask<Pixels>>, scroll_axes) -> Option<ContentMask<Pixels>>` — while the container at `id` is painting into a layer, returns the painted region instead of the viewport (culling in the larger region), else `mask` unchanged.
  - `crate::fast::layers::paint::prepaint_children(window, cx, id: Option<&GlobalElementId>, bounds, content_size, scroll_offset, f: impl FnOnce(&mut Window, &mut App)) ` — decides via `policy::decide` (M4; until M4 lands, a stub returning `Decision::Bypass` or, under a test-only override, `Decision::Repaint`) whether to run `f` normally (`Bypass`), to run it painting into a layer (`Repaint`), or to skip it (`Composite`, handled by M5's `reuse`).
  - `crate::fast::layers::paint::paint_children(window, cx, id, f)` — the paint-phase counterpart: for `Repaint`, swaps `window.next_frame.scene` with `Painting.scene`, runs `f`, swaps back, stores primitives translated by `−viewport_translation(scroll_offset)` into `record.content`, computes tile hashes over the painted region and dirty tiles, then inserts tile quads (Task 3.5); for `Composite`, inserts tile quads only; for `Bypass`, runs `f`.
- Painted region: viewport extended by one viewport extent on each scrolled axis, intersected with the content rect (`bounds.origin − scroll_offset`, `content_size`), in window space at the current (snapped) offset.
- While a layer paints, nested retained views record nothing and reuse nothing: add `pub(crate) fn inside_layer(window) -> bool` and make `fast::retained`'s `reusable_retained`, `begin_retained`, `splice_layout` return `None` / no-op when it is true (one-line checks inside `fast/`, no upstream edits).

- [ ] Tests (`mod paint`, using a test-only `Decision` override on `WindowLayers` so M3 does not wait for M4):
  - `a_repainted_layer_holds_the_content_in_content_space` — a view with `div().id("s").overflow_y_scroll().h(px(100.)).children((0..40).map(|i| div().h(px(20.)).bg(color(i))))`, forced `Repaint`; after draw, the main scene has no row quads, the layer record's content has the rows of the painted region (viewport 100 px + 100 px overscan below; offset 0 so nothing above) = 10 rows at their content positions.
  - `overscan_is_clamped_to_content` — content 60 px tall in a 100 px viewport: painted region = content rect, no tiles below it.
  - `hitboxes_in_overscan_do_not_hit` is M5's; do not test here.
- [ ] Implement. Keep `paint.rs` free of policy: it asks `policy::decide` and does what it says.
- [ ] Commit — `gpui: paint a scroll container's children into a layer`.

### Task 3.3: Snapping scroll offsets to device pixels

**Files:** `fast/layers/paint.rs` (`pub(crate) fn snap_scroll_offset(window, offset: Point<Pixels>) -> Point<Pixels>`), div.rs hook at `with_element_offset(scroll_offset, …)` (~2029): pass `crate::fast::layers::paint::snap_scroll_offset(window, scroll_offset)`.

- [ ] Test `scroll_offsets_land_on_device_pixels` — scale factor 1.25, set offset −10.37 px; after draw the first row's quad bounds origin y × 1.25 is a whole number; with `COMPILED == false` (simulate via a test hook) the offset is untouched.
- [ ] Implement: `if !COMPILED { return offset }`; round each axis `(offset × scale).round() / scale` (use `round_half_toward_zero` to match `pixel_snap`).
- [ ] Commit — `gpui: snap scroll offsets to device pixels where scroll layers are compiled`.

### Task 3.4: Background baking

**Files:** `fast/layers/background.rs`; called by `paint_children` before a `Repaint`.

**Interfaces:** `pub(crate) fn bake(scene: &Scene, viewport: Bounds<ScaledPixels>, window_opaque: bool) -> Option<Rgba>` — `Some(colour)` iff the topmost primitive (highest order) whose clipped bounds intersect `viewport`, among all kinds, is a `Quad` with a solid `Background` of alpha 1, no border crossing into the viewport, corner radii not reaching into the viewport, clipped bounds ⊇ viewport, and `window_opaque`.

- [ ] Tests: opaque panel quad under the viewport → `Some`; gradient → `None`; semi-transparent → `None`; a smaller quad on top partially inside → `None`; rounded corners inside the viewport → `None`; transparent window → `None`; `background_change_repaints_layer` (integration: switch the panel colour between frames with the layer composited → next frame is a `Repaint` with all tiles dirty and the new colour in the record).
- [ ] Commit — `gpui: bake the solid background under a scroll layer into its tiles`.

### Task 3.5: Inserting tile quads

**Files:** `fast/layers/paint.rs` (`fn insert_tile_quads(window, layer: &Layer)`); adds the frame's `LayerFrame` to `window.next_frame.scene.layers`.

- [ ] Test `tile_quads_cover_the_viewport_at_the_current_offset` — composite at offset −130 px: the main scene holds polychrome sprites for exactly the tiles intersecting the viewport, each with `decode_layer_tile` = (key, coord), bounds = `tile_bounds + T`, content mask = viewport (scaled, `cover_bounds`-snapped like other primitives), and `scene.layers.frames` holds one frame whose `content` is the record's `Rc`.
- [ ] Implement: `push_layer(viewport)` so all tiles share one draw order (matches spec §5.1), insert one `PolychromeSprite { opacity: 1., grayscale: false, corner_radii: zero, tile: AtlasTile { texture_id: layer_tile_texture_id(key), tile_id: layer_tile_id(coord), padding: 0, bounds: (0,0,tile,tile) } , .. }` per tile, `pop_layer`. Count `layer_frames_composited` / `layer_frames_repainted` / `tiles_dirtied` in `LayoutStats`.
- [ ] Commit — `gpui: composite scroll layer tiles into the frame`.

---

## M4 — Invalidation and policy (stream I; after M1)

Branch `scroll-layers-invalidation`. Owns `fast/layers/{invalidate,policy}.rs`, hooks in div.rs `paint_scroll_listener` and `ScrollHandle` methods, list.rs/uniform_list.rs scroll listeners and scroll getters/setters.

### Task 4.1: Recording scrolls and offset reads

**Files:** `invalidate.rs`; hooks: div.rs `paint_scroll_listener` (~3401: add `crate::fast::layers::invalidate::note_scrolled(window, &container_id);` next to the existing `invalidate_retained_subtrees` call — the container id is the div's `GlobalElementId`, captured at paint), `ScrollHandle::set_offset`, `scroll_to_item`, `scroll_to_top_of_item`, `scroll_to_bottom` (note by handle), getters `offset`, `max_offset`, `top_item`, `bottom_item`, `bounds_for_item`, `child_bounds` (note read); list.rs `StateInner::scroll` and `ListState::scroll_to*`, `logical_scroll_top`, `scroll_px_offset_for_scrollbar`; uniform_list.rs `UniformListScrollHandle::scroll_to_*` and getters.

**Interfaces:**
- `ScrollLog { scrolled: FxHashSet<GlobalElementId> /* this frame */, offset_reads: RefCell<Vec<ScrollSource>> /* open recording */ }`, `pub(crate) enum ScrollSource { Handle(usize /* Rc ptr of ScrollHandleState */), Container(GlobalElementId) }`.
- `pub(crate) fn note_scrolled(window: &mut Window, container: &GlobalElementId)`; `pub(crate) fn note_handle_scrolled(cx: &App, handle: &Rc<RefCell<ScrollHandleState>>)`; `pub(crate) fn note_offset_read(cx: &App, source: ScrollSource)` — the read is logged only while a render recording is open (reuse `cx.entities.is_recording()` as `note_state_read` does) and attributed to the view being rendered.
- `pub(crate) fn render_read_offset(record: &RenderDependencies, source: &ScrollSource) -> bool` — extend `RenderDependencies` in `fast/dependencies.rs` (gpui-fast file) with `offset_reads: Vec<ScrollSource>` filled when a recording closes.

- [ ] Tests: `a_wheel_scroll_is_noted_for_its_container`; `programmatic_scrolls_are_noted`; `a_render_that_reads_the_offset_depends_on_it` (view calls `handle.offset()` in render; its record lists the read) and `one_that_does_not_does_not`.
- [ ] Commit — `gpui: tell scroll offset changes and reads apart from other changes`.

### Task 4.2: Scroll-only decision

**Files:** `invalidate.rs` (`pub(crate) fn scroll_only(window, cx, id: &GlobalElementId, record: &LayerRecord) -> bool` implementing spec §6.4 exactly), `policy.rs` (`pub(crate) enum Decision { Bypass, Repaint, Composite }`, `pub(crate) fn decide(window, cx, id, bounds, content_size, scroll_offset) -> Decision`, replacing M3's stub).

- [ ] Tests (each draws twice and asserts the second frame's decision, exposed through a test-only `last_decision(id)`):
  - `a_wheel_scroll_composites` (pattern B page, wheel event, → `Composite` from the third scrolled frame on; `Repaint` on promotion).
  - `a_child_view_page_composites` (pattern A).
  - `a_notified_content_view_repaints` (child view notified → `Repaint`).
  - `an_owner_that_reads_the_offset_in_render_repaints`.
  - `a_hover_change_in_the_content_repaints`.
  - `a_style_change_of_the_scroll_div_repaints`.
  - `exposing_past_the_margin_repaints` (scroll beyond painted region − margin → `Repaint` with the region re-centred).
- [ ] Commit — `gpui: decide per scroll container whether a frame only scrolled`.

### Task 4.3: Eligibility, promotion, demotion, drop

**Files:** `policy.rs` (`LayerPolicy { scrolled_streak: u8, change_history: u16 /* bit per frame */, stable_since: u64, demoted_until: Option<u64> }`), `mod.rs` (drop pass at end of frame: `pub(crate) fn finish_frame(window)` called from `fast::retained::finish_retained_frame` — a gpui-fast function, no upstream edit).

- [ ] Tests: `a_container_is_promoted_after_two_scrolled_frames`; `deferred_draws_inside_make_it_ineligible` (an `anchored()` popover open inside content → `Bypass`); `a_focused_input_inside_makes_it_ineligible`; `churning_content_is_demoted_and_repromoted_after_60_stable_frames`; `a_layer_not_composited_for_120_frames_is_dropped`; `resize_drops_layers` (window resize or scale change clears `layers`).
- [ ] Commit — `gpui: promote, demote and drop scroll layers`.

---

## M5 — Input and record carry-over (stream N; after M1, merges after M3)

Branch `scroll-layers-input`. Owns `fast/layers/{reuse,input}.rs`, hooks in `window.rs` `insert_hitbox` and `dispatch_event`.

### Task 5.1: Carrying the content's records on composite frames

**Files:** `reuse.rs`: `pub(crate) fn carry(window: &mut Window, cx: &mut App, record: &mut LayerRecord, delta: Point<Pixels>)` — the composite-frame replacement for running the children: copies from `rendered_frame` the record's prepaint range (hitboxes translated by `delta` and intersected with the viewport, tooltips dropped, deferred draws none by eligibility, dispatch subtree via `dispatch_tree.reuse_subtree`, accessed element states, line layouts) and paint range (mouse listeners, cursor styles, input handlers (none by eligibility), tab stops, accessed element states, window-control hitboxes translated), **without** `scene.replay`; updates `record.prepaint_range`/`paint_range` to the new frame's indices.

Base it on `Window::reuse_prepaint` / `reuse_paint` (window.rs ~3734/3799): write the variant in `reuse.rs` using the `pub(crate)` fields those functions touch (bump visibility with hooks where needed), not by editing them.

- [ ] Tests: `composited_frames_keep_the_content_interactive` (after two composite frames, clicking a row's position dispatches its `on_click`; the rebuild-before-input of 5.2 happens first — assert the click lands, and with 5.2 not yet merged, assert the carried hitbox list has the row's hitbox at the translated bounds); `keyboard_focus_inside_survives_composite_frames` (focus handle inside content keeps focus and action dispatch works).
- [ ] Commit — `gpui: carry a scroll layer's hitboxes and listeners through composited frames`.

### Task 5.2: Rebuild before non-wheel input

**Files:** `input.rs`: `pub(crate) fn before_dispatch(window: &mut Window, cx: &mut App, event: &PlatformInput)` called first in `Window::dispatch_event` (one-line hook): for every live layer with `delta ≠ 0` whose viewport contains the event position (pointer events other than `ScrollWheel`) or which contains the focused element (key events), marks it for `Repaint` and synchronously draws the window (`window.draw(cx)` is how other forced frames happen; reuse the same mechanism `refresh` + draw uses in `fast::retained`, and count `layer_rebuilds_for_input`), then returns so dispatch proceeds against the rebuilt frame.

- [ ] Tests: `click_after_scroll_hits_current_element` (scroll 3 frames composited, then mouse down/up at a fixed point without moving: the handler of the row now under the point runs, and the `ClickEvent` position and the `Hitbox` bounds it sees equal those of a window with layers off after the same history); `drag_after_scroll_sees_current_bounds` (`DragMoveEvent.bounds` equal to layers-off); `wheel_events_do_not_rebuild` (count stays 0 over 10 wheel frames); `a_mouse_move_after_scroll_rebuilds_once`.
- [ ] Commit — `gpui: bring a scrolled layer's content up to date before it sees input`.

### Task 5.3: Hitbox masks, tooltips, getter translation

**Files:** `input.rs`; hooks: `window.rs` `insert_hitbox` (`content_mask` passed through `crate::fast::layers::input::hitbox_mask(self, mask)`, which intersects with the viewport of the layer being painted), div.rs `ScrollHandle::bounds_for_item`/`child_bounds` (translate by the live layer's `delta` on read).

- [ ] Tests: `hitboxes_in_overscan_do_not_hit` (a row painted in overscan below the viewport is not hovered by a pointer placed where it lies); `tooltips_requested_inside_are_dropped_on_composite`; `bounds_for_item_is_current_after_composited_scrolls`.
- [ ] Commit — `gpui: keep hit testing and scroll handle bounds true under scroll layers`.

---

## M6 — Virtual lists (stream L; after M3 and M4)

Branch `scroll-layers-lists`. Owns `fast/layers/lists.rs`, hooks in uniform_list.rs (before `(self.render_items)(visible_range)` ~484, item origins ~500, `measure_item` in prepaint ~358) and list.rs (`layout_items` render calls ~1104, item origins ~1333, overdraw).

**Interfaces:** `LayerRows { painted: BTreeSet<usize> /* row indices in the layer */, row_origins: FxHashMap<usize, Point<Pixels>> /* content space */ }`; `pub(crate) fn rows_to_render(window, id, visible: Range<usize>, overscan: usize) -> RowPlan { render: Vec<Range<usize>>, keep: Range<usize> }`; `pub(crate) fn snap_item_origin(window, origin) -> Point<Pixels>` (M3's snapping, applied to list item origins).

### Task 6.1: uniform_list partial rendering

- [ ] Tests (`fast/tests/layers_lists.rs`): `uniform_list_renders_only_new_rows_when_scrolling` (count `render_items` calls and requested ranges: after promotion, scrolling one row renders one row per direction); `uniform_list_matches_layers_off` (scene with composites expanded equals layers-off over 50 random wheel frames); `measure_item_is_skipped_on_composite_frames`.
- [ ] Implement per spec §8; rows entering the overscan are prepainted/painted into the layer at their content positions and tiles over them re-hashed (only those tiles become dirty).
- [ ] Commit — `gpui: render only the rows a scrolled uniform list uncovers`.

### Task 6.2: list partial rendering

- [ ] Tests: `list_renders_only_new_rows_when_scrolling`; `list_matches_layers_off_with_varying_heights`; `a_list_splice_repaints_the_layer` (`ListState::splice` → `Repaint`).
- [ ] Implement per spec §8 using `SumTree` heights for content positions.
- [ ] Commit — `gpui: render only the rows a scrolled list uncovers`.

---

## M7 — Verification and benchmarks (stream V; starts after M1, grows with the others)

Branch `scroll-layers-verify`. Owns `fast/tests/layers_oracle.rs`, `crates/gpui_perf/**`.

### Task 7.1: Real-wheel scroll scenarios and a GPUI-Kit-like page

**Files:** `crates/gpui_perf/src/showcase/auto.rs` (`bounce` → dispatch `ScrollWheelEvent` with the pointer over the scrolled area instead of `set_offset`), `crates/gpui_perf/src/scenarios/` (new headless scenarios `scroll-child-view`, `scroll-same-view`, `scroll-uniform-list`, `scroll-list`, each dispatching wheel events with the pointer over content and a GPUI-Kit-like page: a sidebar view, a scroll container with a child page view of 24 sections of buttons with hover styles, icons (svg), and text).

- [ ] Test: `cargo run -p gpui_perf --release -- --headless --scenario scroll --frames 50 --verify` passes on `main`'s code path (layers off) — this validates the scenarios themselves before layers exist.
- [ ] Commit — `gpui_perf: scroll with real wheel events over the content`.

### Task 7.2: Layer oracle

**Files:** `fast/tests/layers_oracle.rs`.

- Two windows, same view type and same random history (seeded, 300 steps): wheel scrolls with fractional deltas (0.37, 1.0, 12.5 px) at scale factors 1.0 and 1.25, programmatic scrolls, mouse moves, clicks, key presses into a focused element inside content, content mutations (row text/colour change, insert/remove rows), hover-only moves, window resize. Window A `set_scroll_layers(true)`, window B `set_scroll_layers(false)`.
- Every frame: expand A's scene (replace each tile quad by `LayerFrame::tile_scene` primitives translated to the quad's bounds origin and clipped to its mask; drop quads that were fully culled) and compare with B's scene primitive by primitive (after sorting both by order); compare hit tests at 32 random points; record every listener invocation's observed event position and `Hitbox` bounds in both windows and compare.
- [ ] Commit — `gpui: check scroll layers against drawing from scratch`.

### Task 7.3: `--verify` coverage and stats in the reports

- [ ] `gpui_perf --headless --verify` runs every scroll scenario with layers on and off and compares the expanded scenes as in 7.2; the headless and `--auto` reports print `layer_frames_composited`, `tiles_dirtied`, `layer_rebuilds_for_input` per frame.
- [ ] Commit — `gpui_perf: verify and report scroll layers`.

---

## M8 — Integration (one agent, after M2–M7 merge into `scroll-layers`)

### Task 8.1: End-to-end on the Button story

- [ ] Build GPUI Kit's story against the branch (patch config as in the investigation: a `--config <file>` with `[patch."https://github.com/longbridge/gpui-fast"]` entries; back up and restore gpui-kit's `Cargo.lock`), run with the investigation branch's `GPUI_FAST_AUTOSCROLL=1` driver ported to `gpui_perf` or cherry-picked locally, and measure main-thread draw per frame and process CPU before/after. Target: draw ≤ 35 % of today's excluding the GPUI Kit ancestor share (spec §2).
- [ ] Tune tile size (256/512/1024), overscan (0.5/1/2 viewports) and margin by the `gpui_perf` scroll scenarios; record the chosen values in the spec.

### Task 8.2: Full verification and docs

- [ ] `cargo test -p gpui --features test-support`, `cargo test -p gpui_wgpu`, `cargo clippy -p gpui -p gpui_wgpu -p gpui_perf --all-targets`, `script/check-upstream`, `gpui_perf --headless --verify`, `gpui_perf --auto` with layers on and off; no scenario regresses > 3 % in instructions.
- [ ] Write `docs/scroll-layers.md` (how layers work, when they fall back, how to verify and measure), link it from `docs/architecture.md` and README's performance section.
- [ ] PR `scroll-layers` → `main`.

---

## Parallel execution map

```
M1 (1 agent) ──┬── M2 renderer (agent R) ─────────────────────────────┐
               ├── M3 paint (agent P) ──┬── M5 input (agent N) ───────┤
               ├── M4 invalidation (I) ─┴── M6 lists (agent L) ───────┼── M8 (1 agent)
               └── M7 verify (agent V, continuous) ───────────────────┘
```

Concurrency after M1: 4 agents (R, P, I, V), then 6 (N and L join when P and I have merged their first tasks). File ownership is disjoint except `elements/div.rs`, where P, I and N edit different, non-adjacent lines; merge order P → I → N resolves any textual conflict.
