use super::Gpu;

// a hidden window gives back its swapchain, textures and gpu copies of images
impl Gpu {
    // `forget` also lets the loader drop pictures only this window showed
    pub fn release(&mut self, forget: bool) {
        // nothing was drawn since the last release
        if self.config.width == 0 {
            return;
        }

        self.spare.clear();
        self.painted.clear();

        if forget {
            self.drawn.clear();
            self.forget_unshown();
        } else {
            // only the gpu copies go, the loader keeps the pictures
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

        if let Some(surface) = &self.surface {
            surface.configure(&self.device, &tiny);
        }

        // so the next frame configures the real size again
        self.config.width = 0;
        self.config.height = 0;
    }
}

impl Gpu {
    /*
     * for a window the compositor draws from now on. the surface goes too: once vulkan has presented to it, every
     * buffer attached needs an explicit sync point, which shared memory doesn't have. call it before attaching
     */
    pub fn hand_over(&mut self) {
        self.release(true);

        self.surface = None;

        self.config.width = 0;
        self.config.height = 0;
    }

    // sizes the swapchain to `config`, making the surface again first after `hand_over`
    pub(super) fn configure_surface(&mut self) {
        self.surface();

        if let Some(surface) = &self.surface {
            surface.configure(&self.device, &self.config);
        }
    }

    pub(super) fn surface(&mut self) -> &vello::wgpu::Surface<'static> {
        let (display, window) = (self.display, self.window);

        self.surface.get_or_insert_with(|| {
            super::target::create(&super::shared::instance(display), display, window)
        })
    }
}
