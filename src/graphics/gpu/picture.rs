use vello::wgpu::{
    Device, Extent3d, Origin3d, Queue, TexelCopyBufferLayout, TexelCopyTextureInfo, Texture,
    TextureAspect, TextureDescriptor, TextureDimension, TextureFormat, TextureUsages,
};

use crate::graphics::image::Bitmap;

/*
 * how many sizes of the image to prepare for drawing it `drawn` big: the image
 * itself and each half size down to the drawn one, so a big icon shrunk into
 * a small slot still looks smooth. an image drawn at its own size or larger,
 * like a wallpaper, needs only itself
 */
pub fn levels(image: &Bitmap, drawn: (f32, f32)) -> u32 {
    let shrink = (image.width() as f32 / drawn.0.max(1.0)).max(image.height() as f32 / drawn.1.max(1.0));

    if shrink <= 1.0 {
        return 1;
    }

    // halving until it fits, and one more so the gpu can blend between the two around the drawn size
    let wanted = shrink.log2().ceil() as u32 + 1;

    wanted.min(image.width().max(image.height()).ilog2() + 1)
}

/*
 * an image as a gpu texture with `count` sizes of it (see `levels`). colors are
 * premultiplied, which keeps soft edges from picking up dark fringes when they
 * blend; an opaque image is the same either way, so it goes up as it is, and a
 * fullscreen photo costs a copy instead of a pass over every pixel
 */
pub fn upload(device: &Device, queue: &Queue, image: &Bitmap, count: u32) -> Texture {
    let first = if opaque(image) { std::borrow::Cow::Borrowed(image.pixels.as_slice()) } else { std::borrow::Cow::Owned(premultiplied(image)) };

    let mut levels = vec![first];

    let mut width = image.width();
    let mut height = image.height();

    let mut sizes = vec![(width, height)];

    while levels.len() < count as usize && (width > 1 || height > 1) {
        let last = levels.last().expect("failed to find the last image size");

        let smaller = halve(last, width, height);

        width = (width / 2).max(1);
        height = (height / 2).max(1);

        levels.push(std::borrow::Cow::Owned(smaller));
        sizes.push((width, height));
    }

    let texture = device.create_texture(&TextureDescriptor {
        label: None,
        size: Extent3d {
            width: image.width(),
            height: image.height(),
            depth_or_array_layers: 1,
        },
        mip_level_count: levels.len() as u32,
        sample_count: 1,
        dimension: TextureDimension::D2,
        format: TextureFormat::Rgba8Unorm,
        usage: TextureUsages::TEXTURE_BINDING | TextureUsages::COPY_DST,
        view_formats: &[],
    });

    for (level, (pixels, (width, height))) in levels.iter().zip(sizes).enumerate() {
        let target = TexelCopyTextureInfo {
            texture: &texture,
            mip_level: level as u32,
            origin: Origin3d::ZERO,
            aspect: TextureAspect::All,
        };

        let layout = TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(width * 4),
            rows_per_image: Some(height),
        };

        let extent = Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        };

        queue.write_texture(target, pixels, layout, extent);
    }

    texture
}

// every pixel fully covers what's under it, so premultiplying would change nothing
fn opaque(image: &Bitmap) -> bool {
    image.pixels.as_chunks::<4>().0.iter().all(|pixel| pixel[3] == 255)
}

fn premultiplied(image: &Bitmap) -> Vec<u8> {
    let mut pixels = image.pixels.to_vec();

    for pixel in pixels.as_chunks_mut::<4>().0 {
        let alpha = u16::from(pixel[3]);

        for channel in &mut pixel[..3] {
            *channel = (u16::from(*channel) * alpha / 255) as u8;
        }
    }

    pixels
}

// each pixel of the half size image is the average of the two by two it covers
fn halve(pixels: &[u8], width: u32, height: u32) -> Vec<u8> {
    let half_width = (width / 2).max(1);
    let half_height = (height / 2).max(1);

    let mut half = Vec::with_capacity((half_width * half_height * 4) as usize);

    for y in 0..half_height {
        for x in 0..half_width {
            for channel in 0..4 {
                let mut total = 0u32;

                for (dx, dy) in [(0, 0), (1, 0), (0, 1), (1, 1)] {
                    // an odd edge repeats its last pixel
                    let source_x = (x * 2 + dx).min(width - 1);
                    let source_y = (y * 2 + dy).min(height - 1);

                    let index = ((source_y * width + source_x) * 4 + channel) as usize;

                    total += u32::from(pixels[index]);
                }

                half.push((total / 4) as u8);
            }
        }
    }

    half
}
