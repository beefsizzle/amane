use super::Gpu;

/*
 * a hidden window draws nothing until it shows again, so it gives back
 * what it only needs while shown: its swapchain, which holds several images
 * the size of the window, its spare and kept textures, and its gpu copies of
 * images. the decoded pictures stay with the loader, so showing the window
 * again only uploads them instead of reading every file again
 */
impl Gpu {
    /*
     * `forget` also lets the loader drop the decoded pictures this window was the last to show: right for a window
     * that now shows its picture through the compositor, which has its own copy, but not for one that only hides
     */
    pub fn release(&mut self, forget: bool) {
        // nothing was drawn since the last release, or ever, so there is nothing to give back
        if self.config.width == 0 {
            return;
        }

        self.spare.clear();
        self.painted.clear();

        if forget {
            // nothing is drawn any more, so every picture counts as no longer shown
            self.drawn.clear();
            self.forget_unshown();
        } else {
            // the gpu copies go; the loader keeps the pictures, and lets them go
            // once a window that shows them again stops showing them
            for (key, _) in self.shown.drain() {
                self.images.remove(&key);

                self.quads.forget(key);
            }

            self.drawn.clear();
        }

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
