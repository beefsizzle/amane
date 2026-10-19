use super::Gpu;

/*
 * a hidden window draws nothing until it shows again, so it gives back
 * what it only needs while shown: its swapchain, which holds several images
 * the size of the window, its spare and kept textures, and its copies of images
 */
impl Gpu {
    pub fn release(&mut self) {
        // nothing was drawn since the last release, or ever, so there is nothing to give back
        if self.config.width == 0 {
            return;
        }

        self.spare.clear();
        self.painted.clear();

        // nothing was drawn, so every image counts as no longer shown
        self.drawn.clear();
        self.forget_unshown();

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
