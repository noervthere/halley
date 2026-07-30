//! Read-only implementation of wlr-output-management.
//!
//! Desktop shells use this protocol to discover connector modes and logical
//! layout. Halley's actual output policy remains configuration-file driven, so
//! apply/test requests fail explicitly instead of pretending a mutation took
//! effect.

use smithay::output::{Mode, Output};
use smithay::reexports::wayland_server::backend::GlobalId;
use smithay::reexports::wayland_server::protocol::wl_output;
use smithay::reexports::wayland_server::{
    Client, DataInit, Dispatch, DisplayHandle, GlobalDispatch, New, Resource,
};
use wayland_protocols_wlr::output_management::v1::server::{
    zwlr_output_configuration_head_v1::{self, ZwlrOutputConfigurationHeadV1},
    zwlr_output_configuration_v1::{self, ZwlrOutputConfigurationV1},
    zwlr_output_head_v1::{self, ZwlrOutputHeadV1},
    zwlr_output_manager_v1::{self, ZwlrOutputManagerV1},
    zwlr_output_mode_v1::{self, ZwlrOutputModeV1},
};

use crate::session::{Session, SessionDriver};

const VERSION: u32 = 4;
const SERIAL: u32 = 1;

#[derive(Debug)]
pub struct State {
    _global: GlobalId,
}

impl State {
    pub fn new<D>(display: &DisplayHandle) -> Self
    where
        D: GlobalDispatch<ZwlrOutputManagerV1, ()> + 'static,
    {
        Self {
            _global: display.create_global::<D, ZwlrOutputManagerV1, _>(VERSION, ()),
        }
    }
}

#[derive(Debug)]
pub struct HeadData {
    _output: Output,
}

#[derive(Debug)]
pub struct ModeData {
    _output: Output,
    _mode: Mode,
}

#[derive(Debug)]
pub struct ConfigurationData {
    serial: u32,
}

#[derive(Debug)]
pub struct ConfigurationHeadData;

impl<D: SessionDriver> GlobalDispatch<ZwlrOutputManagerV1, (), Session<D>> for Session<D> {
    fn bind(
        session: &mut Session<D>,
        display: &DisplayHandle,
        client: &Client,
        resource: New<ZwlrOutputManagerV1>,
        _global_data: &(),
        data_init: &mut DataInit<'_, Session<D>>,
    ) {
        let manager = data_init.init(resource, ());
        let output_info = crate::ipc::OutputInfoSource::output_info(&session.driver);
        let outputs: Vec<_> = session.wayland.space.outputs().cloned().collect();
        for output in outputs {
            let adaptive_sync = output_info
                .iter()
                .find(|info| info.name == output.name())
                .is_some_and(|info| info.vrr_active);
            advertise_output::<D>(display, client, &manager, output, adaptive_sync);
        }
        manager.done(SERIAL);
    }
}

fn advertise_output<D: SessionDriver>(
    display: &DisplayHandle,
    client: &Client,
    manager: &ZwlrOutputManagerV1,
    output: Output,
    adaptive_sync: bool,
) {
    let Ok(head) = client.create_resource::<ZwlrOutputHeadV1, _, Session<D>>(
        display,
        manager.version(),
        HeadData {
            _output: output.clone(),
        },
    ) else {
        return;
    };
    manager.head(&head);

    head.name(output.name());
    head.description(output.description());

    let physical = output.physical_properties();
    if physical.size.w > 0 && physical.size.h > 0 {
        head.physical_size(physical.size.w, physical.size.h);
    }
    if head.version() >= 2 {
        if !physical.make.is_empty() {
            head.make(physical.make);
        }
        if !physical.model.is_empty() {
            head.model(physical.model);
        }
        if !physical.serial_number.is_empty() {
            head.serial_number(physical.serial_number);
        }
    }

    let mut current = None;
    for mode in output.modes() {
        let Ok(resource) = client.create_resource::<ZwlrOutputModeV1, _, Session<D>>(
            display,
            head.version().min(3),
            ModeData {
                _output: output.clone(),
                _mode: mode,
            },
        ) else {
            continue;
        };
        head.mode(&resource);
        resource.size(mode.size.w, mode.size.h);
        if mode.refresh > 0 {
            resource.refresh(mode.refresh);
        }
        if output.preferred_mode() == Some(mode) {
            resource.preferred();
        }
        if output.current_mode() == Some(mode) {
            current = Some(resource);
        }
    }

    let enabled = output.current_mode().is_some();
    head.enabled(i32::from(enabled));
    if enabled {
        if let Some(current) = current.as_ref() {
            head.current_mode(current);
        }
        let location = output.current_location();
        head.position(location.x, location.y);
        head.transform(wl_transform(output.current_transform()));
        head.scale(output.current_scale().fractional_scale());
        if head.version() >= 4 {
            head.adaptive_sync(if adaptive_sync {
                zwlr_output_head_v1::AdaptiveSyncState::Enabled
            } else {
                zwlr_output_head_v1::AdaptiveSyncState::Disabled
            });
        }
    }
}

fn wl_transform(transform: smithay::utils::Transform) -> wl_output::Transform {
    match transform {
        smithay::utils::Transform::Normal => wl_output::Transform::Normal,
        smithay::utils::Transform::_90 => wl_output::Transform::_90,
        smithay::utils::Transform::_180 => wl_output::Transform::_180,
        smithay::utils::Transform::_270 => wl_output::Transform::_270,
        smithay::utils::Transform::Flipped => wl_output::Transform::Flipped,
        smithay::utils::Transform::Flipped90 => wl_output::Transform::Flipped90,
        smithay::utils::Transform::Flipped180 => wl_output::Transform::Flipped180,
        smithay::utils::Transform::Flipped270 => wl_output::Transform::Flipped270,
    }
}

impl<D: SessionDriver> Dispatch<ZwlrOutputManagerV1, (), Session<D>> for Session<D> {
    fn request(
        _session: &mut Session<D>,
        _client: &Client,
        manager: &ZwlrOutputManagerV1,
        request: zwlr_output_manager_v1::Request,
        _data: &(),
        _display: &DisplayHandle,
        data_init: &mut DataInit<'_, Session<D>>,
    ) {
        match request {
            zwlr_output_manager_v1::Request::CreateConfiguration { id, serial } => {
                data_init.init(id, ConfigurationData { serial });
            }
            zwlr_output_manager_v1::Request::Stop => manager.finished(),
            _ => unreachable!(),
        }
    }
}

impl<D: SessionDriver> Dispatch<ZwlrOutputHeadV1, HeadData, Session<D>> for Session<D> {
    fn request(
        _session: &mut Session<D>,
        _client: &Client,
        _head: &ZwlrOutputHeadV1,
        request: zwlr_output_head_v1::Request,
        _data: &HeadData,
        _display: &DisplayHandle,
        _data_init: &mut DataInit<'_, Session<D>>,
    ) {
        match request {
            zwlr_output_head_v1::Request::Release => {}
            _ => unreachable!(),
        }
    }
}

impl<D: SessionDriver> Dispatch<ZwlrOutputModeV1, ModeData, Session<D>> for Session<D> {
    fn request(
        _session: &mut Session<D>,
        _client: &Client,
        _mode: &ZwlrOutputModeV1,
        request: zwlr_output_mode_v1::Request,
        _data: &ModeData,
        _display: &DisplayHandle,
        _data_init: &mut DataInit<'_, Session<D>>,
    ) {
        match request {
            zwlr_output_mode_v1::Request::Release => {}
            _ => unreachable!(),
        }
    }
}

impl<D: SessionDriver> Dispatch<ZwlrOutputConfigurationV1, ConfigurationData, Session<D>>
    for Session<D>
{
    fn request(
        _session: &mut Session<D>,
        _client: &Client,
        configuration: &ZwlrOutputConfigurationV1,
        request: zwlr_output_configuration_v1::Request,
        data: &ConfigurationData,
        _display: &DisplayHandle,
        data_init: &mut DataInit<'_, Session<D>>,
    ) {
        match request {
            zwlr_output_configuration_v1::Request::EnableHead { id, .. } => {
                data_init.init(id, ConfigurationHeadData);
            }
            zwlr_output_configuration_v1::Request::DisableHead { .. } => {}
            zwlr_output_configuration_v1::Request::Apply
            | zwlr_output_configuration_v1::Request::Test => {
                if data.serial == SERIAL {
                    configuration.failed();
                } else {
                    configuration.cancelled();
                }
            }
            zwlr_output_configuration_v1::Request::Destroy => {}
            _ => unreachable!(),
        }
    }
}

impl<D: SessionDriver> Dispatch<ZwlrOutputConfigurationHeadV1, ConfigurationHeadData, Session<D>>
    for Session<D>
{
    fn request(
        _session: &mut Session<D>,
        _client: &Client,
        _configuration: &ZwlrOutputConfigurationHeadV1,
        _request: zwlr_output_configuration_head_v1::Request,
        _data: &ConfigurationHeadData,
        _display: &DisplayHandle,
        _data_init: &mut DataInit<'_, Session<D>>,
    ) {
    }
}
