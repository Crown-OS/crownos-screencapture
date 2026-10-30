use std::collections::HashMap;

use sctk::compositor::{CompositorHandler, CompositorState};
use sctk::dispatch2::Dispatch2;
use sctk::foreign_toplevel_list::{ForeignToplevelList, ForeignToplevelListHandler};
use sctk::output::{OutputHandler, OutputState};
use sctk::registry::{ProvidesRegistryState, RegistryState};
use sctk::shell::wlr_layer::{LayerShell, LayerShellHandler, LayerSurface, LayerSurfaceConfigure};
use sctk::shm::{Shm, ShmHandler};
use sctk::{delegate_registry, registry_handlers};
use wayland_client::backend::ObjectId;
use wayland_client::globals::GlobalList;
use wayland_client::protocol::{wl_output::WlOutput, wl_surface::WlSurface};
use wayland_client::{Connection, Proxy, QueueHandle};
use wayland_protocols::ext::foreign_toplevel_list::v1::client::ext_foreign_toplevel_handle_v1::ExtForeignToplevelHandleV1;
use wayland_protocols::ext::image_capture_source::v1::client::{
    ext_foreign_toplevel_image_capture_source_manager_v1::ExtForeignToplevelImageCaptureSourceManagerV1,
    ext_output_image_capture_source_manager_v1::ExtOutputImageCaptureSourceManagerV1,
};
use wayland_protocols::ext::image_copy_capture::v1::client::ext_image_copy_capture_manager_v1::ExtImageCopyCaptureManagerV1;
use wayland_protocols::wp::linux_dmabuf::zv1::client::zwp_linux_dmabuf_v1::ZwpLinuxDmabufV1;

use super::session::{FrameStatus, SessionStatus};
use crate::capture::{CaptureError, OutputInfo, WindowInfo};

/// User data for objects whose events carry nothing this tool needs.
#[derive(Debug, Default, Clone, Copy)]
pub(super) struct Silent;

impl<Interface: Proxy> Dispatch2<Interface, State> for Silent {
    fn event(
        &self,
        _: &mut State,
        _: &Interface,
        _: Interface::Event,
        _: &Connection,
        _: &QueueHandle<State>,
    ) {
    }
}

/// The capture protocol globals.
pub(super) struct CaptureGlobals {
    pub manager: ExtImageCopyCaptureManagerV1,
    pub output_sources: ExtOutputImageCaptureSourceManagerV1,
    pub window_sources: Option<ExtForeignToplevelImageCaptureSourceManagerV1>,
    pub linux_dmabuf: Option<ZwpLinuxDmabufV1>,
}

impl CaptureGlobals {
    pub fn bind(globals: &GlobalList, queue: &QueueHandle<State>) -> Result<Self, CaptureError> {
        let manager = globals
            .bind(queue, 1..=1, Silent)
            .map_err(|_| CaptureError::Unsupported("ext-image-copy-capture-v1"))?;
        let output_sources = globals
            .bind(queue, 1..=1, Silent)
            .map_err(|_| CaptureError::Unsupported("ext-output-image-capture-source-manager-v1"))?;
        Ok(Self {
            manager,
            output_sources,
            window_sources: globals.bind(queue, 1..=1, Silent).ok(),
            linux_dmabuf: globals.bind(queue, 3..=5, Silent).ok(),
        })
    }
}

pub(super) struct State {
    registry: RegistryState,
    pub outputs: OutputState,
    pub toplevels: ForeignToplevelList,
    pub shm: Shm,
    pub compositor: CompositorState,
    pub layer_shell: Option<LayerShell>,
    pub session: SessionStatus,
    pub frames: HashMap<ObjectId, FrameStatus>,
    pub probe: ProbeStatus,
}

#[derive(Debug, Default)]
pub(super) enum ProbeStatus {
    #[default]
    Idle,
    AwaitingConfigure,
    Configured,
    Entered(WlOutput),
    Closed,
}

impl State {
    pub fn new(globals: &GlobalList, queue: &QueueHandle<Self>) -> Result<Self, CaptureError> {
        Ok(Self {
            registry: RegistryState::new(globals),
            outputs: OutputState::new(globals, queue),
            toplevels: ForeignToplevelList::new(globals, queue),
            shm: Shm::bind(globals, queue)?,
            compositor: CompositorState::bind(globals, queue)?,
            layer_shell: LayerShell::bind(globals, queue).ok(),
            session: SessionStatus::default(),
            frames: HashMap::new(),
            probe: ProbeStatus::default(),
        })
    }

    pub fn output_infos(&self) -> Vec<OutputInfo> {
        self.outputs
            .outputs()
            .filter_map(|output| self.output_info(&output))
            .collect()
    }

    pub fn output_info(&self, output: &WlOutput) -> Option<OutputInfo> {
        let info = self.outputs.info(output)?;
        let (width, height) = info.logical_size.unwrap_or_default();
        Some(OutputInfo {
            name: info.name?,
            description: info.description.unwrap_or_default(),
            logical_width: u32::try_from(width).ok()?,
            logical_height: u32::try_from(height).ok()?,
        })
    }

    pub fn wl_output(&self, name: &str) -> Option<WlOutput> {
        self.outputs.outputs().find(|output| {
            self.outputs
                .info(output)
                .and_then(|info| info.name)
                .as_deref()
                == Some(name)
        })
    }

    pub fn window_infos(&self) -> Vec<WindowInfo> {
        let list = &self.toplevels;
        list.toplevels()
            .iter()
            .filter_map(|handle| list.info(handle))
            .map(|info| WindowInfo {
                identifier: info.identifier,
                title: info.title,
                app_id: info.app_id,
            })
            .collect()
    }

    pub fn toplevel(&self, identifier: &str) -> Option<ExtForeignToplevelHandleV1> {
        let list = &self.toplevels;
        list.toplevels()
            .iter()
            .find(|handle| {
                list.info(handle)
                    .is_some_and(|info| info.identifier == identifier)
            })
            .cloned()
    }
}

impl ProvidesRegistryState for State {
    fn registry(&mut self) -> &mut RegistryState {
        &mut self.registry
    }

    registry_handlers![OutputState];
}

impl OutputHandler for State {
    fn output_state(&mut self) -> &mut OutputState {
        &mut self.outputs
    }

    fn new_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: WlOutput) {}

    fn update_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: WlOutput) {}

    fn output_destroyed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: WlOutput) {}
}

impl ForeignToplevelListHandler for State {
    fn foreign_toplevel_list_state(&mut self) -> &mut ForeignToplevelList {
        &mut self.toplevels
    }

    fn new_toplevel(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: ExtForeignToplevelHandleV1,
    ) {
    }

    fn update_toplevel(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: ExtForeignToplevelHandleV1,
    ) {
    }

    fn toplevel_closed(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: ExtForeignToplevelHandleV1,
    ) {
    }
}

impl ShmHandler for State {
    fn shm_state(&mut self) -> &mut Shm {
        &mut self.shm
    }
}

impl CompositorHandler for State {
    fn scale_factor_changed(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &WlSurface,
        _: i32,
    ) {
    }

    fn transform_changed(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &WlSurface,
        _: wayland_client::protocol::wl_output::Transform,
    ) {
    }

    fn frame(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlSurface, _: u32) {}

    fn surface_enter(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &WlSurface,
        output: &WlOutput,
    ) {
        self.probe = ProbeStatus::Entered(output.clone());
    }

    fn surface_leave(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &WlSurface,
        _: &WlOutput,
    ) {
    }
}

impl LayerShellHandler for State {
    fn closed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &LayerSurface) {
        self.probe = ProbeStatus::Closed;
    }

    fn configure(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &LayerSurface,
        _: LayerSurfaceConfigure,
        _: u32,
    ) {
        if matches!(self.probe, ProbeStatus::AwaitingConfigure) {
            self.probe = ProbeStatus::Configured;
        }
    }
}

delegate_registry!(State);
sctk::delegate_dispatch2!(State);
