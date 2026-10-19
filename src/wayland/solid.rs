use smithay_client_toolkit::reexports::protocols::wp::single_pixel_buffer::v1::client::wp_single_pixel_buffer_manager_v1::{
    self, WpSinglePixelBufferManagerV1,
};
use smithay_client_toolkit::{registry::SimpleGlobal, shm::raw::RawPool};
use wayland_client::{
    Connection, Dispatch, QueueHandle,
    protocol::{
        wl_buffer::{self, WlBuffer},
        wl_shm,
    },
};

use crate::Color;

use super::{WaylandState, surface::OpenWindow};

// the one pixel a solid window shows, kept until its color changes
pub struct SolidBuffer {
    pub color: Color,
    pub buffer: WlBuffer,
}

impl Drop for SolidBuffer {
    fn drop(&mut self) {
        self.buffer.destroy();
    }
}

impl OpenWindow {
    /*
     * false when the window has no viewport to stretch the pixel with, then the gpu
     * draws the children instead; otherwise the pixel is stretched to the window's size
     * and committed, and the gpu gives back whatever it held from earlier frames
     */
    pub fn present_solid(&mut self, color: Color) -> bool {
        let Some(fractional) = &self.fractional else {
            return false;
        };

        let old = match &self.solid {
            Some(solid) if solid.color == color => None,

            _ => {
                let Some(buffer) = self.make_pixel(color) else {
                    return false;
                };

                self.solid.replace(SolidBuffer { color, buffer })
            }
        };

        let Some(solid) = &self.solid else {
            return false;
        };

        fractional
            .viewport
            .set_destination(self.width as i32, self.height as i32);

        let surface = self.role.wl_surface();

        surface.attach(Some(&solid.buffer), 0, 0);
        surface.damage_buffer(0, 0, 1, 1);
        surface.commit();

        // the old pixel goes once the new one is committed, so the window never shows nothing
        drop(old);

        self.gpu.release();

        true
    }

    // the compositor's own single pixel buffers when it has them, else one pixel of shared memory
    fn make_pixel(&self, color: Color) -> Option<WlBuffer> {
        let red = premultiply(color.r, color.a);
        let green = premultiply(color.g, color.a);
        let blue = premultiply(color.b, color.a);

        if let Some(manager) = &self.single_pixel {
            let buffer = manager.create_u32_rgba_buffer(
                widen(red),
                widen(green),
                widen(blue),
                widen(color.a),
                &self.qh,
                (),
            );

            return Some(buffer);
        }

        let shm = SimpleGlobal::<wl_shm::WlShm, 1>::from_bound(self.shm.clone());

        let mut pool = RawPool::new(4, &shm).ok()?;

        // argb8888 lies in memory as little endian, so blue comes first
        pool.mmap().copy_from_slice(&[blue, green, red, color.a]);

        // the buffer outlives its pool, which goes once the buffer is made
        let buffer = pool.create_buffer(0, 1, 1, 4, wl_shm::Format::Argb8888, (), &self.qh);

        Some(buffer)
    }
}

// both kinds of buffer hold premultiplied colors
fn premultiply(value: u8, alpha: u8) -> u8 {
    let premultiplied = (u16::from(value) * u16::from(alpha) + 127) / 255;

    premultiplied as u8
}

// single pixel buffers count each channel over the whole u32 range
fn widen(value: u8) -> u32 {
    u32::from(value) * 0x0101_0101
}

// neither the manager nor its buffers say anything amane needs, a release only means the pixel can be reused
impl Dispatch<WpSinglePixelBufferManagerV1, ()> for WaylandState {
    fn event(
        _: &mut Self,
        _: &WpSinglePixelBufferManagerV1,
        _: wp_single_pixel_buffer_manager_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<WlBuffer, ()> for WaylandState {
    fn event(
        _: &mut Self,
        _: &WlBuffer,
        _: wl_buffer::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn channels_are_premultiplied_and_widened_over_the_full_range() {
        assert_eq!(premultiply(255, 255), 255);
        assert_eq!(premultiply(0, 255), 0);
        assert_eq!(premultiply(255, 0), 0);
        assert_eq!(premultiply(255, 128), 128);
        assert_eq!(premultiply(100, 128), 50);

        assert_eq!(widen(255), u32::MAX);
        assert_eq!(widen(0), 0);
    }
}
