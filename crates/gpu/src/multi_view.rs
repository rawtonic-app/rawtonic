//! Multi view on the native display: several photographs side by side in
//! the viewer (Capture One's Multi view, Lightroom's Survey). The primary
//! photograph is the live preview frame (`WgpuDisplay::current_bind_group`);
//! every other cell draws its own developed texture, rendered once at the
//! cell's size by the app with the same GPU pipeline and kept here until its
//! recipe or size changes. Zoomed in (Lightroom's locked zoom) every cell
//! shows the same photograph point at the same magnification: the cell's
//! whole-photograph texture enlarged at once, then the visible region
//! developed at that magnification on top. Develop pixels never leave the
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

/// A secondary photograph's developed frame (display-encoded RGBA8).
struct CellTexture {
    /// What it was rendered from (recipe and size); 0 for a stand-in that
    /// waits for its own render (see [`MultiView::adopt_live_frame`]).
    key: u64,
    /// Unknown for a stand-in: then it is only shown to fit.
    extent: Option<CellExtent>,
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
    zoom: Option<CellZoom>,
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

    pub fn zoom(&self) -> Option<CellZoom> {
        self.zoom
    }

    /// Shows `cells` with `primary` as the live photograph. Textures of
    /// photographs no longer shown are released.
    pub fn set_layout(&mut self, primary: Option<String>, cells: Vec<MultiViewCell>) {
        let shown = |path: &String| cells.iter().any(|cell| &cell.path == path);
        self.textures.retain(|path, _| shown(path));
        self.regions.retain(|path, _| shown(path));
        self.primary = primary;
        self.cells = cells;
    }

    /// Zooms every cell to `zoom` (None: fit). Regions developed for another
    /// magnification are released; panning keeps them (they move along).
    pub fn set_zoom(&mut self, zoom: Option<CellZoom>) {
        let same_scale = match (self.zoom, zoom) {
            (Some(a), Some(b)) => (a.scale - b.scale).abs() <= a.scale * 1e-3,
            _ => false,
        };
        if !same_scale {
            self.regions.clear();
        }
        self.zoom = zoom;
    }

    /// Back to the single view: every secondary texture is released.
    pub fn clear(&mut self) {
        self.cells.clear();
        self.primary = None;
        self.textures.clear();
        self.regions.clear();
        self.zoom = None;
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
            || (layer == CellLayer::Region && self.zoom.is_none())
        {
            return;
        }
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
        let texture = Self::make_texture(device, bindings, key.max(1), Some(extent), texture);
        match layer {
            CellLayer::Whole => self.textures.insert(path.to_string(), texture),
            CellLayer::Region => self.regions.insert(path.to_string(), texture),
        };
    }

    /// The live frame of `path` (the primary's preview, `width` × `height`
    /// of `source`) stays on screen as its cell's frame when another
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
        width: u32,
        height: u32,
    ) {
        if self.cell(path).is_none()
            || width == 0
            || height == 0
            || width > source.width()
            || height > source.height()
        {
            return;
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
        queue.submit(Some(encoder.finish()));
        // The live frame's region of the photograph is not known here: it
        // is shown to fit until the cell's own render arrives.
        let texture = Self::make_texture(device, bindings, 0, None, texture);
        self.textures.insert(path.to_string(), texture);
        self.regions.remove(path);
    }

    fn placement(&self, area: [f32; 4], texture: &CellTexture) -> [f32; 4] {
        match (self.zoom, texture.extent) {
            (Some(zoom), Some(extent)) => zoomed_rect(area, extent, zoom),
            _ => fit_rect(area, texture.width as f32, texture.height as f32),
        }
    }

    /// The screen area a texture covers in `area`: the whole cell for the
    /// whole photograph (around it the viewer's background), only its own
    /// rectangle for a zoomed region (drawn over the enlarged photograph).
    fn clip(&self, area: [f32; 4], texture: &CellTexture, layer: CellLayer) -> [f32; 4] {
        match layer {
            CellLayer::Whole => area,
            CellLayer::Region => intersect(area, self.placement(area, texture)),
        }
    }

    /// The textures drawn in a cell, in order: the whole photograph, then
    /// (zoomed in) its sharp region.
    fn layers(&self, path: &str) -> impl Iterator<Item = (&CellTexture, CellLayer)> {
        let region = self.zoom.and(self.regions.get(path));
        self.textures
            .get(path)
            .map(|t| (t, CellLayer::Whole))
            .into_iter()
            .chain(region.map(|t| (t, CellLayer::Region)))
    }

    /// Writes each cell's placement for a `window` (device px) frame.
    pub(crate) fn prepare(&self, queue: &wgpu::Queue, window: [f32; 2], background: [f32; 4]) {
        for cell in &self.cells {
            for (texture, layer) in self.layers(&cell.path) {
                let size = [texture.width as f32, texture.height as f32];
                let transform = DisplayTransform {
                    rect: self.placement(cell.area, texture),
                    clip: self.clip(cell.area, texture, layer),
                    window,
                    image_size: size,
                    texture_size: size,
                    pixelated: 0.0,
                    rotation: 0.0,
                    bg_primary: background,
                    bg_secondary: background,
                    overlay_color: [0.0; 4],
                    overlay_mode: [0.0; 4],
                    box_rect: [0.0; 4],
                };
                queue.write_buffer(&texture.uniforms, 0, bytemuck::bytes_of(&transform));
            }
        }
    }
}
/// The live primary frame for [`encode_display_pass`]: its bind group and
/// the screen area it may cover.
pub(crate) struct PrimaryDraw<'a> {
    pub bind_group: &'a wgpu::BindGroup,
    pub clip: [f32; 4],
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
    pipeline: &wgpu::RenderPipeline,
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
    rpass.set_pipeline(pipeline);
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
                let Some([x, y, w, h]) = scissor(multi.clip(cell.area, texture, layer), size)
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
        let (pipeline, layout) =
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
        Offscreen {
            device,
            queue,
            pipeline,
            layout,
            sampler,
            empty,
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
            &gpu.pipeline,
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
                    &gpu.pipeline,
                    BG,
                    with_cells.then_some(&multi),
                    Some(PrimaryDraw {
                        bind_group: &live,
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
        multi.adopt_live_frame(&gpu.device, &gpu.queue, &bindings, "a", &source, 12, 8);
        assert_eq!(multi.texture_key("a"), Some(0), "waits for its own render");
        let pixels = present(&gpu, &multi, None);
        assert!(close(pixel(&pixels, 35, 50), [255, 0, 255]));
        // Not shown: nothing is kept.
        multi.adopt_live_frame(&gpu.device, &gpu.queue, &bindings, "gone", &source, 12, 8);
        assert_eq!(multi.texture_key("gone"), None);
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
}
