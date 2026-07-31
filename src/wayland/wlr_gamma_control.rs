use std::collections::HashMap;
use std::fs::File;
use std::io::Read;

use smithay::output::Output;
use smithay::reexports::wayland_server::backend::{ClientId, GlobalId};
use smithay::reexports::wayland_server::{
    Client, DataInit, Dispatch, DisplayHandle, GlobalDispatch, New,
};
use wayland_protocols_wlr::gamma_control::v1::server::{
    zwlr_gamma_control_manager_v1::{self, ZwlrGammaControlManagerV1},
    zwlr_gamma_control_v1::{self, ZwlrGammaControlV1},
};

use crate::session::{Session, SessionDriver};

const VERSION: u32 = 1;

pub struct GlobalData {
    enabled: bool,
}

pub struct State {
    _global: GlobalId,
    controls: HashMap<Output, ZwlrGammaControlV1>,
}

#[derive(Debug)]
pub struct ControlData {
    gamma_size: u32,
}

impl State {
    pub fn new<D>(display: &DisplayHandle, enabled: bool) -> Self
    where
        D: GlobalDispatch<ZwlrGammaControlManagerV1, GlobalData> + 'static,
    {
        Self {
            _global: display
                .create_global::<D, ZwlrGammaControlManagerV1, _>(VERSION, GlobalData { enabled }),
            controls: HashMap::new(),
        }
    }

    pub fn output_disabled(&mut self, output: &Output) -> bool {
        self.controls
            .remove(output)
            .inspect(|control| control.failed())
            .is_some()
    }
}

impl<D: SessionDriver> GlobalDispatch<ZwlrGammaControlManagerV1, GlobalData, Session<D>>
    for Session<D>
{
    fn bind(
        _session: &mut Session<D>,
        _display: &DisplayHandle,
        _client: &Client,
        resource: New<ZwlrGammaControlManagerV1>,
        _global_data: &GlobalData,
        data_init: &mut DataInit<'_, Session<D>>,
    ) {
        data_init.init(resource, ());
    }

    fn can_view(_client: Client, global_data: &GlobalData) -> bool {
        global_data.enabled
    }
}

impl<D: SessionDriver> Dispatch<ZwlrGammaControlManagerV1, (), Session<D>> for Session<D> {
    fn request(
        session: &mut Session<D>,
        _client: &Client,
        _manager: &ZwlrGammaControlManagerV1,
        request: zwlr_gamma_control_manager_v1::Request,
        _data: &(),
        _display: &DisplayHandle,
        data_init: &mut DataInit<'_, Session<D>>,
    ) {
        match request {
            zwlr_gamma_control_manager_v1::Request::GetGammaControl { id, output } => {
                let output = Output::from_resource(&output);
                let gamma_size = output
                    .as_ref()
                    .filter(|output| {
                        !session
                            .wayland
                            .wlr_gamma_control_state
                            .controls
                            .contains_key(*output)
                    })
                    .and_then(|output| session.driver.gamma_size(output).ok());
                let control = data_init.init(
                    id,
                    ControlData {
                        gamma_size: gamma_size.unwrap_or(0),
                    },
                );
                if let (Some(output), Some(gamma_size)) = (output, gamma_size) {
                    control.gamma_size(gamma_size);
                    session
                        .wayland
                        .wlr_gamma_control_state
                        .controls
                        .insert(output, control);
                } else {
                    control.failed();
                }
            }
            zwlr_gamma_control_manager_v1::Request::Destroy => {}
            _ => unreachable!(),
        }
    }
}

impl<D: SessionDriver> Dispatch<ZwlrGammaControlV1, ControlData, Session<D>> for Session<D> {
    fn request(
        session: &mut Session<D>,
        _client: &Client,
        resource: &ZwlrGammaControlV1,
        request: zwlr_gamma_control_v1::Request,
        data: &ControlData,
        _display: &DisplayHandle,
        _data_init: &mut DataInit<'_, Session<D>>,
    ) {
        match request {
            zwlr_gamma_control_v1::Request::SetGamma { fd } => {
                let output = session
                    .wayland
                    .wlr_gamma_control_state
                    .controls
                    .iter()
                    .find_map(|(output, control)| (control == resource).then(|| output.clone()));
                let Some(output) = output else {
                    return;
                };
                let ramp = read_ramp(File::from(fd), data.gamma_size);
                let result = ramp.and_then(|ramp| session.driver.set_gamma(&output, Some(ramp)));
                if let Err(err) = result {
                    eventline::warn!("gamma control for {:?} failed: {err}", output.name());
                    resource.failed();
                    session
                        .wayland
                        .wlr_gamma_control_state
                        .controls
                        .remove(&output);
                    let _ = session.driver.set_gamma(&output, None);
                }
            }
            zwlr_gamma_control_v1::Request::Destroy => {}
            _ => unreachable!(),
        }
    }

    fn destroyed(
        session: &mut Session<D>,
        _client: ClientId,
        resource: &ZwlrGammaControlV1,
        _data: &ControlData,
    ) {
        let output = session
            .wayland
            .wlr_gamma_control_state
            .controls
            .iter()
            .find_map(|(output, control)| (control == resource).then(|| output.clone()));
        if let Some(output) = output {
            session
                .wayland
                .wlr_gamma_control_state
                .controls
                .remove(&output);
            if let Err(err) = session.driver.set_gamma(&output, None) {
                eventline::warn!("failed to reset gamma for {:?}: {err}", output.name());
            }
        }
    }
}

fn read_ramp(mut file: impl Read, gamma_size: u32) -> Result<Vec<u16>, String> {
    let entries = usize::try_from(gamma_size)
        .ok()
        .and_then(|size| size.checked_mul(3))
        .ok_or_else(|| "gamma ramp size overflow".to_string())?;
    let byte_len = entries
        .checked_mul(std::mem::size_of::<u16>())
        .ok_or_else(|| "gamma ramp byte size overflow".to_string())?;
    let mut bytes = vec![0; byte_len];
    file.read_exact(&mut bytes)
        .map_err(|err| format!("gamma ramp is shorter than advertised: {err}"))?;
    let mut trailing = [0];
    match file.read(&mut trailing) {
        Ok(0) => {}
        Ok(_) => return Err("gamma ramp contains trailing data".into()),
        Err(err) => return Err(format!("failed to validate gamma ramp length: {err}")),
    }
    Ok(bytes
        .chunks_exact(2)
        .map(|bytes| u16::from_ne_bytes([bytes[0], bytes[1]]))
        .collect())
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use super::read_ramp;

    #[test]
    fn ramp_parser_preserves_native_channel_order() {
        let values = [1u16, 2, 3, 4, 5, 6];
        let bytes = values
            .iter()
            .flat_map(|value| value.to_ne_bytes())
            .collect::<Vec<_>>();
        assert_eq!(read_ramp(Cursor::new(bytes), 2).unwrap(), values);
    }

    #[test]
    fn ramp_parser_rejects_short_and_trailing_data() {
        assert!(read_ramp(Cursor::new([0; 11]), 2).is_err());
        assert!(read_ramp(Cursor::new([0; 13]), 2).is_err());
    }
}
