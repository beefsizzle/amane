use super::Gpu;

// a hidden window gives back its swapchain, textures and gpu copies of images
impl Gpu {
    pub fn release(&mut self) {
        // nothing was drawn since the last release
        if self.config.width == 0 {
            return;
        }

        self.spare.clear();
        self.painted.clear();

        // the gpu copies go; the loader keeps the pictures, and lets them go
        // once a window that shows them again stops showing them
        for (key, _) in self.shown.drain() {
            self.images.remove(&key);

            self.quads.forget(key);
        }

        self.drawn.clear();

        // a swapchain can't be dropped on its own, but a 1x1 one takes the place of the big one
        let mut tiny = self.config.clone();

        tiny.width = 1;
        tiny.height = 1;

        self.surface.configure(&self.device, &tiny);

        // so the next frame configures the real size again
        self.config.width = 0;
        self.config.height = 0;
    }
}
