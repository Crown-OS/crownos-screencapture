use std::time::{Duration, Instant};

use sctk::shell::WaylandSurface;
use sctk::shell::wlr_layer::{KeyboardInteractivity, Layer};
use wayland_client::protocol::wl_output::WlOutput;

use super::WaylandCapture;
use super::state::ProbeStatus;
use crate::capture::{CaptureError, PixelFormat, ShmFrame};

const PROBE_TIMEOUT: Duration = Duration::from_millis(500);
const PROBE_NAMESPACE: &str = "crownos-screenshot-probe";

impl WaylandCapture {
    /// Maps an invisible 1×1 layer surface and reports the output the compositor put it on,
    /// which is the output new surfaces open on.
    pub(super) fn probe_focused_output(&mut self) -> Result<Option<WlOutput>, CaptureError> {
        let Some(layer_shell) = &self.state.layer_shell else {
            return Ok(None);
        };
        let surface = self.state.compositor.create_surface(&self.handle);
        let empty_region = self
            .state
            .compositor
            .wl_compositor()
            .create_region(&self.handle, super::state::Silent);
        surface.set_input_region(Some(&empty_region));
        let layer = layer_shell.create_layer_surface(
            &self.handle,
            surface,
            Layer::Overlay,
            Some(PROBE_NAMESPACE),
            None,
        );
        layer.set_size(1, 1);
        layer.set_keyboard_interactivity(KeyboardInteractivity::None);
        layer.commit();
        self.state.probe = ProbeStatus::AwaitingConfigure;

        let transparent = ShmFrame::allocate(1, 1, PixelFormat::Bgra)?;
        let (pool, buffer) = self.shm_buffer(&transparent);
        let deadline = Instant::now() + PROBE_TIMEOUT;
        let mut attached = false;
        let output = loop {
            match &self.state.probe {
                ProbeStatus::Entered(output) => break Some(output.clone()),
                ProbeStatus::Closed => break None,
                ProbeStatus::Configured if !attached => {
                    layer.wl_surface().attach(Some(&buffer), 0, 0);
                    layer.wl_surface().damage_buffer(0, 0, 1, 1);
                    layer.commit();
                    attached = true;
                }
                _ => {}
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                break None;
            }
            self.dispatch_timeout(remaining)?;
        };
        self.state.probe = ProbeStatus::Idle;
        drop(layer);
        empty_region.destroy();
        buffer.destroy();
        pool.destroy();
        self.connection.flush()?;
        Ok(output)
    }
}
