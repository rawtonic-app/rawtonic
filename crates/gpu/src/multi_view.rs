//! Multi view on the native display: several photographs side by side in
//! the viewer (Capture One's Multi view, Lightroom's Survey). The primary
//! photograph is the live preview frame (`WgpuDisplay::current_bind_group`);
//! every other cell draws its own developed texture, rendered once at the
//! cell's size by the app with the same GPU pipeline and kept here until its
//! recipe or size changes. Zoomed in, every cell shows the same photograph
//! point at the same magnification (Lightroom's locked zoom) or, without the
//! lock, each cell its own zoom and pan: the cell's whole-photograph texture
//! enlarged at once, then the visible region developed at that
//! magnification on top. Develop pixels never leave the
//! GPU process: the webview only sends cell rectangles and the zoom, and
//! draws captions and frames over them.

use std::collections::HashMap;

use super::DisplayTransform;

/// A cell of the multi view: the photograph at `path` is shown contained and
/// centred in `area` (device pixels, window coordinates).
#[derive(Clone, Debug, PartialEq)]
pub struct MultiViewCell {
    pub path: String,
    pub area: [f32; 4],
}

/// The zoom every cell shares: `scale` device pixels per full-resolution
/// pixel of the shown (cropped, turned) photograph, and the photograph point
/// at each cell's centre (`center`, 0–1 of its shown width and height).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CellZoom {
    pub scale: f32,
    pub center: [f32; 2],
}

/// What part of its photograph a texture holds: the shown photograph's size
/// in full-resolution pixels and, for a zoomed region, that region (x, y,
/// w, h in those pixels); `region` None is the whole photograph.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CellExtent {
    pub shown: [f32; 2],
    pub region: Option<[f32; 4]>,
}

/// The zooms a layout change carries: every cell's shared zoom (the zoom
/// lock) or each cell's own (absent: fit).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct CellZooms {
    pub shared: Option<CellZoom>,
    pub cells: HashMap<String, CellZoom>,
}

/// The live frame on screen as the layout changes: the photograph at
/// `path`, the top left `size` of `texture`, placed at `placed` (window
/// device px; None: not known).
pub(crate) struct LiveFrame<'a> {
    pub path: &'a str,
    pub texture: &'a wgpu::Texture,
    pub size: (u32, u32),
    pub placed: Option<[f32; 4]>,
    /// Shown nearest-neighbour (the editor's maximum zoom).
    pub pixelated: bool,
}

/// A secondary photograph's developed frame (display-encoded RGBA8).
pub(crate) struct CellTexture {
    /// What it was rendered from (recipe and size); 0 for a stand-in that
    /// waits for its own render (see [`MultiView::adopt_live_frame`]).
    key: u64,
    /// Unknown for a stand-in: then it is only shown to fit.
    extent: Option<CellExtent>,
    /// Sampled nearest-neighbour at this magnification: an adopted live
    /// frame keeps the sampling it was shown with (the editor's maximum
    /// zoom) while its cell stays at that zoom, until replaced.
    pixelated_at: Option<f32>,
    _texture: wgpu::Texture,
    width: u32,
    height: u32,
    uniforms: wgpu::Buffer,
    bind_group: wgpu::BindGroup,
}

/// The cells shown and the secondary textures kept for them. Empty (no
/// cells) is the single view; textures exist only for current cells.
#[derive(Default)]
pub struct MultiView {
    cells: Vec<MultiViewCell>,
    primary: Option<String>,
    /// Each secondary's whole photograph at its cell's size.
    textures: HashMap<String, CellTexture>,
    /// Zoomed in: each secondary's visible region at the zoom's magnification.
    regions: HashMap<String, CellTexture>,
    /// The zoom every secondary shares (the zoom lock), or None.
    zoom: Option<CellZoom>,
    /// Without the lock: each secondary's own zoom (absent: fit).
    cell_zooms: HashMap<String, CellZoom>,
}

/// Where a photograph of `width` × `height` sits in `area`: contained and
/// centred, as the webview places the primary in its cell.
pub fn fit_rect(area: [f32; 4], width: f32, height: f32) -> [f32; 4] {
    let [x, y, w, h] = area;
    if width <= 0.0 || height <= 0.0 || w <= 0.0 || h <= 0.0 {
        return [x, y, 0.0, 0.0];
    }
    let (fit_w, fit_h) = if width / height > w / h {
        (w, w * height / width)
    } else {
        (h * width / height, h)
    };
    [x + (w - fit_w) / 2.0, y + (h - fit_h) / 2.0, fit_w, fit_h]
}

/// Where a texture of `extent` sits in `area` at `zoom`: the photograph
/// enlarged by `zoom.scale` with `zoom.center` at the area's centre (the
/// part outside the area is clipped), the texture covering its region of it.
pub fn zoomed_rect(area: [f32; 4], extent: CellExtent, zoom: CellZoom) -> [f32; 4] {
    let s = zoom.scale;
    let ox = area[0] + area[2] / 2.0 - zoom.center[0] * extent.shown[0] * s;
    let oy = area[1] + area[3] / 2.0 - zoom.center[1] * extent.shown[1] * s;
    match extent.region {
        Some([rx, ry, rw, rh]) => [ox + rx * s, oy + ry * s, rw * s, rh * s],
        None => [ox, oy, extent.shown[0] * s, extent.shown[1] * s],
    }
}

/// The part of a shown photograph (`shown`: width, height in full-resolution
/// px) a cell `area` (device px) shows at `zoom`, grown by `margin` of the
/// visible size on each side so that panning stays sharp, inside the
/// photograph. x, y, w, h in full-resolution px of the shown photograph.
pub fn zoomed_region(area: [f32; 4], shown: [f32; 2], zoom: CellZoom, margin: f32) -> [f32; 4] {
    let scale = zoom.scale.max(1e-6);
    let (w, h) = (area[2] / scale, area[3] / scale);
    let (cx, cy) = (zoom.center[0] * shown[0], zoom.center[1] * shown[1]);
    let x1 = (cx - w * (0.5 + margin)).clamp(0.0, shown[0]);
    let y1 = (cy - h * (0.5 + margin)).clamp(0.0, shown[1]);
    let x2 = (cx + w * (0.5 + margin)).clamp(0.0, shown[0]);
    let y2 = (cy + h * (0.5 + margin)).clamp(0.0, shown[1]);
    [x1, y1, (x2 - x1).max(0.0), (y2 - y1).max(0.0)]
}

/// The extent of a live frame of `size` texels placed at `rect` (window
/// device px) at `zoom`: the whole shown photograph, its size in
/// full-resolution px following from the zoom. None when `rect` is not a
/// placement of that frame (hidden, empty, or of another aspect ratio).
pub fn live_extent(rect: [f32; 4], zoom: CellZoom, size: (u32, u32)) -> Option<CellExtent> {
    let [x, y, w, h] = rect;
    let (tw, th) = (size.0 as f32, size.1 as f32);
    let placed = [x, y, w, h].iter().all(|v| v.is_finite())
        && w > 1.0
        && h > 1.0
        && x > -1e5
        && y > -1e5
        && tw > 0.0
        && th > 0.0
        && ((w / h) / (tw / th) - 1.0).abs() <= 0.02
        && zoom.scale.is_finite()
        && zoom.scale > 0.0;
    placed.then(|| CellExtent {
        shown: [w / zoom.scale, h / zoom.scale],
        region: None,
    })
}

/// Whether a live frame of `size` texels placed at `rect` is its photograph
/// shown at `zoom` in a cell `area` (within 2 device px and 1 %).
pub fn placed_at_zoom(rect: [f32; 4], area: [f32; 4], zoom: CellZoom, size: (u32, u32)) -> bool {
    live_extent(rect, zoom, size).is_some_and(|extent| {
        let expected = zoomed_rect(area, extent, zoom);
        let tolerance = 2.0 + 0.01 * rect[2].max(rect[3]);
        (expected[0] - rect[0]).abs() <= tolerance && (expected[1] - rect[1]).abs() <= tolerance
    })
}

/// The texels (x, y, w, h) of a whole-photograph texture of `size` that a
/// cell `area` shows at `zoom` (`shown`: the photograph in full-resolution
/// px), whole texels around it; None when it shows none.
pub fn visible_texels(
    area: [f32; 4],
    shown: [f32; 2],
    zoom: CellZoom,
    size: (u32, u32),
) -> Option<[u32; 4]> {
    let [rx, ry, rw, rh] = zoomed_region(area, shown, zoom, 0.0);
    if rw <= 0.0 || rh <= 0.0 || shown[0] <= 0.0 || shown[1] <= 0.0 {
        return None;
    }
    let (kx, ky) = (size.0 as f32 / shown[0], size.1 as f32 / shown[1]);
    let x1 = ((rx * kx).floor().max(0.0) as u32).min(size.0);
    let y1 = ((ry * ky).floor().max(0.0) as u32).min(size.1);
    let x2 = (((rx + rw) * kx).ceil().max(0.0) as u32).min(size.0);
    let y2 = (((ry + rh) * ky).ceil().max(0.0) as u32).min(size.1);
    (x2 > x1 && y2 > y1).then(|| [x1, y1, x2 - x1, y2 - y1])
}

/// Whether `region` holds everything a cell `area` shows at `zoom`.
pub fn region_covers(region: [f32; 4], area: [f32; 4], shown: [f32; 2], zoom: CellZoom) -> bool {
    let needed = zoomed_region(area, shown, zoom, 0.0);
    let eps = 0.5;
    region[0] <= needed[0] + eps
        && region[1] <= needed[1] + eps
        && region[0] + region[2] + eps >= needed[0] + needed[2]
        && region[1] + region[3] + eps >= needed[1] + needed[3]
}

/// `rect` (x, y, w, h) as a scissor rectangle inside a `size` target, or
/// None when nothing of it is inside.
fn scissor(rect: [f32; 4], size: [u32; 2]) -> Option<[u32; 4]> {
    let x1 = rect[0].max(0.0).floor() as u32;
    let y1 = rect[1].max(0.0).floor() as u32;
    let x2 = ((rect[0] + rect[2]).max(0.0).ceil() as u32).min(size[0]);
    let y2 = ((rect[1] + rect[3]).max(0.0).ceil() as u32).min(size[1]);
    (x2 > x1 && y2 > y1).then(|| [x1, y1, x2 - x1, y2 - y1])
}

fn intersect(a: [f32; 4], b: [f32; 4]) -> [f32; 4] {
    let x1 = a[0].max(b[0]);
    let y1 = a[1].max(b[1]);
    let x2 = (a[0] + a[2]).min(b[0] + b[2]);
    let y2 = (a[1] + a[3]).min(b[1] + b[3]);
    [x1, y1, (x2 - x1).max(0.0), (y2 - y1).max(0.0)]
}

/// What the display pipeline binds for one draw.
pub(crate) struct DisplayBindings<'a> {
    pub layout: &'a wgpu::BindGroupLayout,
    pub sampler: &'a wgpu::Sampler,
    /// Bound as the (unused) mask overlay of secondary cells.
    pub empty_overlay: &'a wgpu::TextureView,
}

/// Which texture of a cell an upload is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CellLayer {
    /// The whole photograph at the cell's size.
    Whole,
    /// The visible region at the zoom's magnification.
    Region,
}

impl CellTexture {
    pub(crate) fn bind_group(&self) -> &wgpu::BindGroup {
        &self.bind_group
    }

    pub(crate) fn uniforms(&self) -> &wgpu::Buffer {
        &self.uniforms
    }

    pub(crate) fn size(&self) -> (u32, u32) {
        (self.width, self.height)
    }
}

impl MultiView {
    pub fn is_active(&self) -> bool {
        !self.cells.is_empty()
    }

    pub fn cells(&self) -> &[MultiViewCell] {
        &self.cells
    }

    pub fn primary(&self) -> Option<&str> {
        self.primary.as_deref()
    }

    pub fn cell(&self, path: &str) -> Option<&MultiViewCell> {
        self.cells.iter().find(|cell| cell.path == path)
    }

    /// The zoom all secondaries share (the zoom lock is on).
    pub fn zoom(&self) -> Option<CellZoom> {
        self.zoom
    }

    /// Each secondary's own zoom (the zoom lock is off).
    pub fn cell_zooms(&self) -> &HashMap<String, CellZoom> {
        &self.cell_zooms
    }

    /// The zoom `path`'s cell is shown at (None: fit).
    pub fn zoom_of(&self, path: &str) -> Option<CellZoom> {
        self.cell_zooms.get(path).copied().or(self.zoom)
    }

    /// Whether any cell is zoomed in.
    pub fn is_zoomed(&self) -> bool {
        self.zoom.is_some() || !self.cell_zooms.is_empty()
    }

    /// Shows `cells` with `primary` as the live photograph. Textures of
    /// photographs no longer shown are released.
    pub fn set_layout(&mut self, primary: Option<String>, cells: Vec<MultiViewCell>) {
        let shown = |path: &String| cells.iter().any(|cell| &cell.path == path);
        self.textures.retain(|path, _| shown(path));
        self.regions.retain(|path, _| shown(path));
        self.cell_zooms.retain(|path, _| shown(path));
        self.primary = primary;
        self.cells = cells;
    }

    /// Zooms every cell to `zoom` (None: fit; the zoom lock).
    pub fn set_zoom(&mut self, zoom: Option<CellZoom>) {
        self.set_zooms(zoom, HashMap::new());
    }

    /// Zooms the cells: every one to `shared` (the zoom lock), or each in
    /// `cells` to its own zoom and the others to fit. A region developed for
    /// another magnification of its cell is released; panning keeps it (it
    /// moves along).
    pub fn set_zooms(&mut self, shared: Option<CellZoom>, cells: HashMap<String, CellZoom>) {
        let before: HashMap<String, Option<CellZoom>> = self
            .regions
            .keys()
            .map(|path| (path.clone(), self.zoom_of(path)))
            .collect();
        self.zoom = shared;
        self.cell_zooms = cells;
        let after: HashMap<String, Option<CellZoom>> = before
            .keys()
            .map(|path| (path.clone(), self.zoom_of(path)))
            .collect();
        self.regions
            .retain(|path, _| match (before[path], after[path]) {
                (Some(a), Some(b)) => (a.scale - b.scale).abs() <= a.scale * 1e-3,
                _ => false,
            });
    }

    /// Back to the single view: every secondary texture is released.
    pub fn clear(&mut self) {
        self.cells.clear();
        self.primary = None;
        self.textures.clear();
        self.regions.clear();
        self.zoom = None;
        self.cell_zooms.clear();
    }

    /// The render key of the whole-photograph texture kept for `path` (0: a
    /// stand-in).
    pub fn texture_key(&self, path: &str) -> Option<u64> {
        self.textures.get(path).map(|texture| texture.key)
    }

    /// The render key and extent of the zoomed region kept for `path`.
    pub fn region_of(&self, path: &str) -> Option<(u64, CellExtent)> {
        self.regions
            .get(path)
            .and_then(|texture| Some((texture.key, texture.extent?)))
    }

    /// GPU memory held by the secondary textures (RGBA8), in bytes.
    pub fn texture_bytes(&self) -> u64 {
        self.textures
            .values()
            .chain(self.regions.values())
            .map(|t| t.width as u64 * t.height as u64 * 4)
            .sum()
    }

    pub fn texture_count(&self) -> usize {
        self.textures.len() + self.regions.len()
    }

    fn make_texture(
        device: &wgpu::Device,
        bindings: &DisplayBindings,
        key: u64,
        extent: Option<CellExtent>,
        texture: wgpu::Texture,
    ) -> CellTexture {
        let (width, height) = (texture.width(), texture.height());
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        let uniforms = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("Multi view cell transform"),
            size: std::mem::size_of::<DisplayTransform>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("Multi view cell"),
            layout: bindings.layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: uniforms.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(&view),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::Sampler(bindings.sampler),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: wgpu::BindingResource::TextureView(bindings.empty_overlay),
                },
            ],
        });
        CellTexture {
            key,
            extent,
            pixelated_at: None,
            _texture: texture,
            width,
            height,
            uniforms,
            bind_group,
        }
    }

    fn cell_texture(device: &wgpu::Device, width: u32, height: u32) -> wgpu::Texture {
        device.create_texture(&wgpu::TextureDescriptor {
            label: Some("Multi view cell"),
            size: wgpu::Extent3d {
                width: width.max(1),
                height: height.max(1),
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        })
    }

    /// `rgba` (display-encoded RGBA8, `width` × `height`) on the GPU with
    /// its own transform, bound like a cell (a stand-in, key 0).
    pub(crate) fn uploaded_texture(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        bindings: &DisplayBindings,
        width: u32,
        height: u32,
        rgba: &[u8],
    ) -> CellTexture {
        let texture = Self::cell_texture(device, width, height);
        queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            rgba,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(width * 4),
                rows_per_image: Some(height),
            },
            wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
        );
        Self::make_texture(device, bindings, 0, None, texture)
    }

    /// Keeps `rgba` (display-encoded RGBA8, `width` × `height`) as `path`'s
    /// whole photograph or zoomed region (`layer`), rendered for `key`.
    /// Ignored when `path` is not shown, and a region when no longer zoomed.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn upload(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        bindings: &DisplayBindings,
        path: &str,
        key: u64,
        extent: CellExtent,
        layer: CellLayer,
        width: u32,
        height: u32,
        rgba: &[u8],
    ) {
        if self.cell(path).is_none()
            || width == 0
            || height == 0
            || (layer == CellLayer::Region && self.zoom_of(path).is_none())
        {
            return;
        }
        let mut texture = Self::uploaded_texture(device, queue, bindings, width, height, rgba);
        texture.key = key.max(1);
        texture.extent = Some(extent);
        match layer {
            CellLayer::Whole => self.textures.insert(path.to_string(), texture),
            CellLayer::Region => self.regions.insert(path.to_string(), texture),
        };
    }

    /// Keeps `rgba` (display-encoded RGBA8, `width` × `height`, the whole
    /// photograph as shown) as `path`'s stand-in (key 0, shown to fit) until
    /// its cell's own render replaces it: a cached image, so that a cell
    /// shows its photograph at once rather than the background. Never over a
    /// texture the cell already has. Whether it was kept.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn stand_in(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        bindings: &DisplayBindings,
        path: &str,
        width: u32,
        height: u32,
        rgba: &[u8],
        shown: impl FnOnce() -> Option<[f32; 2]>,
    ) -> bool {
        if self.cell(path).is_none()
            || self.primary() == Some(path)
            || self.textures.contains_key(path)
            || width == 0
            || height == 0
            || rgba.len() != width as usize * height as usize * 4
        {
            return false;
        }
        let mut texture = Self::uploaded_texture(device, queue, bindings, width, height, rgba);
        // A zoomed cell shows the stand-in enlarged where its photograph's
        // size is known (a layout shown again after a lost device, DVL3),
        // not to fit until its own render arrives.
        if self.zoom_of(path).is_some() {
            texture.extent = shown().map(|shown| CellExtent {
                shown,
                region: None,
            });
        }
        self.textures.insert(path.to_string(), texture);
        true
    }

    /// Shows `cells` with `primary` as the live photograph, zoomed to
    /// `zooms` (None: as they are) in the same step, so that the photograph
    /// that stops being the primary is placed at its own zoom from the first
    /// present on: its `live` frame stays on screen as its cell's frame
    /// where it was ([`MultiView::adopt_live_frame`]) until the cell's own
    /// render replaces it. A zoom that arrived after the layout left the old
    /// primary at fit until then (G74, WMV3).
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn show(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        bindings: &DisplayBindings,
        primary: Option<String>,
        cells: Vec<MultiViewCell>,
        zooms: Option<CellZooms>,
        live: Option<LiveFrame>,
    ) {
        let old = self.primary.clone();
        let old_area = old
            .as_deref()
            .and_then(|path| self.cell(path))
            .map(|cell| cell.area);
        self.set_layout(primary.clone(), cells);
        if let Some(zooms) = zooms
            && (self.zoom != zooms.shared || self.cell_zooms != zooms.cells)
        {
            self.set_zooms(zooms.shared, zooms.cells);
        }
        if let (Some(old), Some(live)) = (old, live)
            && primary.as_deref() != Some(old.as_str())
            && live.path == old
        {
            // Only a placement that is the old primary's zoom in its cell
            // (not one the next photograph's editor already sent).
            let placed = live.placed.filter(|rect| {
                self.zoom_of(&old)
                    .zip(old_area)
                    .is_some_and(|(zoom, area)| placed_at_zoom(*rect, area, zoom, live.size))
            });
            self.adopt_live_frame(
                device,
                queue,
                bindings,
                &old,
                live.texture,
                live.size,
                placed,
            );
            let scale = self.zoom_of(&old).map(|zoom| zoom.scale);
            if placed.is_some() && live.pixelated {
                for texture in [self.textures.get_mut(&old), self.regions.get_mut(&old)]
                    .into_iter()
                    .flatten()
                    .filter(|texture| texture.key == 0)
                {
                    texture.pixelated_at = scale;
                }
            }
        }
    }

    /// The live frame of `path` (the primary's preview, `width` × `height`
    /// of `source`, placed at `placed` on screen: window device px, None
    /// when not known) stays on screen as its cell's frame when another
    /// photograph becomes the primary, until the cell's own render replaces
    /// it (key 0). A GPU copy: no re-render, no readback.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn adopt_live_frame(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        bindings: &DisplayBindings,
        path: &str,
        source: &wgpu::Texture,
        (width, height): (u32, u32),
        placed: Option<[f32; 4]>,
    ) {
        // Zoomed in, the live frame covered `placed`: the whole shown
        // photograph enlarged by the zoom, so the photograph's size follows
        // from it and the frame stays placed as it was (and moves with a
        // pan). At fit, or when the placement is not known, it is shown to
        // fit until the cell's own render arrives.
        let zoom = self.zoom_of(path);
        let extent = zoom
            .zip(placed)
            .and_then(|(zoom, rect)| live_extent(rect, zoom, (width, height)));
        // A zoomed cell that still has its own placed frame keeps it rather
        // than a live frame that can only be shown to fit, so it never jumps
        // to fit (and back) while every other cell stays zoomed. Its sharp
        // region was rendered while the photograph was the live one and may
        // predate edits made since: it is released, as before.
        if extent.is_none() && self.keeps_zoomed_frame(path) {
            self.regions.remove(path);
            return;
        }
        if !self.copy_frame(
            device,
            queue,
            bindings,
            path,
            source,
            (width, height),
            0,
            extent,
        ) {
            return;
        }
        self.regions.remove(path);
        // Its visible part, as sharp as it was on screen, stays drawn over
        // the cell's own whole-photograph render (at the cell's size, soft
        // at 100 %) until the cell's region is developed.
        if let (Some(zoom), Some(extent)) = (zoom, extent) {
            self.adopt_visible_region(
                device,
                queue,
                bindings,
                path,
                source,
                (width, height),
                zoom,
                extent,
            );
        }
    }

    /// Copies the part of `source` (the live frame of `path`, `extent`, the
    /// whole shown photograph) that its cell shows at `zoom` as the cell's
    /// zoomed region (key 0: developed again).
    #[allow(clippy::too_many_arguments)]
    fn adopt_visible_region(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        bindings: &DisplayBindings,
        path: &str,
        source: &wgpu::Texture,
        (width, height): (u32, u32),
        zoom: CellZoom,
        extent: CellExtent,
    ) {
        let Some(area) = self.cell(path).map(|cell| cell.area) else {
            return;
        };
        let Some([x, y, w, h]) = visible_texels(area, extent.shown, zoom, (width, height)) else {
            return;
        };
        let texture = Self::cell_texture(device, w, h);
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("Multi view live region copy"),
        });
        encoder.copy_texture_to_texture(
            wgpu::TexelCopyTextureInfo {
                texture: source,
                mip_level: 0,
                origin: wgpu::Origin3d { x, y, z: 0 },
                aspect: wgpu::TextureAspect::All,
            },
            texture.as_image_copy(),
            wgpu::Extent3d {
                width: w,
                height: h,
                depth_or_array_layers: 1,
            },
        );
        queue.submit(Some(encoder.finish()));
        let (kx, ky) = (
            extent.shown[0] / width as f32,
            extent.shown[1] / height as f32,
        );
        let region = CellExtent {
            shown: extent.shown,
            region: Some([x as f32 * kx, y as f32 * ky, w as f32 * kx, h as f32 * ky]),
        };
        let texture = Self::make_texture(device, bindings, 0, Some(region), texture);
        self.regions.insert(path.to_string(), texture);
    }

    /// Whether `texture` of `path`'s cell is sampled nearest-neighbour now:
    /// an adopted live frame shown so, while the cell stays at its zoom.
    fn pixelated(&self, path: &str, texture: &CellTexture) -> bool {
        texture.pixelated_at.is_some_and(|at| {
            self.zoom_of(path)
                .is_some_and(|zoom| (zoom.scale - at).abs() <= at * 1e-3)
        })
    }

    /// Whether `path`'s cell shows its adopted live region (key 0, as sharp
    /// as the photograph was on screen as the primary) and it still covers
    /// what the cell shows: an interim region, developed from a reduced
    /// source, would only make it softer until the developed one arrives.
    pub fn keeps_live_region(&self, path: &str) -> bool {
        let (Some(cell), Some(zoom), Some(region)) =
            (self.cell(path), self.zoom_of(path), self.regions.get(path))
        else {
            return false;
        };
        region.key == 0
            && region.extent.is_some_and(|extent| {
                extent
                    .region
                    .is_some_and(|r| region_covers(r, cell.area, extent.shown, zoom))
            })
    }

    /// Whether `path`'s cell is zoomed in and already has a whole-photograph
    /// frame placed for its zoom (a known extent): the old primary's live
    /// frame, shown only to fit, would replace a frame shown as asked.
    fn keeps_zoomed_frame(&self, path: &str) -> bool {
        self.zoom_of(path).is_some()
            && self
                .textures
                .get(path)
                .is_some_and(|texture| texture.extent.is_some())
    }

    /// Copies the top left `width` × `height` of `source` (a developed
    /// whole photograph, display-encoded RGBA8) on the GPU as `path`'s
    /// whole-photograph texture, rendered for `key` (0: a stand-in) and
    /// covering `extent` (None: shown to fit only). False when `path` is
    /// not shown or the size does not fit `source`.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn copy_frame(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        bindings: &DisplayBindings,
        path: &str,
        source: &wgpu::Texture,
        (width, height): (u32, u32),
        key: u64,
        extent: Option<CellExtent>,
    ) -> bool {
        if self.cell(path).is_none()
            || width == 0
            || height == 0
            || width > source.width()
            || height > source.height()
        {
            return false;
        }
        let texture = Self::cell_texture(device, width, height);
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("Multi view live frame copy"),
        });
        encoder.copy_texture_to_texture(
            source.as_image_copy(),
            texture.as_image_copy(),
            wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
        );
        rawtonic_rawdecode::gpu_submit::submit(
            device,
            queue,
            "multi view frame copy",
            0,
            Some(encoder.finish()),
        );
        let texture = Self::make_texture(device, bindings, key, extent, texture);
        self.textures.insert(path.to_string(), texture);
        true
    }

    /// Releases `path`'s zoomed region (its recipe changed).
    pub fn remove_region(&mut self, path: &str) {
        self.regions.remove(path);
    }

    /// The pixel size of `path`'s whole-photograph texture.
    pub fn texture_size(&self, path: &str) -> Option<(u32, u32)> {
        self.textures.get(path).map(CellTexture::size)
    }

    fn placement(&self, path: &str, area: [f32; 4], texture: &CellTexture) -> [f32; 4] {
        match (self.zoom_of(path), texture.extent) {
            (Some(zoom), Some(extent)) => zoomed_rect(area, extent, zoom),
            _ => fit_rect(area, texture.width as f32, texture.height as f32),
        }
    }

    /// The screen area a texture covers in `area`: the whole cell for the
    /// whole photograph (around it the viewer's background), only its own
    /// rectangle for a zoomed region (drawn over the enlarged photograph).
    fn clip(
        &self,
        path: &str,
        area: [f32; 4],
        texture: &CellTexture,
        layer: CellLayer,
    ) -> [f32; 4] {
        match layer {
            CellLayer::Whole => area,
            CellLayer::Region => intersect(area, self.placement(path, area, texture)),
        }
    }

    /// The textures drawn in a cell, in order: the whole photograph, then
    /// (zoomed in) its sharp region.
    fn layers(&self, path: &str) -> impl Iterator<Item = (&CellTexture, CellLayer)> {
        let region = self.zoom_of(path).and(self.regions.get(path));
        self.textures
            .get(path)
            .map(|t| (t, CellLayer::Whole))
            .into_iter()
            .chain(region.map(|t| (t, CellLayer::Region)))
    }

    /// Each cell's layers as drawn now (area, texture size and key, extent,
    /// placement, clip), for the log: a cell drawn outside its area shows
    /// the background only.
    pub fn describe(&self) -> String {
        let mut out = String::new();
        for cell in &self.cells {
            let primary = self.primary.as_deref() == Some(cell.path.as_str());
            let name = cell.path.rsplit(['/', '\\']).next().unwrap_or(&cell.path);
            out += &format!(
                "\n  {name}{} area {:?} zoom {:?}:",
                if primary { " (primary)" } else { "" },
                cell.area,
                self.zoom_of(&cell.path).map(|z| (z.scale, z.center))
            );
            let mut any = false;
            for (texture, layer) in self.layers(&cell.path) {
                any = true;
                out += &format!(
                    " [{layer:?} {}x{} key {} extent {:?} at {:?} clip {:?}]",
                    texture.width,
                    texture.height,
                    texture.key,
                    texture.extent.map(|e| (e.shown, e.region)),
                    self.placement(&cell.path, cell.area, texture),
                    self.clip(&cell.path, cell.area, texture, layer)
                );
            }
            if !any {
                out += " nothing";
            }
        }
        out
    }

    /// Writes each cell's placement for a `window` (device px) frame.
    pub(crate) fn prepare(&self, queue: &wgpu::Queue, window: [f32; 2], background: [f32; 4]) {
        for cell in &self.cells {
            for (texture, layer) in self.layers(&cell.path) {
                let size = [texture.width as f32, texture.height as f32];
                let transform = DisplayTransform {
                    rect: self.placement(&cell.path, cell.area, texture),
                    clip: self.clip(&cell.path, cell.area, texture, layer),
                    window,
                    image_size: size,
                    texture_size: size,
                    pixelated: if self.pixelated(&cell.path, texture) {
                        1.0
                    } else {
                        0.0
                    },
                    rotation: 0.0,
                    bg_primary: background,
                    bg_secondary: background,
                    overlay_color: [0.0; 4],
                    overlay_mode: [0.0; 4],
                    box_rect: [0.0; 4],
                    split: [0.0; 4],
                };
                queue.write_buffer(&texture.uniforms, 0, bytemuck::bytes_of(&transform));
            }
        }
    }
}
/// The live primary frame for [`encode_display_pass`]: its bind group, the
/// split view's before (the empty one without a split) and the screen area
/// it may cover.
pub(crate) struct PrimaryDraw<'a> {
    pub bind_group: &'a wgpu::BindGroup,
    pub before: &'a wgpu::BindGroup,
    pub clip: [f32; 4],
}

/// The display pipeline and the before bound with every draw that shows no
/// split (secondary cells; the split is single-view only).
pub(crate) struct DisplayPass<'a> {
    pub pipeline: &'a wgpu::RenderPipeline,
    pub empty_before: &'a wgpu::BindGroup,
}

/// Records the display's one render pass into `view` (`size` device px):
/// cleared with `clear`, then (in multi view) every secondary cell that has
/// a frame, then the live primary frame within its clip. In multi view the
/// primary's own cell is drawn from its texture only while the live frame
/// does not show it (`primary` is None), e.g. while the next photograph's
/// first frame is on its way.
pub(crate) fn encode_display_pass(
    encoder: &mut wgpu::CommandEncoder,
    view: &wgpu::TextureView,
    size: [u32; 2],
    display: DisplayPass,
    clear: [f32; 4],
    multi: Option<&MultiView>,
    primary: Option<PrimaryDraw>,
) {
    let mut rpass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
        label: Some("Display pass"),
        color_attachments: &[Some(wgpu::RenderPassColorAttachment {
            view,
            resolve_target: None,
            ops: wgpu::Operations {
                load: wgpu::LoadOp::Clear(wgpu::Color {
                    r: clear[0] as f64,
                    g: clear[1] as f64,
                    b: clear[2] as f64,
                    a: clear[3] as f64,
                }),
                store: wgpu::StoreOp::Store,
            },
            depth_slice: None,
        })],
        depth_stencil_attachment: None,
        timestamp_writes: None,
        occlusion_query_set: None,
        multiview_mask: std::num::NonZero::new(0),
    });
    rpass.set_pipeline(display.pipeline);
    rpass.set_bind_group(1, display.empty_before, &[]);
    let mut primary_clip = primary.as_ref().map(|p| p.clip);
    if let Some(multi) = multi {
        let live = primary.is_some();
        for cell in &multi.cells {
            let is_primary = multi.primary.as_deref() == Some(cell.path.as_str());
            if is_primary {
                // The live frame stays inside its cell.
                primary_clip = primary_clip.map(|clip| intersect(clip, cell.area));
                if live {
                    continue;
                }
            }
            for (texture, layer) in multi.layers(&cell.path) {
                let Some([x, y, w, h]) =
                    scissor(multi.clip(&cell.path, cell.area, texture, layer), size)
                else {
                    continue;
                };
                rpass.set_scissor_rect(x, y, w, h);
                rpass.set_bind_group(0, &texture.bind_group, &[]);
                rpass.draw(0..4, 0..1);
            }
        }
    }
    if let (Some(primary), Some(clip)) = (primary, primary_clip)
        && let Some([x, y, w, h]) = scissor(clip, size)
    {
        rpass.set_scissor_rect(x, y, w, h);
        rpass.set_bind_group(0, primary.bind_group, &[]);
        rpass.set_bind_group(1, primary.before, &[]);
        rpass.draw(0..4, 0..1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn photographs_are_contained_and_centred_in_their_cell() {
        assert_eq!(
            fit_rect([10.0, 20.0, 400.0, 400.0], 800.0, 400.0),
            [10.0, 120.0, 400.0, 200.0]
        );
        assert_eq!(
            fit_rect([0.0, 0.0, 400.0, 200.0], 300.0, 600.0),
            [150.0, 0.0, 100.0, 200.0]
        );
        assert_eq!(fit_rect([5.0, 6.0, 0.0, 10.0], 3.0, 2.0)[2], 0.0);
    }

    struct Offscreen {
        device: wgpu::Device,
        queue: wgpu::Queue,
        pipeline: wgpu::RenderPipeline,
        layout: wgpu::BindGroupLayout,
        sampler: wgpu::Sampler,
        empty: wgpu::TextureView,
        empty_before: wgpu::BindGroup,
    }

    const SIZE: [u32; 2] = [200, 100];
    const BG: [f32; 4] = [35.0 / 255.0, 35.0 / 255.0, 35.0 / 255.0, 1.0];

    fn offscreen() -> Offscreen {
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());
        let adapter =
            pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions::default()))
                .expect("a GPU adapter");
        let (device, queue) =
            pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor::default()))
                .expect("a GPU device");
        let (pipeline, layout, before_layout) =
            super::super::display_pipeline(&device, wgpu::TextureFormat::Rgba8Unorm).unwrap();
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });
        let empty = device
            .create_texture(&wgpu::TextureDescriptor {
                label: None,
                size: wgpu::Extent3d {
                    width: 1,
                    height: 1,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::R8Unorm,
                usage: wgpu::TextureUsages::TEXTURE_BINDING,
                view_formats: &[],
            })
            .create_view(&Default::default());
        let empty_before = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: None,
            layout: &before_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::TextureView(
                    &device
                        .create_texture(&wgpu::TextureDescriptor {
                            label: None,
                            size: wgpu::Extent3d {
                                width: 1,
                                height: 1,
                                depth_or_array_layers: 1,
                            },
                            mip_level_count: 1,
                            sample_count: 1,
                            dimension: wgpu::TextureDimension::D2,
                            format: wgpu::TextureFormat::Rgba8Unorm,
                            usage: wgpu::TextureUsages::TEXTURE_BINDING,
                            view_formats: &[],
                        })
                        .create_view(&Default::default()),
                ),
            }],
        });
        Offscreen {
            device,
            queue,
            pipeline,
            layout,
            sampler,
            empty,
            empty_before,
        }
    }

    fn whole(width: u32, height: u32) -> CellExtent {
        CellExtent {
            shown: [width as f32, height as f32],
            region: None,
        }
    }

    fn solid(width: u32, height: u32, rgb: [u8; 3]) -> Vec<u8> {
        (0..width * height)
            .flat_map(|_| [rgb[0], rgb[1], rgb[2], 255])
            .collect()
    }

    /// A live frame as the display binds it: its own transform and texture.
    fn live_frame(gpu: &Offscreen, rect: [f32; 4], clip: [f32; 4]) -> wgpu::BindGroup {
        let mut primary = MultiView::default();
        primary.set_layout(
            None,
            vec![MultiViewCell {
                path: "live".into(),
                area: rect,
            }],
        );
        let bindings = DisplayBindings {
            layout: &gpu.layout,
            sampler: &gpu.sampler,
            empty_overlay: &gpu.empty,
        };
        primary.upload(
            &gpu.device,
            &gpu.queue,
            &bindings,
            "live",
            7,
            whole(8, 8),
            CellLayer::Whole,
            8,
            8,
            &solid(8, 8, [0, 0, 255]),
        );
        let texture = primary.textures.remove("live").unwrap();
        let transform = DisplayTransform {
            rect,
            clip,
            window: [SIZE[0] as f32, SIZE[1] as f32],
            image_size: [8.0, 8.0],
            texture_size: [8.0, 8.0],
            pixelated: 0.0,
            rotation: 0.0,
            bg_primary: BG,
            bg_secondary: BG,
            overlay_color: [0.0; 4],
            overlay_mode: [0.0; 4],
            box_rect: [0.0; 4],
            split: [0.0; 4],
        };
        gpu.queue
            .write_buffer(&texture.uniforms, 0, bytemuck::bytes_of(&transform));
        texture.bind_group
    }

    fn present(gpu: &Offscreen, multi: &MultiView, primary: Option<PrimaryDraw>) -> Vec<u8> {
        let target = gpu.device.create_texture(&wgpu::TextureDescriptor {
            label: None,
            size: wgpu::Extent3d {
                width: SIZE[0],
                height: SIZE[1],
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        multi.prepare(&gpu.queue, [SIZE[0] as f32, SIZE[1] as f32], BG);
        let mut encoder = gpu.device.create_command_encoder(&Default::default());
        encode_display_pass(
            &mut encoder,
            &target.create_view(&Default::default()),
            SIZE,
            DisplayPass {
                pipeline: &gpu.pipeline,
                empty_before: &gpu.empty_before,
            },
            BG,
            multi.is_active().then_some(multi),
            primary,
        );
        gpu.queue.submit(Some(encoder.finish()));
        super::super::read_texture_data_roi(
            &gpu.device,
            &gpu.queue,
            &target,
            wgpu::Origin3d::ZERO,
            wgpu::Extent3d {
                width: SIZE[0],
                height: SIZE[1],
                depth_or_array_layers: 1,
            },
            4,
        )
        .unwrap()
    }

    fn pixel(pixels: &[u8], x: u32, y: u32) -> [u8; 3] {
        let i = ((y * SIZE[0] + x) * 4) as usize;
        [pixels[i], pixels[i + 1], pixels[i + 2]]
    }

    fn close(a: [u8; 3], b: [u8; 3]) -> bool {
        a.iter().zip(b).all(|(a, b)| a.abs_diff(b) <= 1)
    }

    /// Three cells of 60 × 80 px, 10 px apart: a red and a green secondary,
    /// a primary drawn live (blue) or from its stand-in (yellow), and a
    /// fourth cell whose frame is not rendered yet.
    fn layout(gpu: &Offscreen) -> MultiView {
        let cells = vec![
            MultiViewCell {
                path: "a".into(),
                area: [5.0, 10.0, 60.0, 80.0],
            },
            MultiViewCell {
                path: "primary".into(),
                area: [75.0, 10.0, 60.0, 80.0],
            },
            MultiViewCell {
                path: "c".into(),
                area: [145.0, 10.0, 50.0, 40.0],
            },
            MultiViewCell {
                path: "pending".into(),
                area: [145.0, 55.0, 50.0, 40.0],
            },
        ];
        let mut multi = MultiView::default();
        multi.set_layout(Some("primary".into()), cells);
        let bindings = DisplayBindings {
            layout: &gpu.layout,
            sampler: &gpu.sampler,
            empty_overlay: &gpu.empty,
        };
        // A landscape frame in a portrait cell: letterboxed.
        for (path, w, h, rgb) in [
            ("a", 30, 20, [255, 0, 0]),
            ("c", 10, 8, [0, 255, 0]),
            ("primary", 6, 8, [255, 255, 0]),
        ] {
            multi.upload(
                &gpu.device,
                &gpu.queue,
                &bindings,
                path,
                1,
                whole(w, h),
                CellLayer::Whole,
                w,
                h,
                &solid(w, h, rgb),
            );
        }
        multi
    }

    #[test]
    #[ignore = "requires a native GPU; multi view cells drawn from their own textures"]
    fn each_cell_shows_its_own_texture_on_the_viewer_background() {
        let gpu = offscreen();
        let multi = layout(&gpu);
        assert_eq!(multi.texture_count(), 3);
        // The live primary covers its cell's photograph area (and is kept
        // inside the cell even though its clip is larger).
        let live = live_frame(&gpu, [75.0, 10.0, 60.0, 80.0], [0.0, 0.0, 200.0, 100.0]);
        let pixels = present(
            &gpu,
            &multi,
            Some(PrimaryDraw {
                bind_group: &live,
                before: &gpu.empty_before,
                clip: [70.0, 5.0, 70.0, 90.0],
            }),
        );
        let bg = [35, 35, 35];
        // Cell a: 30 × 20 fit into 60 × 80 → 60 × 40 centred (y 30..70).
        assert!(close(pixel(&pixels, 35, 50), [255, 0, 0]));
        assert!(close(pixel(&pixels, 35, 15), bg), "letterbox");
        assert!(close(pixel(&pixels, 70, 50), bg), "gap between cells");
        assert!(close(pixel(&pixels, 2, 2), bg), "outside every cell");
        // The live frame, not the primary's stand-in, and not beyond its cell.
        assert!(close(pixel(&pixels, 105, 50), [0, 0, 255]));
        assert!(close(pixel(&pixels, 137, 50), bg));
        assert!(close(pixel(&pixels, 170, 30), [0, 255, 0]));
        assert!(close(pixel(&pixels, 170, 75), bg), "not rendered yet");

        // While the live frame shows another photograph, the primary's
        // cell keeps its own frame (6 × 8 in 60 × 80: the whole cell).
        let pixels = present(&gpu, &multi, None);
        assert!(close(pixel(&pixels, 105, 50), [255, 255, 0]));
        assert!(close(pixel(&pixels, 35, 50), [255, 0, 0]));
    }

    #[test]
    #[ignore = "requires a native GPU; a zoomed cell's stand-in is enlarged, not at fit"]
    fn a_stand_in_in_a_zoomed_cell_is_enlarged_where_its_size_is_known() {
        let gpu = offscreen();
        let bindings = DisplayBindings {
            layout: &gpu.layout,
            sampler: &gpu.sampler,
            empty_overlay: &gpu.empty,
        };
        // Left half magenta, right half cyan; at fit the cell's left
        // quarter shows magenta, zoomed to the right half it shows cyan.
        let halves: Vec<u8> = (0..8)
            .flat_map(|_| {
                (0..10).flat_map(|x| {
                    if x < 5 {
                        [255, 0, 255, 255]
                    } else {
                        [0, 255, 255, 255]
                    }
                })
            })
            .collect();
        for (shown, expected) in [(None, [255, 0, 255]), (Some([10.0, 8.0]), [0, 255, 255])] {
            let mut multi = layout(&gpu);
            multi.set_zoom(Some(CellZoom {
                scale: 10.0,
                center: [0.75, 0.5],
            }));
            assert!(multi.stand_in(
                &gpu.device,
                &gpu.queue,
                &bindings,
                "pending",
                10,
                8,
                &halves,
                || shown
            ));
            let pixels = present(&gpu, &multi, None);
            assert!(
                close(pixel(&pixels, 150, 75), expected),
                "{shown:?}: {:?}",
                pixel(&pixels, 150, 75)
            );
        }
    }

    #[test]
    #[ignore = "requires a native GPU; a cached image stands in until the cell's render"]
    fn a_cached_image_stands_in_for_a_cell_until_its_render() {
        let gpu = offscreen();
        let mut multi = layout(&gpu);
        let bindings = DisplayBindings {
            layout: &gpu.layout,
            sampler: &gpu.sampler,
            empty_overlay: &gpu.empty,
        };
        let magenta = solid(10, 8, [255, 0, 255]);
        let stand_in = |multi: &mut MultiView, path: &str| {
            multi.stand_in(
                &gpu.device,
                &gpu.queue,
                &bindings,
                path,
                10,
                8,
                &magenta,
                || None,
            )
        };
        assert!(stand_in(&mut multi, "pending"));
        assert_eq!(
            multi.texture_key("pending"),
            Some(0),
            "waits for its own render"
        );
        assert!(!stand_in(&mut multi, "a"), "never over a cell's texture");
        assert!(
            !stand_in(&mut multi, "primary"),
            "the primary is drawn live"
        );
        assert!(!stand_in(&mut multi, "gone"), "not shown");
        assert!(
            !multi.stand_in(
                &gpu.device,
                &gpu.queue,
                &bindings,
                "c",
                10,
                9,
                &magenta,
                || None
            ),
            "a size its pixels do not have"
        );
        let pixels = present(&gpu, &multi, None);
        assert!(
            close(pixel(&pixels, 170, 75), [255, 0, 255]),
            "stand-in shown"
        );
        assert!(
            close(pixel(&pixels, 35, 50), [255, 0, 0]),
            "a keeps its render"
        );
        // The cell's own render replaces it in place.
        multi.upload(
            &gpu.device,
            &gpu.queue,
            &bindings,
            "pending",
            7,
            whole(10, 8),
            CellLayer::Whole,
            10,
            8,
            &solid(10, 8, [0, 255, 255]),
        );
        assert_eq!(multi.texture_key("pending"), Some(7));
        assert!(!stand_in(&mut multi, "pending"), "never over its render");
        let pixels = present(&gpu, &multi, None);
        assert!(close(pixel(&pixels, 170, 75), [0, 255, 255]));
    }

    #[test]
    #[ignore = "requires a native GPU; single view and released textures"]
    fn the_single_view_draws_only_the_live_frame_and_releases_cells() {
        let gpu = offscreen();
        let mut multi = layout(&gpu);
        let live = live_frame(&gpu, [50.0, 0.0, 100.0, 100.0], [0.0, 0.0, 200.0, 100.0]);
        multi.set_layout(
            Some("primary".into()),
            vec![MultiViewCell {
                path: "primary".into(),
                area: [75.0, 10.0, 60.0, 80.0],
            }],
        );
        assert_eq!(multi.texture_count(), 1, "cells no longer shown are freed");
        multi.clear();
        assert_eq!((multi.texture_count(), multi.texture_bytes()), (0, 0));
        let pixels = present(
            &gpu,
            &multi,
            Some(PrimaryDraw {
                bind_group: &live,
                before: &gpu.empty_before,
                clip: [0.0, 0.0, 200.0, 100.0],
            }),
        );
        assert!(close(pixel(&pixels, 100, 50), [0, 0, 255]));
        assert!(close(pixel(&pixels, 20, 50), [35, 35, 35]));
    }

    /// GPU time of the display pass for a 2880 × 1800 window (a 14" MacBook
    /// Pro at 2×) with eleven secondary cells of 1088 × 724 texels against
    /// the pass without them: the per-frame cost the multi view adds to the
    /// presentation (not to the primary's own render). Prints the medians.
    #[test]
    #[ignore = "requires a native GPU; timing probe"]
    fn multi_view_presentation_timing_probe() {
        let gpu = offscreen();
        let window = [2880u32, 1800u32];
        let target = gpu.device.create_texture(&wgpu::TextureDescriptor {
            label: None,
            size: wgpu::Extent3d {
                width: window[0],
                height: window[1],
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            view_formats: &[],
        });
        let view = target.create_view(&Default::default());
        let bindings = DisplayBindings {
            layout: &gpu.layout,
            sampler: &gpu.sampler,
            empty_overlay: &gpu.empty,
        };
        let (w, h) = (1088u32, 724u32);
        let pixels = solid(w, h, [120, 90, 60]);
        let mut multi = MultiView::default();
        let cells: Vec<MultiViewCell> = (0..12)
            .map(|i| MultiViewCell {
                path: format!("p{i}"),
                area: [
                    16.0 + (i % 4) as f32 * (w as f32 + 24.0),
                    16.0 + (i / 4) as f32 * (h as f32 + 60.0),
                    w as f32,
                    h as f32,
                ],
            })
            .collect();
        multi.set_layout(Some("p0".into()), cells);
        for i in 1..12 {
            multi.upload(
                &gpu.device,
                &gpu.queue,
                &bindings,
                &format!("p{i}"),
                1,
                whole(w, h),
                CellLayer::Whole,
                w,
                h,
                &pixels,
            );
        }
        let live = live_frame(
            &gpu,
            [16.0, 16.0, w as f32, h as f32],
            [16.0, 16.0, w as f32, h as f32],
        );
        let time = |with_cells: bool| {
            let mut samples = Vec::new();
            for _ in 0..60 {
                let start = std::time::Instant::now();
                multi.prepare(&gpu.queue, [window[0] as f32, window[1] as f32], BG);
                let mut encoder = gpu.device.create_command_encoder(&Default::default());
                encode_display_pass(
                    &mut encoder,
                    &view,
                    window,
                    DisplayPass {
                        pipeline: &gpu.pipeline,
                        empty_before: &gpu.empty_before,
                    },
                    BG,
                    with_cells.then_some(&multi),
                    Some(PrimaryDraw {
                        bind_group: &live,
                        before: &gpu.empty_before,
                        clip: [16.0, 16.0, w as f32, h as f32],
                    }),
                );
                gpu.queue.submit(Some(encoder.finish()));
                let _ = gpu.device.poll(wgpu::PollType::Wait {
                    submission_index: None,
                    timeout: None,
                });
                samples.push(start.elapsed().as_secs_f64() * 1e3);
            }
            samples.sort_by(f64::total_cmp);
            samples[samples.len() / 2]
        };
        let _ = time(true);
        let single = time(false);
        let multi_ms = time(true);
        println!(
            "display pass 2880x1800: primary only {single:.3} ms, primary + 11 cells {multi_ms:.3} ms (median of 60, submit to GPU idle); cell textures {:.1} MB",
            multi.texture_bytes() as f64 / 1e6
        );
    }

    #[test]
    #[ignore = "requires a native GPU; a still shown as the live frame is copied like a developed one"]
    fn a_still_frame_becomes_its_cells_stand_in_too() {
        let gpu = offscreen();
        let mut multi = layout(&gpu);
        let size = wgpu::Extent3d {
            width: 16,
            height: 16,
            depth_or_array_layers: 1,
        };
        let still = gpu
            .device
            .create_texture(&crate::gpu_processing::still_texture_descriptor(size));
        gpu.queue.write_texture(
            still.as_image_copy(),
            &solid(16, 16, [255, 0, 255]),
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(64),
                rows_per_image: Some(16),
            },
            size,
        );
        let bindings = DisplayBindings {
            layout: &gpu.layout,
            sampler: &gpu.sampler,
            empty_overlay: &gpu.empty,
        };
        let validation = gpu.device.push_error_scope(wgpu::ErrorFilter::Validation);
        multi.adopt_live_frame(
            &gpu.device,
            &gpu.queue,
            &bindings,
            "a",
            &still,
            (16, 16),
            None,
        );
        let error = pollster::block_on(validation.pop());
        assert!(error.is_none(), "{error:?}");
        let pixels = present(&gpu, &multi, None);
        assert!(close(pixel(&pixels, 35, 50), [255, 0, 255]));
    }

    #[test]
    #[ignore = "requires a native GPU; the old primary's live frame stays as its cell's frame"]
    fn a_live_frame_becomes_its_cells_stand_in() {
        let gpu = offscreen();
        let mut multi = layout(&gpu);
        // The display's output texture: copyable, filled with the frame.
        let source = {
            let texture = gpu.device.create_texture(&wgpu::TextureDescriptor {
                label: None,
                size: wgpu::Extent3d {
                    width: 16,
                    height: 16,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::Rgba8Unorm,
                usage: wgpu::TextureUsages::COPY_SRC | wgpu::TextureUsages::COPY_DST,
                view_formats: &[],
            });
            gpu.queue.write_texture(
                texture.as_image_copy(),
                &solid(16, 16, [255, 0, 255]),
                wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(64),
                    rows_per_image: Some(16),
                },
                wgpu::Extent3d {
                    width: 16,
                    height: 16,
                    depth_or_array_layers: 1,
                },
            );
            texture
        };
        let bindings = DisplayBindings {
            layout: &gpu.layout,
            sampler: &gpu.sampler,
            empty_overlay: &gpu.empty,
        };
        // Only a 12 × 8 corner holds the frame (a smaller preview).
        multi.adopt_live_frame(
            &gpu.device,
            &gpu.queue,
            &bindings,
            "a",
            &source,
            (12, 8),
            None,
        );
        assert_eq!(multi.texture_key("a"), Some(0), "waits for its own render");
        let pixels = present(&gpu, &multi, None);
        assert!(close(pixel(&pixels, 35, 50), [255, 0, 255]));
        // Not shown: nothing is kept.
        multi.adopt_live_frame(
            &gpu.device,
            &gpu.queue,
            &bindings,
            "gone",
            &source,
            (12, 8),
            None,
        );
        assert_eq!(multi.texture_key("gone"), None);
    }

    #[test]
    #[ignore = "requires a native GPU; a zoomed cell keeps its placed frame over the old live frame"]
    fn a_zoomed_cell_keeps_its_own_frame_over_the_old_live_frame() {
        let gpu = offscreen();
        let mut multi = layout(&gpu);
        multi.set_zoom(Some(CellZoom {
            scale: 4.0,
            center: [0.5, 0.5],
        }));
        let size = wgpu::Extent3d {
            width: 16,
            height: 16,
            depth_or_array_layers: 1,
        };
        let source = gpu
            .device
            .create_texture(&crate::gpu_processing::still_texture_descriptor(size));
        gpu.queue.write_texture(
            source.as_image_copy(),
            &solid(16, 16, [255, 0, 255]),
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(64),
                rows_per_image: Some(16),
            },
            size,
        );
        let bindings = DisplayBindings {
            layout: &gpu.layout,
            sampler: &gpu.sampler,
            empty_overlay: &gpu.empty,
        };
        // "a" has a sharp region (green) beside its whole frame (red).
        multi.upload(
            &gpu.device,
            &gpu.queue,
            &bindings,
            "a",
            1,
            CellExtent {
                shown: [30.0, 20.0],
                region: Some([10.0, 5.0, 10.0, 10.0]),
            },
            CellLayer::Region,
            10,
            10,
            &solid(10, 10, [0, 255, 0]),
        );
        assert!(multi.region_of("a").is_some());
        let pixels = present(&gpu, &multi, None);
        assert!(close(pixel(&pixels, 35, 50), [0, 255, 0]), "region drawn");
        // "a" has its own render placed for the zoom: it stays, red and
        // enlarged, not the live frame shown to fit; the region, which may
        // predate edits, is released.
        multi.adopt_live_frame(
            &gpu.device,
            &gpu.queue,
            &bindings,
            "a",
            &source,
            (16, 16),
            None,
        );
        assert_eq!(multi.texture_key("a"), Some(1));
        assert_eq!(multi.region_of("a"), None);
        let pixels = present(&gpu, &multi, None);
        assert!(close(pixel(&pixels, 35, 50), [255, 0, 0]));
        // Above the fitted image (letterbox), only the enlarged frame is red.
        assert!(close(pixel(&pixels, 35, 20), [255, 0, 0]));
        // A cell without a frame of its own still takes the live frame.
        multi.adopt_live_frame(
            &gpu.device,
            &gpu.queue,
            &bindings,
            "pending",
            &source,
            (16, 16),
            None,
        );
        assert_eq!(multi.texture_key("pending"), Some(0));
        // Back at fit the live frame (the newer pixels) replaces the old one.
        multi.set_zoom(None);
        multi.adopt_live_frame(
            &gpu.device,
            &gpu.queue,
            &bindings,
            "a",
            &source,
            (16, 16),
            None,
        );
        assert_eq!(multi.texture_key("a"), Some(0));
    }

    /// A 32 × 32 live frame: magenta with a cyan centre (texels 12–19).
    fn bullseye(gpu: &Offscreen) -> wgpu::Texture {
        let size = wgpu::Extent3d {
            width: 32,
            height: 32,
            depth_or_array_layers: 1,
        };
        let texture = gpu
            .device
            .create_texture(&crate::gpu_processing::still_texture_descriptor(size));
        let rgba: Vec<u8> = (0..32 * 32)
            .flat_map(|i| {
                let (x, y) = (i % 32, i / 32);
                if (12..20).contains(&x) && (12..20).contains(&y) {
                    [0, 255, 255, 255]
                } else {
                    [255, 0, 255, 255]
                }
            })
            .collect();
        gpu.queue.write_texture(
            texture.as_image_copy(),
            &rgba,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(128),
                rows_per_image: Some(32),
            },
            size,
        );
        texture
    }

    #[test]
    #[ignore = "requires a native GPU; the outgoing primary keeps its zoom and live frame in one step"]
    fn the_outgoing_primary_stays_zoomed_where_it_was() {
        let gpu = offscreen();
        let bindings = DisplayBindings {
            layout: &gpu.layout,
            sampler: &gpu.sampler,
            empty_overlay: &gpu.empty,
        };
        let source = bullseye(&gpu);
        // The primary's cell [75, 10, 60, 80] showed its 32 × 32 photograph
        // at 4 device px per pixel, centred: the frame covered 128 × 128 px
        // about the cell's centre (105, 50).
        let zoom = CellZoom {
            scale: 4.0,
            center: [0.5, 0.5],
        };
        let placed = [41.0, -14.0, 128.0, 128.0];
        for (label, zooms) in [
            (
                "lock on",
                CellZooms {
                    shared: Some(zoom),
                    cells: HashMap::new(),
                },
            ),
            (
                "lock off",
                CellZooms {
                    shared: None,
                    cells: HashMap::from([("primary".to_string(), zoom)]),
                },
            ),
        ] {
            let mut multi = layout(&gpu);
            let cells = multi.cells().to_vec();
            multi.show(
                &gpu.device,
                &gpu.queue,
                &bindings,
                Some("a".into()),
                cells,
                Some(zooms),
                Some(LiveFrame {
                    path: "primary",
                    texture: &source,
                    size: (32, 32),
                    placed: Some(placed),
                    // The editor's maximum zoom samples nearest-neighbour.
                    pixelated: label == "lock on",
                }),
            );
            assert_eq!(multi.primary(), Some("a"), "{label}");
            assert_eq!(multi.zoom_of("primary"), Some(zoom), "{label}");
            assert_eq!(
                multi.texture_key("primary"),
                Some(0),
                "{label}: waits for its own render"
            );
            // Its visible part stays as sharp as it was: the cell shows
            // texels 8.5–23.5 of 32 (60 × 80 px at 4 px each).
            let (key, extent) = multi.region_of("primary").expect(label);
            assert_eq!(key, 0, "{label}");
            assert_eq!(extent.shown, [32.0, 32.0], "{label}");
            assert_eq!(extent.region, Some([8.0, 6.0, 16.0, 20.0]), "{label}");
            assert!(
                multi.keeps_live_region("primary"),
                "{label}: no interim over it"
            );
            let pixelated = label == "lock on";
            assert_eq!(
                multi.pixelated("primary", &multi.regions["primary"]),
                pixelated,
                "{label}"
            );
            assert_eq!(
                multi.pixelated("primary", &multi.textures["primary"]),
                pixelated,
                "{label}"
            );
            let pixels = present(&gpu, &multi, None);
            // Enlarged as before the switch: the cyan centre spans 89–121
            // (at fit it would span 97.5–112.5 only, magenta around it).
            for (x, y) in [(93, 50), (117, 50), (105, 38), (105, 62)] {
                assert!(
                    close(pixel(&pixels, x, y), [0, 255, 255]),
                    "{label}: ({x}, {y}) {:?}",
                    pixel(&pixels, x, y)
                );
            }
            assert!(close(pixel(&pixels, 80, 50), [255, 0, 255]), "{label}");
            // At the edge of the cyan centre (texels 11 | 12, screen 85–89 |
            // 89–93): the pixel at 88.5 is texel 11 sampled nearest-neighbour
            // (magenta) as the live frame showed it at the maximum zoom, a
            // blend of both filtered.
            let edge = pixel(&pixels, 88, 50);
            assert_eq!(
                close(edge, [255, 0, 255]),
                pixelated,
                "{label}: edge {edge:?}"
            );
            if pixelated {
                // Zoomed to fit while its own render is pending: filtered
                // again (nearest-neighbour belongs to the maximum zoom).
                multi.set_zooms(None, HashMap::new());
                assert!(!multi.pixelated("primary", &multi.textures["primary"]));
                multi.set_zooms(Some(zoom), HashMap::new());
                assert!(multi.pixelated("primary", &multi.textures["primary"]));
                // The region went with the zoom change; adopt it again.
                multi.adopt_visible_region(
                    &gpu.device,
                    &gpu.queue,
                    &bindings,
                    "primary",
                    &source,
                    (32, 32),
                    zoom,
                    multi.textures["primary"].extent.unwrap(),
                );
            }
            assert!(
                close(pixel(&pixels, 105, 15), [255, 0, 255]),
                "{label}: no letterbox"
            );
            // The cell's own whole render (at the cell's size, here a flat
            // red) goes under the sharp region until its region is developed.
            multi.upload(
                &gpu.device,
                &gpu.queue,
                &bindings,
                "primary",
                9,
                whole(32, 32),
                CellLayer::Whole,
                32,
                32,
                &solid(32, 32, [255, 0, 0]),
            );
            let pixels = present(&gpu, &multi, None);
            assert!(
                close(pixel(&pixels, 93, 50), [0, 255, 255]),
                "{label}: region kept"
            );
        }
    }

    #[test]
    #[ignore = "requires a native GPU; without its zoom the old primary's frame is shown to fit"]
    fn an_outgoing_primary_at_fit_is_shown_to_fit() {
        let gpu = offscreen();
        let bindings = DisplayBindings {
            layout: &gpu.layout,
            sampler: &gpu.sampler,
            empty_overlay: &gpu.empty,
        };
        let source = bullseye(&gpu);
        let mut multi = layout(&gpu);
        let cells = multi.cells().to_vec();
        multi.show(
            &gpu.device,
            &gpu.queue,
            &bindings,
            Some("a".into()),
            cells,
            Some(CellZooms::default()),
            Some(LiveFrame {
                path: "primary",
                texture: &source,
                size: (32, 32),
                placed: Some([75.0, 20.0, 60.0, 60.0]),
                pixelated: false,
            }),
        );
        assert_eq!(multi.region_of("primary"), None);
        let pixels = present(&gpu, &multi, None);
        assert!(close(pixel(&pixels, 93, 50), [255, 0, 255]));
        assert!(close(pixel(&pixels, 105, 50), [0, 255, 255]));
    }

    #[test]
    fn a_live_frame_takes_its_extent_from_where_it_was_placed() {
        let zoom = CellZoom {
            scale: 2.0,
            center: [0.5, 0.5],
        };
        assert_eq!(
            live_extent([10.0, 20.0, 300.0, 200.0], zoom, (3000, 2000)),
            Some(CellExtent {
                shown: [150.0, 100.0],
                region: None
            })
        );
        // Hidden (the webview's off-screen placement), empty, or of another
        // aspect ratio: not a placement of this frame.
        assert_eq!(
            live_extent([-999999.0, -999999.0, 1.0, 1.0], zoom, (30, 20)),
            None
        );
        assert_eq!(live_extent([0.0, 0.0, 300.0, 0.0], zoom, (30, 20)), None);
        assert_eq!(live_extent([0.0, 0.0, 300.0, 300.0], zoom, (30, 20)), None);
        // Placed as the zoom puts it in its cell, or elsewhere (another
        // photograph's placement): then it is not taken.
        let area = [0.0, 0.0, 100.0, 100.0];
        assert!(placed_at_zoom(
            [-100.0, -50.0, 300.0, 200.0],
            area,
            zoom,
            (30, 20)
        ));
        assert!(!placed_at_zoom(
            [0.0, 0.0, 300.0, 200.0],
            area,
            zoom,
            (30, 20)
        ));
        assert_eq!(
            visible_texels(
                [0.0, 0.0, 60.0, 80.0],
                [32.0, 32.0],
                CellZoom {
                    scale: 4.0,
                    center: [0.5, 0.5]
                },
                (32, 32)
            ),
            Some([8, 6, 16, 20])
        );
        // Twice as many texels as pixels; clipped to the photograph.
        assert_eq!(
            visible_texels(
                [0.0, 0.0, 60.0, 80.0],
                [32.0, 32.0],
                CellZoom {
                    scale: 4.0,
                    center: [0.0, 0.5]
                },
                (64, 64)
            ),
            Some([0, 12, 15, 40])
        );
    }

    #[test]
    fn scissors_stay_inside_the_target() {
        assert_eq!(
            scissor([-5.0, 10.5, 30.0, 20.0], [100, 100]),
            Some([0, 10, 25, 21])
        );
        assert_eq!(scissor([120.0, 0.0, 10.0, 10.0], [100, 100]), None);
        assert_eq!(
            intersect([0.0, 0.0, 10.0, 10.0], [5.0, 5.0, 10.0, 10.0]),
            [5.0, 5.0, 5.0, 5.0]
        );
    }

    #[test]
    fn zoomed_cells_show_the_same_photograph_point() {
        // A 6000 × 4000 photograph at 1:1 (one device px per photo px),
        // centred on (0.5, 0.25), in a 400 × 300 cell at (100, 50).
        let area = [100.0, 50.0, 400.0, 300.0];
        let zoom = CellZoom {
            scale: 1.0,
            center: [0.5, 0.25],
        };
        let shown = [6000.0, 4000.0];
        let whole = zoomed_rect(
            area,
            CellExtent {
                shown,
                region: None,
            },
            zoom,
        );
        // The photo point (3000, 1000) lands on the cell's centre (300, 200).
        assert_eq!(whole, [300.0 - 3000.0, 200.0 - 1000.0, 6000.0, 4000.0]);
        // What the cell shows, and with a margin of half its size each side.
        assert_eq!(
            zoomed_region(area, shown, zoom, 0.0),
            [2800.0, 850.0, 400.0, 300.0]
        );
        let region = zoomed_region(area, shown, zoom, 0.5);
        assert_eq!(region, [2600.0, 700.0, 800.0, 600.0]);
        // The region texture sits exactly over its part of the enlarged photo.
        let placed = zoomed_rect(
            area,
            CellExtent {
                shown,
                region: Some(region),
            },
            zoom,
        );
        assert_eq!(placed, [-100.0, -100.0, 800.0, 600.0]);
        assert!(region_covers(region, area, shown, zoom));
        // Panned by less than the margin it still covers; further it does not.
        let panned = CellZoom {
            center: [0.5 + 150.0 / 6000.0, 0.25],
            ..zoom
        };
        assert!(region_covers(region, area, shown, panned));
        let far = CellZoom {
            center: [0.5 + 250.0 / 6000.0, 0.25],
            ..zoom
        };
        assert!(!region_covers(region, area, shown, far));
        // At 2:1 the cell shows half as many photo pixels.
        let double = CellZoom { scale: 2.0, ..zoom };
        assert_eq!(
            zoomed_region(area, shown, double, 0.0),
            [2900.0, 925.0, 200.0, 150.0]
        );
        // Near the photograph's edge the region stays inside it.
        let corner = CellZoom {
            scale: 1.0,
            center: [0.0, 1.0],
        };
        assert_eq!(
            zoomed_region(area, shown, corner, 0.5),
            [0.0, 3700.0, 400.0, 300.0]
        );
    }

    #[test]
    #[ignore = "requires a native GPU; zoomed cells: enlarged photograph and its sharp region"]
    fn zoomed_cells_draw_the_region_over_the_enlarged_photograph() {
        let gpu = offscreen();
        let bindings = DisplayBindings {
            layout: &gpu.layout,
            sampler: &gpu.sampler,
            empty_overlay: &gpu.empty,
        };
        // One secondary cell (0, 0, 200, 100); the photograph is 400 × 200.
        let mut multi = MultiView::default();
        multi.set_layout(
            Some("primary".into()),
            vec![MultiViewCell {
                path: "a".into(),
                area: [0.0, 0.0, 200.0, 100.0],
            }],
        );
        let shown = [400.0, 200.0];
        multi.upload(
            &gpu.device,
            &gpu.queue,
            &bindings,
            "a",
            1,
            CellExtent {
                shown,
                region: None,
            },
            CellLayer::Whole,
            20,
            10,
            &solid(20, 10, [255, 0, 0]),
        );
        // At fit the whole photograph fills the cell.
        let pixels = present(&gpu, &multi, None);
        assert!(close(pixel(&pixels, 10, 10), [255, 0, 0]));
        assert!(close(pixel(&pixels, 190, 90), [255, 0, 0]));
        // A region is ignored unless zoomed in.
        let region = [150.0, 50.0, 100.0, 100.0];
        let upload_region = |multi: &mut MultiView| {
            multi.upload(
                &gpu.device,
                &gpu.queue,
                &bindings,
                "a",
                2,
                CellExtent {
                    shown,
                    region: Some(region),
                },
                CellLayer::Region,
                100,
                100,
                &solid(100, 100, [0, 255, 0]),
            );
        };
        upload_region(&mut multi);
        assert_eq!(multi.region_of("a"), None);
        // 1:1 centred on the photograph's centre (200, 100): the cell shows
        // photo x 100..300, y 50..150; the region (150..250, 50..150) sits
        // at cell x 50..150, the enlarged whole photograph around it.
        multi.set_zoom(Some(CellZoom {
            scale: 1.0,
            center: [0.5, 0.5],
        }));
        upload_region(&mut multi);
        assert_eq!(multi.region_of("a").map(|(key, _)| key), Some(2));
        let pixels = present(&gpu, &multi, None);
        assert!(close(pixel(&pixels, 100, 50), [0, 255, 0]), "region");
        assert!(close(pixel(&pixels, 20, 50), [255, 0, 0]), "enlarged photo");
        assert!(
            close(pixel(&pixels, 180, 50), [255, 0, 0]),
            "enlarged photo"
        );
        // Panning moves the region along; another magnification drops it.
        multi.set_zoom(Some(CellZoom {
            scale: 1.0,
            center: [0.55, 0.5],
        }));
        assert!(multi.region_of("a").is_some());
        let pixels = present(&gpu, &multi, None);
        assert!(
            close(pixel(&pixels, 40, 50), [0, 255, 0]),
            "moved with the pan"
        );
        multi.set_zoom(Some(CellZoom {
            scale: 2.0,
            center: [0.5, 0.5],
        }));
        assert_eq!(multi.region_of("a"), None);
        multi.set_zoom(None);
        let pixels = present(&gpu, &multi, None);
        assert!(close(pixel(&pixels, 100, 50), [255, 0, 0]), "back to fit");
    }

    #[test]
    fn a_cell_without_its_own_zoom_follows_the_shared_one() {
        let mut multi = MultiView::default();
        let cell = |path: &str| MultiViewCell {
            path: path.into(),
            area: [0.0, 0.0, 10.0, 10.0],
        };
        multi.set_layout(Some("p".into()), vec![cell("p"), cell("a"), cell("b")]);
        let one = CellZoom {
            scale: 1.0,
            center: [0.5, 0.5],
        };
        assert!(!multi.is_zoomed());
        multi.set_zooms(None, HashMap::from([("a".to_string(), one)]));
        assert_eq!(multi.zoom_of("a"), Some(one));
        assert_eq!(multi.zoom_of("b"), None);
        assert!(multi.is_zoomed());
        multi.set_zoom(Some(one));
        assert_eq!(multi.zoom_of("b"), Some(one));
        assert!(multi.cell_zooms().is_empty());
        // A cell that leaves the layout loses its zoom.
        multi.set_zooms(None, HashMap::from([("a".to_string(), one)]));
        multi.set_layout(Some("p".into()), vec![cell("p"), cell("b")]);
        assert!(!multi.is_zoomed());
    }

    /// A new device's display gets the lost one's zooms before its layout
    /// (`multi_view::show_again` in the app, WDVL3): the new multi view
    /// keeps them when the layout comes, and when the webview sends the
    /// same layout again.
    #[test]
    fn zooms_set_before_the_layout_stay() {
        let cell = |path: &str| MultiViewCell {
            path: path.into(),
            area: [0.0, 0.0, 10.0, 10.0],
        };
        let one = CellZoom {
            scale: 1.0,
            center: [0.25, 0.75],
        };
        let mut multi = MultiView::default();
        multi.set_zooms(None, HashMap::from([("a".to_string(), one)]));
        assert!(!multi.is_active());
        let layout = vec![cell("p"), cell("a"), cell("b")];
        multi.set_layout(Some("p".into()), layout.clone());
        assert_eq!(multi.zoom_of("a"), Some(one));
        assert_eq!(multi.zoom_of("b"), None);
        multi.set_layout(Some("p".into()), layout);
        assert_eq!(multi.zoom_of("a"), Some(one));
        // The zoom lock's shared zoom too.
        let mut locked = MultiView::default();
        locked.set_zoom(Some(one));
        locked.set_layout(Some("p".into()), vec![cell("p"), cell("b")]);
        assert_eq!(locked.zoom_of("b"), Some(one));
    }

    #[test]
    #[ignore = "requires a native GPU; each cell zoomed on its own without the zoom lock"]
    fn individual_zooms_zoom_only_their_cell() {
        let gpu = offscreen();
        let bindings = DisplayBindings {
            layout: &gpu.layout,
            sampler: &gpu.sampler,
            empty_overlay: &gpu.empty,
        };
        // Two secondary cells of 100 × 50 side by side; both photographs
        // are 400 × 200. At 1:1 on the centre a cell shows photo x 150..250,
        // y 75..125, all inside the region (150..250, 50..150).
        let mut multi = MultiView::default();
        multi.set_layout(
            Some("primary".into()),
            vec![
                MultiViewCell {
                    path: "a".into(),
                    area: [0.0, 0.0, 100.0, 50.0],
                },
                MultiViewCell {
                    path: "b".into(),
                    area: [100.0, 0.0, 100.0, 50.0],
                },
            ],
        );
        let shown = [400.0, 200.0];
        for (path, colour) in [("a", [255, 0, 0]), ("b", [0, 0, 255])] {
            multi.upload(
                &gpu.device,
                &gpu.queue,
                &bindings,
                path,
                1,
                CellExtent {
                    shown,
                    region: None,
                },
                CellLayer::Whole,
                20,
                10,
                &solid(20, 10, colour),
            );
        }
        let region = |multi: &mut MultiView, path: &str| {
            multi.upload(
                &gpu.device,
                &gpu.queue,
                &bindings,
                path,
                2,
                CellExtent {
                    shown,
                    region: Some([150.0, 50.0, 100.0, 100.0]),
                },
                CellLayer::Region,
                100,
                100,
                &solid(100, 100, [0, 255, 0]),
            );
        };
        let one = CellZoom {
            scale: 1.0,
            center: [0.5, 0.5],
        };
        // Only "a" is zoomed: its region shows, "b" stays at fit and takes
        // no region.
        multi.set_zooms(None, HashMap::from([("a".to_string(), one)]));
        region(&mut multi, "a");
        region(&mut multi, "b");
        assert!(multi.region_of("a").is_some());
        assert_eq!(multi.region_of("b"), None);
        let pixels = present(&gpu, &multi, None);
        assert!(close(pixel(&pixels, 50, 25), [0, 255, 0]), "a's region");
        assert!(close(pixel(&pixels, 105, 5), [0, 0, 255]), "b at fit");
        assert!(close(pixel(&pixels, 195, 45), [0, 0, 255]), "b at fit");
        // "a" pans on its own: the region moves along, "b" does not move.
        multi.set_zooms(
            None,
            HashMap::from([(
                "a".to_string(),
                CellZoom {
                    center: [0.55, 0.5],
                    ..one
                },
            )]),
        );
        assert!(multi.region_of("a").is_some(), "a pan keeps the region");
        let pixels = present(&gpu, &multi, None);
        assert!(
            close(pixel(&pixels, 40, 25), [0, 255, 0]),
            "moved with a's pan"
        );
        assert!(close(pixel(&pixels, 90, 25), [255, 0, 0]), "a enlarged");
        assert!(close(pixel(&pixels, 150, 25), [0, 0, 255]), "b unchanged");
        // The lock on: both cells at the shared zoom; "a" keeps its region at
        // the same magnification, "b" takes one now.
        multi.set_zoom(Some(one));
        assert!(multi.region_of("a").is_some());
        region(&mut multi, "b");
        assert!(multi.region_of("b").is_some());
        let pixels = present(&gpu, &multi, None);
        assert!(close(pixel(&pixels, 150, 25), [0, 255, 0]), "b's region");
        // Each to fit again.
        multi.set_zooms(None, HashMap::new());
        assert_eq!(multi.region_of("a"), None);
        assert_eq!(multi.region_of("b"), None);
        let pixels = present(&gpu, &multi, None);
        assert!(close(pixel(&pixels, 50, 25), [255, 0, 0]));
        assert!(close(pixel(&pixels, 150, 25), [0, 0, 255]));
    }
}
