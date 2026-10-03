//! Compatibility implementation of Valve's `frog-color-management-v1`.

use std::sync::Mutex;

use crate::core::color::{
    ColorDescription, ColorPrimaries, RenderingIntent, SurfaceColorState, TransferFunction,
};
use crate::core::desktop::DesktopState;
use smithay::wayland::compositor::with_states;
use wayland_server::protocol::wl_surface::WlSurface;
use wayland_server::{
    Client, DataInit, Dispatch, DisplayHandle, GlobalDispatch, New, Resource, WEnum,
};

mod generated {
    #![allow(dead_code, non_camel_case_types, unused_unsafe, unused_variables)]
    #![allow(non_upper_case_globals, non_snake_case, unused_imports)]
    #![allow(missing_docs, clippy::all)]

    pub mod server {
        use wayland_server;
        use wayland_server::protocol::*;

        pub mod __interfaces {
            use wayland_server::protocol::__interfaces::*;
            wayland_scanner::generate_interfaces!("protocols/frog-color-management-v1.xml");
        }
        use self::__interfaces::*;

        wayland_scanner::generate_server_code!("protocols/frog-color-management-v1.xml");
    }
}

use generated::server::{frog_color_managed_surface, frog_color_management_factory_v1};

pub struct FrogColorManagementState;

impl FrogColorManagementState {
    pub fn bind_global<D>(display: &DisplayHandle)
    where
        D: GlobalDispatch<frog_color_management_factory_v1::FrogColorManagementFactoryV1, ()>
            + Dispatch<frog_color_management_factory_v1::FrogColorManagementFactoryV1, ()>
            + Dispatch<frog_color_managed_surface::FrogColorManagedSurface, FrogSurface>
            + 'static,
    {
        display
            .create_global::<D, frog_color_management_factory_v1::FrogColorManagementFactoryV1, _>(
                1,
                (),
            );
    }
}

#[derive(Clone, Copy, Debug, Default)]
struct FrogPendingColor {
    transfer: Option<TransferFunction>,
    primaries: Option<ColorPrimaries>,
    max_luminance_nits: Option<f32>,
    max_cll_nits: Option<f32>,
    max_fall_nits: Option<f32>,
}

impl FrogPendingColor {
    fn description(self) -> ColorDescription {
        let transfer = self.transfer.unwrap_or(TransferFunction::Srgb);
        let primaries = self.primaries.unwrap_or(ColorPrimaries::Srgb);
        let hdr = matches!(
            transfer,
            TransferFunction::St2084Pq | TransferFunction::Linear
        );
        let max_luminance_nits = self
            .max_luminance_nits
            .unwrap_or(if hdr { 1_000.0 } else { 80.0 })
            .clamp(1.0, 10_000.0);
        ColorDescription {
            primaries,
            transfer,
            reference_white_nits: if hdr { 203.0 } else { 80.0 },
            max_luminance_nits,
            max_cll_nits: self.max_cll_nits,
            max_fall_nits: self.max_fall_nits,
            windows_scrgb_stimulus: transfer == TransferFunction::Linear
                && primaries == ColorPrimaries::Srgb,
        }
    }
}

pub struct FrogSurface {
    surface: WlSurface,
    pending: Mutex<FrogPendingColor>,
}

impl FrogSurface {
    fn update(&self, update: impl FnOnce(&mut FrogPendingColor)) {
        let Ok(mut pending) = self.pending.lock() else {
            return;
        };
        update(&mut pending);
        let description = pending.description();
        with_states(&self.surface, |states| {
            let mut color = states.cached_state.get::<SurfaceColorState>();
            color.pending().description = Some(description);
            color.pending().intent = RenderingIntent::Perceptual;
        });
    }

    fn unset(&self) {
        if !self.surface.is_alive() {
            return;
        }
        with_states(&self.surface, |states| {
            states
                .cached_state
                .get::<SurfaceColorState>()
                .pending()
                .description = None;
        });
    }
}

impl GlobalDispatch<frog_color_management_factory_v1::FrogColorManagementFactoryV1, ()>
    for DesktopState
{
    fn bind(
        _state: &mut Self,
        _handle: &DisplayHandle,
        _client: &Client,
        resource: New<frog_color_management_factory_v1::FrogColorManagementFactoryV1>,
        _global_data: &(),
        data_init: &mut DataInit<'_, Self>,
    ) {
        data_init.init(resource, ());
    }
}

impl Dispatch<frog_color_management_factory_v1::FrogColorManagementFactoryV1, ()> for DesktopState {
    fn request(
        state: &mut Self,
        _client: &Client,
        _resource: &frog_color_management_factory_v1::FrogColorManagementFactoryV1,
        request: frog_color_management_factory_v1::Request,
        _data: &(),
        _dh: &DisplayHandle,
        data_init: &mut DataInit<'_, Self>,
    ) {
        match request {
            frog_color_management_factory_v1::Request::Destroy => {}
            frog_color_management_factory_v1::Request::GetColorManagedSurface {
                surface,
                callback,
            } => {
                let object = data_init.init(
                    callback,
                    FrogSurface {
                        surface: surface.clone(),
                        pending: Mutex::new(FrogPendingColor::default()),
                    },
                );
                send_preferred_metadata(state, &surface, &object);
            }
            _ => {}
        }
    }
}

impl Dispatch<frog_color_managed_surface::FrogColorManagedSurface, FrogSurface> for DesktopState {
    fn request(
        _state: &mut Self,
        _client: &Client,
        _resource: &frog_color_managed_surface::FrogColorManagedSurface,
        request: frog_color_managed_surface::Request,
        data: &FrogSurface,
        _dh: &DisplayHandle,
        _data_init: &mut DataInit<'_, Self>,
    ) {
        match request {
            frog_color_managed_surface::Request::Destroy => data.unset(),
            frog_color_managed_surface::Request::SetKnownTransferFunction { transfer_function } => {
                let transfer = match transfer_function {
                    WEnum::Value(frog_color_managed_surface::TransferFunction::Srgb) => {
                        Some(TransferFunction::Srgb)
                    }
                    WEnum::Value(frog_color_managed_surface::TransferFunction::Gamma22) => {
                        Some(TransferFunction::Gamma22)
                    }
                    WEnum::Value(frog_color_managed_surface::TransferFunction::St2084Pq) => {
                        Some(TransferFunction::St2084Pq)
                    }
                    WEnum::Value(frog_color_managed_surface::TransferFunction::ScrgbLinear) => {
                        Some(TransferFunction::Linear)
                    }
                    _ => None,
                };
                data.update(|pending| pending.transfer = transfer);
            }
            frog_color_managed_surface::Request::SetKnownContainerColorVolume { primaries } => {
                let primaries = match primaries {
                    WEnum::Value(frog_color_managed_surface::Primaries::Rec709) => {
                        Some(ColorPrimaries::Srgb)
                    }
                    WEnum::Value(frog_color_managed_surface::Primaries::Rec2020) => {
                        Some(ColorPrimaries::Bt2020)
                    }
                    _ => None,
                };
                data.update(|pending| pending.primaries = primaries);
            }
            frog_color_managed_surface::Request::SetRenderIntent { .. } => {}
            frog_color_managed_surface::Request::SetHdrMetadata {
                max_display_mastering_luminance,
                max_cll,
                max_fall,
                ..
            } => data.update(|pending| {
                pending.max_luminance_nits = nonzero_nits(max_display_mastering_luminance);
                pending.max_cll_nits = nonzero_nits(max_cll);
                pending.max_fall_nits = nonzero_nits(max_fall);
            }),
            _ => {}
        }
    }

    fn destroyed(
        _state: &mut Self,
        _client: wayland_server::backend::ClientId,
        _resource: &frog_color_managed_surface::FrogColorManagedSurface,
        data: &FrogSurface,
    ) {
        data.unset();
    }
}

fn nonzero_nits(value: u32) -> Option<f32> {
    (value > 0).then_some((value as f32).min(10_000.0))
}

fn chromaticity(value: f32) -> u32 {
    (value.clamp(0.0, 1.0) * 50_000.0).round() as u32
}

fn send_preferred_metadata(
    state: &DesktopState,
    surface: &WlSurface,
    object: &frog_color_managed_surface::FrogColorManagedSurface,
) {
    let output_id = state.preferred_output_id_for_surface(surface);
    let Some(output) = state.outputs.get(&output_id) else {
        return;
    };
    let hdr = output.hdr_kms_applied;
    let primaries = if hdr {
        ColorPrimaries::Bt2020.chromaticity()
    } else {
        output.color_description.primaries.chromaticity()
    };
    let appearance = output.hdr_appearance.validate().unwrap_or_default();
    object.preferred_metadata(
        if hdr {
            frog_color_managed_surface::TransferFunction::St2084Pq
        } else {
            frog_color_managed_surface::TransferFunction::Srgb
        },
        chromaticity(primaries.r[0]),
        chromaticity(primaries.r[1]),
        chromaticity(primaries.g[0]),
        chromaticity(primaries.g[1]),
        chromaticity(primaries.b[0]),
        chromaticity(primaries.b[1]),
        chromaticity(primaries.w[0]),
        chromaticity(primaries.w[1]),
        if hdr {
            appearance.peak_nits.round() as u32
        } else {
            80
        },
        if hdr {
            (appearance.black_level_nits * 10_000.0).round() as u32
        } else {
            0
        },
        if hdr {
            appearance.full_frame_peak_nits.round() as u32
        } else {
            80
        },
    );
}

#[cfg(test)]
mod tests {
    use super::FrogPendingColor;
    use crate::core::color::{ColorPrimaries, TransferFunction};

    #[test]
    fn frog_pq_metadata_maps_to_native_hdr_description() {
        let description = FrogPendingColor {
            transfer: Some(TransferFunction::St2084Pq),
            primaries: Some(ColorPrimaries::Bt2020),
            max_luminance_nits: Some(1_000.0),
            max_cll_nits: Some(800.0),
            max_fall_nits: Some(400.0),
        }
        .description();
        assert_eq!(description.transfer, TransferFunction::St2084Pq);
        assert_eq!(description.primaries, ColorPrimaries::Bt2020);
        assert_eq!(description.max_cll_nits, Some(800.0));
        assert!(!description.windows_scrgb_stimulus);
    }

    #[test]
    fn frog_scrgb_preserves_extended_linear_stimulus() {
        let description = FrogPendingColor {
            transfer: Some(TransferFunction::Linear),
            primaries: Some(ColorPrimaries::Srgb),
            ..Default::default()
        }
        .description();
        assert!(description.windows_scrgb_stimulus);
        assert_eq!(description.reference_white_nits, 203.0);
    }
}
