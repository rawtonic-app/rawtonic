//! The camera preview on the native display: the camera's own JPEG of the
//! photograph (display-encoded, turned and framed as its still,
//! `embedded_still`) drawn in place of the live frame, to fit the viewer
//! whatever the edit's zoom. A view only: developed frames keep arriving and
//! stay bound underneath, so taking the cover away shows the edit at once,
//! without a render. Never drawn in the multi view; asking for another
//! photograph takes it away (`WgpuDisplay::release_still_for`).

use super::multi_view::{CellTexture, DisplayBindings, MultiView};
use super::{DisplayTransform, fit_placement};

pub struct CameraCover {
    path: String,
    /// The size the still stands for (its developed frame's shape).
    shown: (u32, u32),
    texture: CellTexture,
}

impl CameraCover {
    /// `image` (display-encoded RGBA) as the camera preview of `path`,
    /// placed as a frame of `shown` would be; none for an empty image.
    pub(crate) fn new(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        bindings: &DisplayBindings,
        path: &str,
        image: &image::RgbaImage,
        shown: (u32, u32),
    ) -> Option<Self> {
        let (width, height) = image.dimensions();
        if width == 0 || height == 0 || shown.0 == 0 || shown.1 == 0 {
            return None;
        }
        let texture =
            MultiView::uploaded_texture(device, queue, bindings, width, height, image.as_raw());
        Some(Self {
            path: path.to_string(),
            shown,
            texture,
        })
    }

    /// The photograph it shows.
    pub fn path(&self) -> &str {
        &self.path
    }

    pub(crate) fn bind_group(&self) -> &wgpu::BindGroup {
        self.texture.bind_group()
    }

    /// Writes its placement (`cover_transform` in `area`) for the draw.
    pub(crate) fn prepare(&self, queue: &wgpu::Queue, base: &DisplayTransform, area: [f32; 4]) {
        let (width, height) = self.texture.size();
        let transform = cover_transform(base, area, self.shown, (width, height));
        queue.write_buffer(self.texture.uniforms(), 0, bytemuck::bytes_of(&transform));
    }
}

/// The camera preview's uniforms: the window, clip and background of the
/// display (`base`), the still contained and centred in the viewer's `area`
/// (device pixels) as a frame of `shown` would be, unrotated, without the
/// mask overlay, the split or the zoom's pixelation.
pub fn cover_transform(
    base: &DisplayTransform,
    area: [f32; 4],
    shown: (u32, u32),
    texture: (u32, u32),
) -> DisplayTransform {
    let texture = [texture.0 as f32, texture.1 as f32];
    DisplayTransform {
        rect: fit_placement(area, shown.0 as f32, shown.1 as f32),
        image_size: texture,
        texture_size: texture,
        pixelated: 0.0,
        rotation: 0.0,
        overlay_mode: [0.0; 4],
        box_rect: [0.0; 4],
        split: [0.0; 4],
        ..*base
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base() -> DisplayTransform {
        DisplayTransform {
            rect: [-400.0, -300.0, 4000.0, 3000.0],
            clip: [10.0, 20.0, 800.0, 600.0],
            window: [1000.0, 700.0],
            image_size: [6000.0, 4000.0],
            texture_size: [2048.0, 1365.0],
            pixelated: 1.0,
            rotation: 0.2,
            bg_primary: [0.1, 0.1, 0.1, 1.0],
            bg_secondary: [0.2, 0.2, 0.2, 1.0],
            overlay_color: [1.0, 0.0, 0.0, 0.5],
            overlay_mode: [1.0, 0.0, 0.0, 0.0],
            box_rect: [1.0, 2.0, 3.0, 4.0],
            split: [1.0, 300.0, 0.0, 0.0],
        }
    }

    #[test]
    fn the_camera_preview_fits_the_viewer_whatever_the_edit_zoom() {
        // The edit is zoomed in (rect far beyond the viewer), rotated, with a
        // mask overlay and a split: the camera JPEG is shown whole, to fit.
        let t = cover_transform(
            &base(),
            [12.0, 22.0, 796.0, 596.0],
            (6000, 4000),
            (1500, 1000),
        );
        let fit = fit_placement([12.0, 22.0, 796.0, 596.0], 6000.0, 4000.0);
        assert_eq!(t.rect, fit);
        assert_eq!(t.image_size, [1500.0, 1000.0]);
        assert_eq!(t.texture_size, [1500.0, 1000.0]);
        assert_eq!((t.pixelated, t.rotation), (0.0, 0.0));
        assert_eq!(t.overlay_mode, [0.0; 4]);
        assert_eq!(t.split, [0.0; 4]);
        assert_eq!(t.box_rect, [0.0; 4]);
        // The display's own window, clip and background stay.
        assert_eq!(
            (t.clip, t.window, t.bg_primary),
            (base().clip, base().window, base().bg_primary)
        );
    }

    #[test]
    fn a_portrait_preview_is_placed_upright_and_centred() {
        let t = cover_transform(
            &base(),
            [0.0, 0.0, 900.0, 600.0],
            (4000, 6000),
            (1000, 1500),
        );
        assert_eq!(t.rect, [250.0, 0.0, 400.0, 600.0]);
    }
}
