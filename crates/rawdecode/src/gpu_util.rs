//! wgpu helpers for every GPU crate (this is the lowest crate on wgpu).

use wgpu::util::BufferInitDescriptor;

/// `wgpu::util::DeviceExt::create_buffer_init` without its panic. On a lost
/// device the new buffer is invalid and cannot be mapped, and wgpu's version
/// panicked in whatever thread made one: after 'GPU device lost: Out of
/// memory' on a Radeon 680M an export panicked creating the processor's
/// 'LUT shaper (none)' buffer and every processor rebuilt after it did too
/// (G76/WCC6). Here that buffer is returned invalid (and the failure
/// logged); the work that uses it fails with a validation error the caller
/// reports, as with any other resource of a lost device.
pub fn create_buffer_init(
    device: &wgpu::Device,
    descriptor: &BufferInitDescriptor,
) -> wgpu::Buffer {
    let unpadded = descriptor.contents.len() as wgpu::BufferAddress;
    if unpadded == 0 {
        return device.create_buffer(&wgpu::BufferDescriptor {
            label: descriptor.label,
            size: 0,
            usage: descriptor.usage,
            mapped_at_creation: false,
        });
    }
    // As wgpu's: a mapped-at-creation buffer's size is a multiple of
    // COPY_BUFFER_ALIGNMENT and not zero.
    let align = wgpu::COPY_BUFFER_ALIGNMENT - 1;
    let size = ((unpadded + align) & !align).max(wgpu::COPY_BUFFER_ALIGNMENT);
    let buffer = device.create_buffer(&wgpu::BufferDescriptor {
        label: descriptor.label,
        size,
        usage: descriptor.usage,
        mapped_at_creation: true,
    });
    match buffer.get_mapped_range_mut(..) {
        Ok(mut range) => {
            range
                .slice(..unpadded as usize)
                .copy_from_slice(descriptor.contents);
            drop(range);
            buffer.unmap();
        }
        Err(error) => log::error!(
            "GPU buffer '{}' could not be filled (device lost?): {error:?}",
            descriptor.label.unwrap_or("unnamed")
        ),
    }
    buffer
}
