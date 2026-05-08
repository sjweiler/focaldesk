use smithay::reexports::drm::control::{
    self as drm,
    Device as ControlDevice,
};

pub fn dump_drm_topology<D: ControlDevice>(drm: &D, node_path: &str) -> anyhow::Result<()> {
    flog("========== DRM TOPOLOGY ==========");

    let res = drm.resource_handles()?;

    flog(&format!("DRM node: {}", node_path));

    // --- CRTCs ---
    flog("CRTCs:");
    for crtc in res.crtcs() {
        flog(&format!("  CRTC: {:?}", crtc));
    }

    // --- Connectors ---
    flog("Connectors:");
    for conn in res.connectors() {
        let info = drm.get_connector(*conn, true)?;

        let name = format!(
            "{}-{}",
            info.interface().as_str(),
            info.interface_id()
        );

        flog(&format!(
            "  Connector: {} ({:?}) state={:?} size_mm={}x{}",
            name,
            info.handle(),
            info.state(),
            info.size().0,
            info.size().1,
        ));

        flog(&format!("    encoders: {:?}", info.encoders()));
        flog(&format!("    current_encoder: {:?}", info.current_encoder()));

        // Modes
        for mode in info.modes() {
            flog(&format!(
                "    mode: {}x{} @ {}Hz flags={:?} type={:?}",
                mode.size().0,
                mode.size().1,
                mode.vrefresh(),
                mode.flags(),
                mode.mode_type(),
            ));
        }
    }

    // --- Encoders ---
    flog("Encoders:");
    for enc in res.encoders() {
        let info = drm.get_encoder(*enc)?;

        flog(&format!(
            "  Encoder {:?}: crtc={:?} possible_crtcs={:?}",
            enc,
            info.crtc(),
            info.possible_crtcs(),
        ));
    }

    // --- Planes (optional but VERY useful later) ---
    if let Ok(planes) = drm.plane_handles() {
        flog("Planes:");
        for plane in planes.planes() {
            if let Ok(info) = drm.get_plane(*plane) {
                flog(&format!(
                    "  Plane {:?}: crtc={:?} formats={:?}",
                    plane,
                    info.crtc(),
                    info.formats(),
                ));
            }
        }
    }

    flog("==================================");

    Ok(())
}
