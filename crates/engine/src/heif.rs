//! HEIC/HEIF sources: HEVC still images in a HEIF container (ISO/IEC
//! 23008-12), as iPhones write them (`.heic`) and Canon, Sony and Fujifilm
//! cameras (`.hif`).
//!
//! HEVC is decoded by the operating system's codec, never one bundled with
//! the app (HEVC is patent-licensed; the OS vendor licenses its decoder):
//! on macOS through ImageIO (`imageio`). Other systems report
//! `UNSUPPORTED` when such a file is opened (Windows' HEIF extension and
//! Linux are open follow-ups, notes: parity-gaps/G41-ingest.md).
//!
//! ImageIO hands over the coded pixels (10-bit files at 16 bits, not
//! clipped to 8) without orientation or colour conversion; both follow the
//! container as for AVIF (avif.rs): the primary item's `irot`/`imir`
//! (`avif::orientation`; HEIF readers apply these, not the EXIF tag) and its
//! `colr` profile or code points through `avif::to_working_space` (Display
//! P3, PQ/HLG as AVIF's), else the colour space ImageIO decodes to. Gain
//! maps (Apple's and ISO 21496-1) are not applied: the base (SDR) image
//! shows, as for AVIF.

use anyhow::Result;
use image::DynamicImage;

/// Why a HEIC/HEIF file does not open here.
pub const UNSUPPORTED: &str =
    "HEIC/HEIF photos can't be opened on this system yet (Rawtonic opens them on macOS)";

/// Major/compatible brands of HEVC (and other non-AV1) HEIF still images.
const BRANDS: &[&[u8; 4]] = &[
    b"heic", b"heix", b"heim", b"heis", b"hevc", b"hevx", b"mif1", b"msf1",
];

/// Whether `bytes` are a HEIF file that is not AVIF (avif.rs reads those):
/// an ISO BMFF file type box naming a HEIC/HEIF brand.
pub fn is_heif(bytes: &[u8]) -> bool {
    if bytes.get(4..8) != Some(b"ftyp") || crate::avif::is_avif(bytes) {
        return false;
    }
    let Some(size) = bytes
        .get(0..4)
        .map(|s| u32::from_be_bytes(s.try_into().unwrap()) as usize)
    else {
        return false;
    };
    let end = size.min(bytes.len());
    // Major brand at 8, minor version at 12, compatible brands from 16.
    bytes
        .get(8..12)
        .into_iter()
        .chain(bytes.get(16..end).unwrap_or(&[]).as_chunks::<4>().0.iter().map(|b| &b[..]))
        .any(|brand| BRANDS.iter().any(|known| &known[..] == brand))
}

/// The primary image as sRGB-encoded values (input_profile.rs), not yet
/// oriented (`crate::avif::orientation` gives the container's orientation).
pub fn decode(bytes: &[u8]) -> Result<DynamicImage> {
    #[cfg(target_os = "macos")]
    {
        let decoded = imageio::decode(bytes)?;
        Ok(to_working_space(bytes, decoded))
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = bytes;
        anyhow::bail!(UNSUPPORTED)
    }
}

/// A small preview, oriented and sRGB-encoded, at least `size / 2` pixels
/// on its short side (the file's embedded thumbnail when it is large
/// enough, else the image scaled to `size` on its long side); `None` where
/// the system cannot decode HEIF.
pub fn preview(bytes: &[u8], size: u32) -> Option<DynamicImage> {
    #[cfg(target_os = "macos")]
    {
        let decoded = imageio::preview(bytes, size)?;
        Some(match decoded.pixels {
            imageio::Pixels::Encoded { image, icc } => match icc {
                Some(icc) => crate::input_profile::to_working_space(image, &icc),
                None => image,
            },
            imageio::Pixels::Working(image) => image,
        })
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (bytes, size);
        None
    }
}

/// The decode in the working space: the container's colour as AVIF's, else
/// the profile of the colour space ImageIO decoded to.
#[cfg(target_os = "macos")]
fn to_working_space(bytes: &[u8], decoded: imageio::Decoded) -> DynamicImage {
    let (image, icc) = match decoded.pixels {
        imageio::Pixels::Working(image) => return image,
        imageio::Pixels::Encoded { image, icc } => (image, icc),
    };
    let (container_icc, nclx) = crate::avif::primary_colour(bytes);
    if container_icc.is_none() && nclx.is_none() {
        return match icc {
            Some(icc) => crate::input_profile::to_working_space(image, &icc),
            None => image,
        };
    }
    crate::avif::to_working_space(crate::avif::AvifImage {
        image,
        icc: container_icc,
        // Without `nclx` an ICC profile applies; the code points are then unused.
        cicp: nclx.unwrap_or(crate::avif::Cicp {
            primaries: 2,
            transfer: 2,
            matrix: 2,
            full_range: true,
        }),
        orientation: 1,
    })
}

#[cfg(target_os = "macos")]
pub(crate) mod imageio {
    //! The macOS ImageIO / Core Graphics calls (C API, linked from the
    //! system frameworks).

    use std::ffi::c_void;
    use std::ptr::{NonNull, null, null_mut};

    use anyhow::{Result, anyhow, bail};
    use image::{DynamicImage, ImageBuffer};

    type CFTypeRef = *const c_void;

    #[repr(C)]
    #[derive(Clone, Copy)]
    struct CGRect {
        x: f64,
        y: f64,
        width: f64,
        height: f64,
    }

    // CFDictionary callbacks are structs; only their addresses are passed.
    #[repr(C)]
    struct Callbacks([usize; 6]);

    #[link(name = "CoreFoundation", kind = "framework")]
    unsafe extern "C" {
        static kCFTypeDictionaryKeyCallBacks: Callbacks;
        static kCFTypeDictionaryValueCallBacks: Callbacks;
        static kCFBooleanTrue: CFTypeRef;
        static kCFBooleanFalse: CFTypeRef;
        fn CFRelease(cf: CFTypeRef);
        fn CFDataCreate(allocator: CFTypeRef, bytes: *const u8, length: isize) -> CFTypeRef;
        fn CFDataGetLength(data: CFTypeRef) -> isize;
        fn CFDataGetBytePtr(data: CFTypeRef) -> *const u8;
        fn CFNumberCreate(allocator: CFTypeRef, kind: isize, value: *const c_void) -> CFTypeRef;
        fn CFNumberGetValue(number: CFTypeRef, kind: isize, value: *mut c_void) -> bool;
        fn CFDictionaryCreate(
            allocator: CFTypeRef,
            keys: *const CFTypeRef,
            values: *const CFTypeRef,
            count: isize,
            key_callbacks: *const Callbacks,
            value_callbacks: *const Callbacks,
        ) -> CFTypeRef;
        fn CFDictionaryGetValue(dictionary: CFTypeRef, key: CFTypeRef) -> CFTypeRef;
    }

    #[link(name = "ImageIO", kind = "framework")]
    unsafe extern "C" {
        static kCGImageSourceShouldCache: CFTypeRef;
        static kCGImageSourceCreateThumbnailFromImageIfAbsent: CFTypeRef;
        static kCGImageSourceCreateThumbnailFromImageAlways: CFTypeRef;
        static kCGImageSourceCreateThumbnailWithTransform: CFTypeRef;
        static kCGImageSourceThumbnailMaxPixelSize: CFTypeRef;
        static kCGImagePropertyOrientation: CFTypeRef;
        fn CGImageSourceCreateWithData(data: CFTypeRef, options: CFTypeRef) -> CFTypeRef;
        fn CGImageSourceGetCount(source: CFTypeRef) -> usize;
        fn CGImageSourceGetPrimaryImageIndex(source: CFTypeRef) -> usize;
        fn CGImageSourceCreateImageAtIndex(
            source: CFTypeRef,
            index: usize,
            options: CFTypeRef,
        ) -> CFTypeRef;
        fn CGImageSourceCreateThumbnailAtIndex(
            source: CFTypeRef,
            index: usize,
            options: CFTypeRef,
        ) -> CFTypeRef;
        fn CGImageSourceCopyPropertiesAtIndex(
            source: CFTypeRef,
            index: usize,
            options: CFTypeRef,
        ) -> CFTypeRef;
    }

    #[link(name = "CoreGraphics", kind = "framework")]
    unsafe extern "C" {
        static kCGColorSpaceExtendedLinearSRGB: CFTypeRef;
        fn CGImageGetWidth(image: CFTypeRef) -> usize;
        fn CGImageGetHeight(image: CFTypeRef) -> usize;
        fn CGImageGetColorSpace(image: CFTypeRef) -> CFTypeRef;
        fn CGColorSpaceGetModel(space: CFTypeRef) -> i32;
        fn CGColorSpaceCopyICCData(space: CFTypeRef) -> CFTypeRef;
        fn CGColorSpaceCreateWithName(name: CFTypeRef) -> CFTypeRef;
        fn CGBitmapContextCreate(
            data: *mut c_void,
            width: usize,
            height: usize,
            bits_per_component: usize,
            bytes_per_row: usize,
            space: CFTypeRef,
            bitmap_info: u32,
        ) -> CFTypeRef;
        fn CGContextSetBlendMode(context: CFTypeRef, mode: i32);
        fn CGContextDrawImage(context: CFTypeRef, rect: CGRect, image: CFTypeRef);
    }

    const NUMBER_SINT64: isize = 4;
    const MODEL_RGB: i32 = 1;
    const BLEND_COPY: i32 = 17;
    const ALPHA_PREMULTIPLIED_LAST: u32 = 1;
    const FLOAT_COMPONENTS: u32 = 1 << 8;
    const BYTE_ORDER_16_LITTLE: u32 = 4 << 12;
    const BYTE_ORDER_32_LITTLE: u32 = 2 << 12;

    /// An owned Core Foundation object (Create/Copy rule), released on drop.
    pub(crate) struct Cf(NonNull<c_void>);

    impl Cf {
        fn new(object: CFTypeRef) -> Option<Self> {
            NonNull::new(object.cast_mut()).map(Self)
        }

        fn get(&self) -> CFTypeRef {
            self.0.as_ptr()
        }
    }

    impl Drop for Cf {
        fn drop(&mut self) {
            // SAFETY: owned by this wrapper (Create/Copy rule), released once.
            unsafe { CFRelease(self.0.as_ptr()) };
        }
    }

    /// A CFDictionary of `entries` (the keys are ImageIO's constants).
    fn dictionary(entries: &[(CFTypeRef, CFTypeRef)]) -> Option<Cf> {
        let keys: Vec<_> = entries.iter().map(|e| e.0).collect();
        let values: Vec<_> = entries.iter().map(|e| e.1).collect();
        // SAFETY: valid key/value arrays of `entries.len()` CF objects; the
        // dictionary retains them.
        Cf::new(unsafe {
            CFDictionaryCreate(
                null(),
                keys.as_ptr(),
                values.as_ptr(),
                entries.len() as isize,
                &raw const kCFTypeDictionaryKeyCallBacks,
                &raw const kCFTypeDictionaryValueCallBacks,
            )
        })
    }

    fn number(value: i64) -> Option<Cf> {
        // SAFETY: reads one i64.
        Cf::new(unsafe { CFNumberCreate(null(), NUMBER_SINT64, (&raw const value).cast()) })
    }

    /// An image source over a copy of `bytes` (ImageIO may read it lazily
    /// while the source lives), without ImageIO's decoded-image cache.
    pub(crate) fn source(bytes: &[u8]) -> Result<(Cf, usize)> {
        let length = isize::try_from(bytes.len())?;
        // SAFETY: CFDataCreate copies `length` bytes from the slice.
        let data = Cf::new(unsafe { CFDataCreate(null(), bytes.as_ptr(), length) })
            .ok_or_else(|| anyhow!("Out of memory reading the HEIF file"))?;
        // SAFETY: ImageIO constants and a CFBoolean.
        let options = dictionary(&[unsafe { (kCGImageSourceShouldCache, kCFBooleanFalse) }]);
        let options = options.as_ref().map_or(null(), Cf::get);
        // SAFETY: valid CFData and options dictionary (or NULL).
        let source = Cf::new(unsafe { CGImageSourceCreateWithData(data.get(), options) })
            .ok_or_else(|| anyhow!("The HEIF file could not be read"))?;
        // SAFETY: a valid image source.
        if unsafe { CGImageSourceGetCount(source.get()) } == 0 {
            bail!("The HEIF file holds no image the system can decode");
        }
        // SAFETY: a valid image source.
        let primary = unsafe { CGImageSourceGetPrimaryImageIndex(source.get()) };
        Ok((source, primary))
    }

    /// Decoded pixels: as coded in the colour space ImageIO decoded to
    /// (with that space's ICC profile), or converted by Core Graphics into
    /// the working space where that space is not RGB or has no profile.
    pub(crate) enum Pixels {
        Encoded {
            image: DynamicImage,
            icc: Option<Vec<u8>>,
        },
        Working(DynamicImage),
    }

    pub(crate) struct Decoded {
        pub pixels: Pixels,
    }

    /// The primary image at its full size, as coded (not oriented).
    pub(crate) fn decode(bytes: &[u8]) -> Result<Decoded> {
        let (source, index) = source(bytes)?;
        // SAFETY: a valid image source and index.
        let image = Cf::new(unsafe { CGImageSourceCreateImageAtIndex(source.get(), index, null()) })
            .ok_or_else(|| anyhow!("The system could not decode this HEIF file"))?;
        Ok(Decoded {
            pixels: pixels(&image)?,
        })
    }

    /// `heif::preview`'s image: the embedded thumbnail when its short side
    /// is at least `size / 2`, else the image scaled to `size`; oriented.
    pub(crate) fn preview(bytes: &[u8], size: u32) -> Option<Decoded> {
        let (source, index) = source(bytes).ok()?;
        let max = number(i64::from(size.max(1)))?;
        let thumbnail = |always: bool| {
            // SAFETY: ImageIO constants and CF values that outlive the call.
            let options = dictionary(&unsafe {
                [
                    (
                        if always {
                            kCGImageSourceCreateThumbnailFromImageAlways
                        } else {
                            kCGImageSourceCreateThumbnailFromImageIfAbsent
                        },
                        kCFBooleanTrue,
                    ),
                    (kCGImageSourceCreateThumbnailWithTransform, kCFBooleanTrue),
                    (kCGImageSourceThumbnailMaxPixelSize, max.get()),
                ]
            })?;
            // SAFETY: a valid image source, index and options dictionary.
            Cf::new(unsafe {
                CGImageSourceCreateThumbnailAtIndex(source.get(), index, options.get())
            })
        };
        let embedded = thumbnail(false).filter(|image| {
            // SAFETY: a valid CGImage.
            let short = unsafe { CGImageGetWidth(image.get()).min(CGImageGetHeight(image.get())) };
            short as u64 * 2 >= u64::from(size)
        });
        let image = embedded.or_else(|| thumbnail(true))?;
        Some(Decoded {
            pixels: pixels(&image).ok()?,
        })
    }

    /// ImageIO's own reading of the primary image's orientation (tests
    /// compare it with the container's).
    #[cfg(test)]
    pub(crate) fn orientation(bytes: &[u8]) -> Option<i64> {
        let (source, index) = source(bytes).ok()?;
        // SAFETY: a valid image source and index.
        let properties =
            Cf::new(unsafe { CGImageSourceCopyPropertiesAtIndex(source.get(), index, null()) })?;
        // SAFETY: a valid dictionary; the value follows the Get rule.
        let value = unsafe { CFDictionaryGetValue(properties.get(), kCGImagePropertyOrientation) };
        if value.is_null() {
            return None;
        }
        let mut orientation = 0i64;
        // SAFETY: a CFNumber read into an i64.
        unsafe { CFNumberGetValue(value, NUMBER_SINT64, (&raw mut orientation).cast()) }
            .then_some(orientation)
    }

    /// Draws `image` into a bitmap of its size: `bits` 16 (integer) or 32
    /// (float) per component, premultiplied RGBA, in `space`.
    fn draw(image: &Cf, space: CFTypeRef, bits: usize) -> Option<(usize, usize, Vec<u8>)> {
        // SAFETY: a valid CGImage.
        let (width, height) = unsafe { (CGImageGetWidth(image.get()), CGImageGetHeight(image.get())) };
        if width == 0 || height == 0 {
            return None;
        }
        let row = width.checked_mul(4 * bits / 8)?;
        let mut buffer = vec![0u8; row.checked_mul(height)?];
        let info = if bits == 32 {
            ALPHA_PREMULTIPLIED_LAST | FLOAT_COMPONENTS | BYTE_ORDER_32_LITTLE
        } else {
            ALPHA_PREMULTIPLIED_LAST | BYTE_ORDER_16_LITTLE
        };
        // SAFETY: the buffer holds `height` rows of `row` bytes and outlives
        // the context, which is released before it is returned.
        let context = Cf::new(unsafe {
            CGBitmapContextCreate(buffer.as_mut_ptr().cast(), width, height, bits, row, space, info)
        })?;
        let rect = CGRect {
            x: 0.0,
            y: 0.0,
            width: width as f64,
            height: height as f64,
        };
        // SAFETY: a valid bitmap context and CGImage.
        unsafe {
            CGContextSetBlendMode(context.get(), BLEND_COPY);
            CGContextDrawImage(context.get(), rect, image.get());
        }
        drop(context);
        Some((width, height, buffer))
    }

    /// The image's pixels (`Pixels`). Drawn into a bitmap of its own colour
    /// space, Core Graphics converts nothing: the values stay as coded,
    /// 10-bit (and deeper) files at 16 bits.
    fn pixels(image: &Cf) -> Result<Pixels> {
        // SAFETY: a valid CGImage; the colour space follows the Get rule.
        let space = unsafe { CGImageGetColorSpace(image.get()) };
        // SAFETY: a valid colour space (checked for NULL first).
        let rgb = !space.is_null() && unsafe { CGColorSpaceGetModel(space) } == MODEL_RGB;
        // SAFETY: a valid colour space; the data follows the Copy rule.
        let icc = rgb
            .then(|| Cf::new(unsafe { CGColorSpaceCopyICCData(space) }))
            .flatten()
            .map(|data| {
                // SAFETY: a valid CFData of `length` bytes.
                unsafe {
                    let length = CFDataGetLength(data.get()) as usize;
                    std::slice::from_raw_parts(CFDataGetBytePtr(data.get()), length).to_vec()
                }
            });
        if rgb && let Some((width, height, buffer)) = draw(image, space, 16) {
            let values: Vec<f32> = buffer
                .as_chunks::<8>()
                .0
                .iter()
                .flat_map(|p| {
                    let channel = |i: usize| f32::from(u16::from_le_bytes([p[i], p[i + 1]]));
                    // Opaque pixels are as coded; transparent ones unpremultiplied.
                    let alpha = channel(6).max(1.0);
                    [0, 2, 4].map(|i| (channel(i) / alpha).min(1.0))
                })
                .collect();
            let image = ImageBuffer::from_raw(width as u32, height as u32, values)
                .ok_or_else(|| anyhow!("HEIF image too large"))?;
            return Ok(Pixels::Encoded {
                image: DynamicImage::ImageRgb32F(image),
                icc,
            });
        }
        // Grey, or a space Core Graphics cannot draw into: converted by it
        // into extended linear sRGB (unclipped), then sRGB-encoded.
        // SAFETY: a constant colour space name.
        let linear = Cf::new(unsafe { CGColorSpaceCreateWithName(kCGColorSpaceExtendedLinearSRGB) })
            .ok_or_else(|| anyhow!("No extended sRGB colour space"))?;
        let (width, height, buffer) = draw(image, linear.get(), 32)
            .ok_or_else(|| anyhow!("The system could not draw this HEIF image"))?;
        let values: Vec<f32> = buffer
            .as_chunks::<16>()
            .0
            .iter()
            .flat_map(|p| {
                let channel = |i: usize| f32::from_le_bytes(p[i..i + 4].try_into().unwrap());
                let alpha = channel(12);
                let alpha = if alpha > 0.0 { alpha } else { 1.0 };
                [0, 4, 8].map(|i| crate::input_profile::encode(channel(i) / alpha))
            })
            .collect();
        let image = ImageBuffer::from_raw(width as u32, height as u32, values)
            .ok_or_else(|| anyhow!("HEIF image too large"))?;
        Ok(Pixels::Working(DynamicImage::ImageRgb32F(image)))
    }

    /// ImageIO's and Core Graphics' own decode of the primary image,
    /// converted by Core Graphics to extended linear sRGB (the tests'
    /// colour reference) and not oriented.
    #[cfg(test)]
    pub(crate) fn reference_linear_srgb(bytes: &[u8]) -> Option<(usize, usize, Vec<[f32; 3]>)> {
        let (source, index) = source(bytes).ok()?;
        // SAFETY: a valid image source and index.
        let image = Cf::new(unsafe { CGImageSourceCreateImageAtIndex(source.get(), index, null()) })?;
        // SAFETY: a constant colour space name.
        let linear =
            Cf::new(unsafe { CGColorSpaceCreateWithName(kCGColorSpaceExtendedLinearSRGB) })?;
        let (width, height, buffer) = draw(&image, linear.get(), 32)?;
        let pixels = buffer
            .as_chunks::<16>()
            .0
            .iter()
            .map(|p| [0, 4, 8].map(|i| f32::from_le_bytes(p[i..i + 4].try_into().unwrap())))
            .collect();
        Some((width, height, pixels))
    }

    #[cfg(test)]
    pub(crate) use test_encode::encode_heic;

    #[cfg(test)]
    mod test_encode {
        //! HEIC files made by ImageIO at test time (no photographs in the
        //! repository).
        use super::*;

        #[link(name = "ImageIO", kind = "framework")]
        unsafe extern "C" {
            static kCGImageDestinationLossyCompressionQuality: CFTypeRef;
            fn CGImageDestinationCreateWithData(
                data: CFTypeRef,
                kind: CFTypeRef,
                count: usize,
                options: CFTypeRef,
            ) -> CFTypeRef;
            fn CGImageDestinationAddImage(
                destination: CFTypeRef,
                image: CFTypeRef,
                properties: CFTypeRef,
            );
            fn CGImageDestinationFinalize(destination: CFTypeRef) -> bool;
        }

        #[link(name = "CoreFoundation", kind = "framework")]
        unsafe extern "C" {
            fn CFDataCreateMutable(allocator: CFTypeRef, capacity: isize) -> CFTypeRef;
            fn CFStringCreateWithBytes(
                allocator: CFTypeRef,
                bytes: *const u8,
                length: isize,
                encoding: u32,
                external: bool,
            ) -> CFTypeRef;
            fn CFNumberCreate(allocator: CFTypeRef, kind: isize, value: *const c_void) -> CFTypeRef;
        }

        #[link(name = "CoreGraphics", kind = "framework")]
        unsafe extern "C" {
            fn CGBitmapContextCreateImage(context: CFTypeRef) -> CFTypeRef;
        }

        fn string(text: &str) -> Cf {
            // SAFETY: UTF-8 bytes of `text` (encoding kCFStringEncodingUTF8).
            Cf::new(unsafe {
                CFStringCreateWithBytes(null(), text.as_ptr(), text.len() as isize, 0x0800_0100, false)
            })
            .unwrap()
        }

        /// A HEIC of `pixels` (16-bit RGB, row-major, `width` wide) in the
        /// named colour space (e.g. "kCGColorSpaceDisplayP3"), written by
        /// ImageIO at full quality with `orientation` (1–8); 16-bit input
        /// gives a 10-bit file.
        pub(crate) fn encode_heic(
            width: usize,
            height: usize,
            pixels: &[[u16; 3]],
            space: &str,
            orientation: i64,
        ) -> Vec<u8> {
            let mut buffer: Vec<u8> = pixels
                .iter()
                .flat_map(|p| [p[0], p[1], p[2], u16::MAX].map(u16::to_le_bytes))
                .flatten()
                .collect();
            let name = string(space);
            // SAFETY: a colour space name.
            let space = Cf::new(unsafe { CGColorSpaceCreateWithName(name.get()) }).unwrap();
            // SAFETY: the buffer holds `height` rows of `width * 8` bytes and
            // outlives the context.
            let context = Cf::new(unsafe {
                CGBitmapContextCreate(
                    buffer.as_mut_ptr().cast(),
                    width,
                    height,
                    16,
                    width * 8,
                    space.get(),
                    ALPHA_PREMULTIPLIED_LAST | BYTE_ORDER_16_LITTLE,
                )
            })
            .unwrap();
            // SAFETY: a valid bitmap context.
            let image = Cf::new(unsafe { CGBitmapContextCreateImage(context.get()) }).unwrap();
            // SAFETY: plain CF object creation.
            let data = Cf::new(unsafe { CFDataCreateMutable(null(), 0) }).unwrap();
            let kind = string("public.heic");
            // SAFETY: a mutable CFData and a UTI string.
            let destination = Cf::new(unsafe {
                CGImageDestinationCreateWithData(data.get(), kind.get(), 1, null())
            })
            .unwrap();
            let quality = 1.0f64;
            // SAFETY: reads one f64 (kCFNumberFloat64Type = 6).
            let quality = Cf::new(unsafe { CFNumberCreate(null(), 6, (&raw const quality).cast()) })
                .unwrap();
            let orientation = number(orientation).unwrap();
            // SAFETY: ImageIO constants and CF values.
            let properties = dictionary(&unsafe {
                [
                    (kCGImageDestinationLossyCompressionQuality, quality.get()),
                    (kCGImagePropertyOrientation, orientation.get()),
                ]
            })
            .unwrap();
            // SAFETY: a valid destination, image and dictionary.
            unsafe {
                CGImageDestinationAddImage(destination.get(), image.get(), properties.get());
                assert!(CGImageDestinationFinalize(destination.get()));
            }
            drop(destination);
            // SAFETY: a valid CFData of `length` bytes.
            unsafe {
                let length = CFDataGetLength(data.get()) as usize;
                std::slice::from_raw_parts(CFDataGetBytePtr(data.get()), length).to_vec()
            }
        }
    }
}

#[cfg(test)]
mod tests;
