use std::collections::HashMap;

use swash::FontRef;
use vello::wgpu::{
    Device, Extent3d, Origin3d, Queue, TexelCopyBufferLayout, TexelCopyTextureInfo, Texture,
    TextureAspect, TextureDescriptor, TextureDimension, TextureFormat, TextureUsages,
};

use crate::graphics::letter;

const SIZE: u32 = 1024;

// a gap between letters, so none reads a neighbour's pixels
const PADDING: u32 = 1;

// where one letter's pixels are in the atlas, and where they sit from the pen on the baseline
#[derive(Clone, Copy)]
pub struct Letter {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
    pub left: f32,
    pub top: f32,
}

// the font, the letter and its size in pixels
type Key = (u64, u16, u32);

/*
 * every letter drawn so far, in one texture of coverage values; letters fill
 * it in rows, and a full atlas is emptied when the next frame starts, since
 * letters queued this frame still read their pixels
 */
pub struct Atlas {
    device: Device,

    // made with the first letter, and given back with the window's other textures
    texture: Option<Texture>,

    letters: HashMap<Key, Option<Letter>>,

    // the row being filled: its top, how far along it is and its tallest letter
    row_top: u32,
    row_x: u32,
    row_height: u32,

    // no room was left this frame, so it starts over empty next frame
    full: bool,
}

impl Atlas {
    pub fn new(device: &Device) -> Self {
        Self {
            device: device.clone(),
            texture: None,
            letters: HashMap::new(),
            row_top: 0,
            row_x: 0,
            row_height: 0,
            full: false,
        }
    }

    fn texture(&mut self) -> &Texture {
        self.texture.get_or_insert_with(|| make(&self.device))
    }

    // none until the first letter is drawn
    pub fn made(&self) -> Option<&Texture> {
        self.texture.as_ref()
    }

    // a hidden window's letters go with its texture, and are drawn again when it shows
    pub fn forget(&mut self) {
        self.texture = None;
        self.letters.clear();

        self.row_top = 0;
        self.row_x = 0;
        self.row_height = 0;

        self.full = false;
    }

    // the last frame's draws were submitted, so nothing reads the old pixels anymore
    pub fn start_frame(&mut self) {
        if !self.full {
            return;
        }

        self.letters.clear();

        self.row_top = 0;
        self.row_x = 0;
        self.row_height = 0;

        self.full = false;
    }

    // none for a letter without pixels, like a space
    pub fn letter(
        &mut self,
        queue: &Queue,
        font: FontRef<'static>,
        id: u16,
        size: f32,
    ) -> Option<Letter> {
        let key = (font.key.value(), id, size.to_bits());

        if let Some(letter) = self.letters.get(&key) {
            return *letter;
        }

        let Some(pixels) = letter::rasterize(font, id, size) else {
            self.letters.insert(key, None);

            return None;
        };

        let width = pixels.width;
        let height = pixels.height;

        // too big for the atlas at all, kept so it isn't turned into pixels every frame
        if width + PADDING > SIZE || height + PADDING > SIZE {
            self.letters.insert(key, None);

            return None;
        }

        // not kept, so it gets placed again once the atlas is emptied
        let (x, y) = self.place(width, height)?;

        let target = TexelCopyTextureInfo {
            texture: self.texture(),
            mip_level: 0,
            origin: Origin3d { x, y, z: 0 },
            aspect: TextureAspect::All,
        };

        let layout = TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(width),
            rows_per_image: Some(height),
        };

        let extent = Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        };

        queue.write_texture(target, &pixels.coverage, layout, extent);

        let letter = Letter {
            x,
            y,
            width,
            height,
            left: pixels.left,
            top: pixels.top,
        };

        self.letters.insert(key, Some(letter));

        Some(letter)
    }

    // the next free spot in the rows, none once the atlas is full
    fn place(&mut self, width: u32, height: u32) -> Option<(u32, u32)> {
        if self.row_x + width + PADDING > SIZE {
            self.row_top += self.row_height + PADDING;
            self.row_x = 0;
            self.row_height = 0;
        }

        if self.row_top + height + PADDING > SIZE {
            self.full = true;

            return None;
        }

        let spot = (self.row_x, self.row_top);

        self.row_x += width + PADDING;
        self.row_height = self.row_height.max(height);

        Some(spot)
    }
}

fn make(device: &Device) -> Texture {
    device.create_texture(&TextureDescriptor {
        label: None,
        size: Extent3d {
            width: SIZE,
            height: SIZE,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: TextureDimension::D2,
        format: TextureFormat::R8Unorm,
        usage: TextureUsages::TEXTURE_BINDING | TextureUsages::COPY_DST,
        view_formats: &[],
    })
}
