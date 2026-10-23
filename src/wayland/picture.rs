use std::cell::RefCell;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::rc::{Rc, Weak};

use smithay_client_toolkit::{registry::SimpleGlobal, shm::raw::RawPool};
use wayland_client::protocol::{wl_buffer::WlBuffer, wl_shm};

use crate::graphics::image::{self, Bitmap};

use super::surface::OpenWindow;

/*
 * a picture the compositor draws: the decoded pixels go into shared memory once, and every window showing the same
 * file attaches that one buffer, cropped and scaled by its viewport. so a fullscreen wallpaper needs no gpu memory
 * of its own (no swapchain, no textures), on any number of monitors
 */
pub struct PictureBuffer {
    buffer: WlBuffer,
    width: u32,
    height: u32,
}

impl Drop for PictureBuffer {
    fn drop(&mut self) {
        self.buffer.destroy();
    }
}

// every window draws on the main thread. a buffer goes once no window holds it
thread_local! {
    static PICTURES: RefCell<HashMap<PathBuf, Weak<PictureBuffer>>> = RefCell::new(HashMap::new());
}

fn cached(path: &Path) -> Option<Rc<PictureBuffer>> {
    PICTURES.with(|pictures| pictures.borrow().get(path).and_then(Weak::upgrade))
}

/// called from the view: starts decoding the file unless a window already shows it, and makes the window that asked
/// draw again once it's decoded
pub(crate) fn want(path: &Path) {
    if cached(path).is_none() {
        let _ = image::load(path, None, 0);
    }
}

impl OpenWindow {
    /*
     * false while the file is still decoding or can't be read, or when there's no viewport to scale with: then the gpu
     * draws the children instead. otherwise the picture is attached, cropped to cover the window, and the gpu gives
     * back whatever it held from earlier frames
     */
    pub fn present_picture(&mut self, path: &Path) -> bool {
        let Some(fractional) = &self.fractional else {
            return false;
        };

        let picture = match cached(path) {
            Some(picture) => picture,
            None => {
                let Some(bitmap) = image::load(path, None, 0) else {
                    return false;
                };

                let Some(picture) = self.make_picture(&bitmap) else {
                    return false;
                };

                let picture = Rc::new(picture);

                PICTURES.with(|pictures| {
                    let mut pictures = pictures.borrow_mut();

                    pictures.retain(|_, picture| picture.strong_count() > 0);
                    pictures.insert(path.to_path_buf(), Rc::downgrade(&picture));
                });

                // the pixels live in the shared memory now, the decoded copy isn't needed
                image::forget(&bitmap);

                picture
            }
        };

        // the part of the picture that covers the window, cut evenly off the longer side
        let (width, height) = (self.width as f64, self.height as f64);
        let (picture_width, picture_height) = (f64::from(picture.width), f64::from(picture.height));

        let scale = (width / picture_width).max(height / picture_height);
        let (source_width, source_height) = (width / scale, height / scale);

        fractional.viewport.set_source(
            (picture_width - source_width) / 2.0,
            (picture_height - source_height) / 2.0,
            source_width,
            source_height,
        );

        fractional
            .viewport
            .set_destination(self.width as i32, self.height as i32);

        let surface = self.role.wl_surface();

        surface.attach(Some(&picture.buffer), 0, 0);
        surface.damage_buffer(0, 0, picture.width as i32, picture.height as i32);
        surface.commit();

        // the one shown before goes once the new one is committed, so the window never shows nothing
        let old = self.picture.replace(picture);

        drop(old);

        self.gpu.release(true);

        true
    }

    // the window draws with the gpu again: the viewport stops cropping, and the picture goes after that frame
    pub fn leave_picture(&mut self) -> Option<Rc<PictureBuffer>> {
        let old = self.picture.take()?;

        if let Some(fractional) = &self.fractional {
            fractional.viewport.set_source(-1.0, -1.0, -1.0, -1.0);
        }

        Some(old)
    }

    // the pixels as premultiplied argb in shared memory, the format every compositor reads
    fn make_picture(&self, bitmap: &Bitmap) -> Option<PictureBuffer> {
        let (width, height) = (bitmap.width(), bitmap.height());

        let stride = width.checked_mul(4)?;
        let size = usize::try_from(stride).ok()?.checked_mul(usize::try_from(height).ok()?)?;

        let shm = SimpleGlobal::<wl_shm::WlShm, 1>::from_bound(self.shm.clone());

        let mut pool = RawPool::new(size, &shm).ok()?;

        // argb8888 lies in memory as little endian, so blue comes first
        let pixels = pool.mmap().as_chunks_mut::<4>().0.iter_mut().zip(bitmap.pixels.as_chunks::<4>().0);

        // an opaque picture, like any photo, is the same premultiplied, so only its channels swap. for a fullscreen
        // wallpaper that's a copy instead of the slowest step of showing it
        if bitmap.pixels.as_chunks::<4>().0.iter().all(|pixel| pixel[3] == 255) {
            for (into, pixel) in pixels {
                *into = [pixel[2], pixel[1], pixel[0], 255];
            }
        } else {
            for (into, pixel) in pixels {
                let alpha = u16::from(pixel[3]);
                let premultiply = |value: u8| ((u16::from(value) * alpha + 127) / 255) as u8;

                into.copy_from_slice(&[premultiply(pixel[2]), premultiply(pixel[1]), premultiply(pixel[0]), pixel[3]]);
            }
        }

        // the buffer outlives its pool: the compositor keeps the memory, and this program's mapping goes with the pool
        let buffer = pool.create_buffer(
            0,
            i32::try_from(width).ok()?,
            i32::try_from(height).ok()?,
            i32::try_from(stride).ok()?,
            wl_shm::Format::Argb8888,
            (),
            &self.qh,
        );

        Some(PictureBuffer { buffer, width, height })
    }
}
