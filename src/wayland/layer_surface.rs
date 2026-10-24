use smithay_client_toolkit::reexports::protocols_wlr::layer_shell::v1::client::{
    zwlr_layer_shell_v1::{self, ZwlrLayerShellV1},
    zwlr_layer_surface_v1::{self, ZwlrLayerSurfaceV1},
};
use wayland_client::{
    Connection, Dispatch, Proxy, QueueHandle,
    protocol::{
        wl_callback::{self, WlCallback},
        wl_output::WlOutput,
        wl_surface::WlSurface,
    },
};

use super::WaylandState;

/*
 * a layer window's role, made here instead of by smithay-client-toolkit, which acks every configure as it arrives.
 * acking one sent before the window hid is a protocol error, so the window decides, see `stale_configures`
 */
pub struct LayerSurface {
    wl_surface: WlSurface,
    layer: ZwlrLayerSurfaceV1,
}

// what each layer surface carries, to find its window when an event comes
pub struct LayerData {
    wl_surface: WlSurface,
}

// answers the sync sent as a window hid; configures before it are stale
pub struct StaleUntil {
    wl_surface: WlSurface,
}

impl StaleUntil {
    pub fn new(wl_surface: WlSurface) -> Self {
        Self { wl_surface }
    }
}

impl LayerSurface {
    pub fn new(
        shell: &ZwlrLayerShellV1,
        wl_surface: WlSurface,
        output: Option<&WlOutput>,
        layer: zwlr_layer_shell_v1::Layer,
        namespace: &str,
        qh: &QueueHandle<WaylandState>,
    ) -> Self {
        let data = LayerData {
            wl_surface: wl_surface.clone(),
        };

        let layer = shell.get_layer_surface(
            &wl_surface,
            output,
            layer,
            String::from(namespace),
            qh,
            data,
        );

        Self { wl_surface, layer }
    }

    pub fn wl_surface(&self) -> &WlSurface {
        &self.wl_surface
    }

    pub fn layer(&self) -> &ZwlrLayerSurfaceV1 {
        &self.layer
    }
}

impl Drop for LayerSurface {
    fn drop(&mut self) {
        self.layer.destroy();
        self.wl_surface.destroy();
    }
}

impl Dispatch<ZwlrLayerSurfaceV1, LayerData> for WaylandState {
    fn event(
        state: &mut Self,
        layer: &ZwlrLayerSurfaceV1,
        event: zwlr_layer_surface_v1::Event,
        data: &LayerData,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            zwlr_layer_surface_v1::Event::Configure {
                serial,
                width,
                height,
            } => {
                let Some(window) = state.window(&data.wl_surface) else {
                    return;
                };

                // sent before the window hid, showing it again brings a new one
                if window.stale_configures {
                    return;
                }

                layer.ack_configure(serial);

                window.resize(width, height);
            }

            zwlr_layer_surface_v1::Event::Closed => state.close(&data.wl_surface),

            _ => {}
        }
    }
}

impl Dispatch<WlCallback, StaleUntil> for WaylandState {
    fn event(
        state: &mut Self,
        _: &WlCallback,
        event: wl_callback::Event,
        data: &StaleUntil,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if !matches!(event, wl_callback::Event::Done { .. }) {
            return;
        }

        if let Some(window) = state.window(&data.wl_surface) {
            window.stale_configures = false;
        }
    }
}

// set_layer came with the protocol's second version
pub fn set_layer(layer: &ZwlrLayerSurfaceV1, value: zwlr_layer_shell_v1::Layer) {
    if layer.version() >= 2 {
        layer.set_layer(value);
    }
}
