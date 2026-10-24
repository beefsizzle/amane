use smithay_client_toolkit::globals::ProvidesBoundGlobal;
use smithay_client_toolkit::reexports::protocols_wlr::layer_shell::v1::client::{
    zwlr_layer_shell_v1::{self, ZwlrLayerShellV1},
    zwlr_layer_surface_v1::{Anchor, KeyboardInteractivity},
};
use smithay_client_toolkit::shell::wlr_layer::{
    LayerShell, LayerShellHandler, LayerSurface as ToolkitLayerSurface, LayerSurfaceConfigure,
};
use wayland_client::{
    Connection, QueueHandle,
    protocol::{wl_output::WlOutput, wl_surface::WlSurface},
};

use crate::scale::ScaleFactor;
use crate::{Horizontal, Keyboard, Layer, LayerWindow, Margin, Vertical, WindowSize, Zone};

use super::WaylandState;
use super::layer_surface::{self, LayerSurface};

// the parts of the window the compositor knows about, kept to see what the next view changes
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Settings {
    pub width: WindowSize,
    pub height: WindowSize,

    pub vertical: Vertical,
    pub horizontal: Horizontal,

    pub margin: Margin,
    pub layer: Layer,
    pub keyboard: Keyboard,
    pub zone: Zone,

    pub namespace: &'static str,

    pub visible: bool,
}

impl From<&LayerWindow> for Settings {
    fn from(window: &LayerWindow) -> Self {
        Self {
            width: window.width,
            height: window.height,

            vertical: window.vertical,
            horizontal: window.horizontal,

            margin: window.margin,
            layer: window.layer,
            keyboard: window.keyboard,
            zone: window.zone,

            namespace: window.namespace,

            visible: window.visible,
        }
    }
}

// the windows' surfaces are made in layer_surface.rs, so these never come
impl LayerShellHandler for WaylandState {
    fn configure(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &ToolkitLayerSurface,
        _: LayerSurfaceConfigure,
        _: u32,
    ) {
    }

    fn closed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &ToolkitLayerSurface) {}
}

pub fn create(
    layer_shell: &LayerShell,
    surface: WlSurface,
    output: Option<&WlOutput>,
    qh: &QueueHandle<WaylandState>,
    settings: &Settings,
    scale_factor: ScaleFactor,
) -> LayerSurface {
    let shell: ZwlrLayerShellV1 =
        ProvidesBoundGlobal::<ZwlrLayerShellV1, 4>::bound_global(layer_shell)
            .expect("failed to find wlr-layer-shell");

    // with no output given, the compositor chooses the monitor
    let layer_surface = LayerSurface::new(
        &shell,
        surface,
        output,
        to_layer(settings.layer),
        settings.namespace,
        qh,
    );

    apply(&layer_surface, settings, scale_factor);

    // a window that starts hidden makes this first commit once it is shown
    if settings.visible {
        layer_surface.wl_surface().commit();
    }

    layer_surface
}

// the requests only take effect with the next commit
pub fn apply(layer_surface: &LayerSurface, settings: &Settings, scale_factor: ScaleFactor) {
    let layer_surface = layer_surface.layer();

    layer_surface::set_layer(layer_surface, to_layer(settings.layer));

    layer_surface.set_size(
        to_pixels(settings.width, scale_factor),
        to_pixels(settings.height, scale_factor),
    );

    layer_surface.set_anchor(to_anchor(settings));

    let margin = settings.margin;

    layer_surface.set_margin(
        scale_factor.coordinate(margin.top),
        scale_factor.coordinate(margin.right),
        scale_factor.coordinate(margin.bottom),
        scale_factor.coordinate(margin.left),
    );

    layer_surface.set_keyboard_interactivity(to_interactivity(settings.keyboard));

    layer_surface.set_exclusive_zone(to_exclusive_zone(settings, scale_factor));
}

// 0 tells the compositor to stretch between the anchored edges
pub fn to_pixels(size: WindowSize, scale_factor: ScaleFactor) -> u32 {
    match size {
        WindowSize::Full => 0,
        WindowSize::Fixed(pixels) => scale_factor.pixels(pixels),
    }
}

fn to_anchor(settings: &Settings) -> Anchor {
    let mut anchor = Anchor::empty();

    match settings.vertical {
        Vertical::Top => anchor |= Anchor::Top,
        Vertical::Middle => {}
        Vertical::Bottom => anchor |= Anchor::Bottom,
    }

    match settings.horizontal {
        Horizontal::Left => anchor |= Anchor::Left,
        Horizontal::Middle => {}
        Horizontal::Right => anchor |= Anchor::Right,
    }

    // stretching only works between two opposite anchored edges
    if settings.width == WindowSize::Full {
        anchor |= Anchor::Left | Anchor::Right;
    }

    if settings.height == WindowSize::Full {
        anchor |= Anchor::Top | Anchor::Bottom;
    }

    anchor
}

fn to_layer(layer: Layer) -> zwlr_layer_shell_v1::Layer {
    match layer {
        Layer::Background => zwlr_layer_shell_v1::Layer::Background,
        Layer::Bottom => zwlr_layer_shell_v1::Layer::Bottom,
        Layer::Top => zwlr_layer_shell_v1::Layer::Top,
        Layer::Overlay => zwlr_layer_shell_v1::Layer::Overlay,
    }
}

fn to_interactivity(keyboard: Keyboard) -> KeyboardInteractivity {
    match keyboard {
        Keyboard::None => KeyboardInteractivity::None,
        Keyboard::Exclusive => KeyboardInteractivity::Exclusive,
        Keyboard::OnDemand => KeyboardInteractivity::OnDemand,
    }
}

fn to_exclusive_zone(settings: &Settings, scale_factor: ScaleFactor) -> i32 {
    match settings.zone {
        Zone::Reserve => measure_thickness(settings, scale_factor),
        Zone::Respect => 0,
        Zone::Ignore => -1,
    }
}

// a bar on the left or right edge is as thick as its width, any other bar as its height
fn measure_thickness(settings: &Settings, scale_factor: ScaleFactor) -> i32 {
    let margin = settings.margin;

    // the compositor adds the anchored edge's margin, but never the one facing the windows
    let (size, inner_margin) = match (settings.horizontal, settings.width) {
        (Horizontal::Left, WindowSize::Fixed(_)) => (settings.width, margin.right),
        (Horizontal::Right, WindowSize::Fixed(_)) => (settings.width, margin.left),

        _ => match settings.vertical {
            Vertical::Top => (settings.height, margin.bottom),
            Vertical::Bottom => (settings.height, margin.top),
            Vertical::Middle => (settings.height, 0),
        },
    };

    let thickness = to_pixels(size, scale_factor);
    let thickness = i32::try_from(thickness).expect("failed to convert reserved space");

    thickness + scale_factor.coordinate(inner_margin)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Full;

    #[test]
    fn scaling_preserves_full_and_exclusive_zone_sentinels() {
        let factor = ScaleFactor::new(1.4);
        let bar = LayerWindow::new()
            .width(Full)
            .height(30.0)
            .space(Zone::Reserve);
        let mut settings = Settings::from(&bar);

        assert_eq!(to_pixels(settings.width, factor), 0);
        assert_eq!(to_pixels(settings.height, factor), 42);
        assert_eq!(to_exclusive_zone(&settings, factor), 42);

        settings.zone = Zone::Respect;
        assert_eq!(to_exclusive_zone(&settings, factor), 0);
        settings.zone = Zone::Ignore;
        assert_eq!(to_exclusive_zone(&settings, factor), -1);

        settings.zone = Zone::Reserve;
        settings.horizontal = Horizontal::Left;
        settings.width = WindowSize::Fixed(20.0);
        assert_eq!(to_exclusive_zone(&settings, ScaleFactor::new(1.5)), 30);

        // the margin facing the windows is reserved too
        settings.horizontal = Horizontal::Middle;
        settings.width = WindowSize::Full;
        settings.height = WindowSize::Fixed(30.0);
        settings.vertical = Vertical::Top;
        settings.margin.bottom = 10;
        assert_eq!(to_exclusive_zone(&settings, ScaleFactor::new(1.0)), 40);
    }
}
