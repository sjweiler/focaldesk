use smithay::backend::allocator::dmabuf::Dmabuf;
use smithay::backend::allocator::{Buffer, Fourcc, Modifier};
use smithay::backend::renderer::ImportDma;
use smithay::wayland::dmabuf::{DmabufGlobal, DmabufHandler, DmabufState, ImportNotifier};

use crate::core::desktop::DesktopState;
use focaldesk_logging::flog;
use std::sync::atomic::{AtomicUsize, Ordering};

static DMABUF_IMPORT_LOGS: AtomicUsize = AtomicUsize::new(0);

impl DmabufHandler for DesktopState {
    fn dmabuf_state(&mut self) -> &mut DmabufState {
        &mut self.dmabuf_state
    }

    fn dmabuf_imported(
        &mut self,
        _global: &DmabufGlobal,
        dmabuf: Dmabuf,
        notifier: ImportNotifier,
    ) {
        if let Some(node) = self.dmabuf_node {
            dmabuf.set_node(node);
        }
        let seq = DMABUF_IMPORT_LOGS.fetch_add(1, Ordering::Relaxed);

        // Validate the format is importable, but do not create a GLES texture here.
        // Early import during the params protocol races XWayland's first GPU write and
        // pins a stale EGLImage in the renderer dmabuf cache. Textures are imported when
        // building render elements on the bound frame instead.
        let Some(ctx) = self.portal_dispatch_ctx.as_mut() else {
            // The nested wgpu backend currently advertises only the formats it
            // can synchronize, linearly map, and upload. Its global is created
            // by that backend, so this branch remains unreachable for nested
            // backends that do not advertise linux-dmabuf.
            let format = dmabuf.format();
            if self.backend_kind == focaldesk_flow::keybinds::BackendKind::Winit
                && dmabuf.num_planes() == 1
                && format.modifier == Modifier::Linear
                && matches!(format.code, Fourcc::Argb8888 | Fourcc::Xrgb8888)
            {
                if seq < 200 {
                    flog(format!(
                        "linux-dmabuf accepted by nested wgpu format={format:?}"
                    ));
                }
                let _ = notifier.successful::<DesktopState>();
            } else {
                flog("linux-dmabuf: no compatible renderer during params import");
                notifier.failed();
            }
            return;
        };

        // SAFETY: `portal_dispatch_ctx` is set immediately around `dispatch_clients` and cleared
        // after dispatch returns. `dmabuf_imported` runs synchronously from that dispatch path.
        let renderer = unsafe { &mut *ctx.renderer.as_ptr() };
        let format = dmabuf.format();
        if renderer.has_dmabuf_format(format) {
            if seq < 200 {
                flog(format!(
                    "linux-dmabuf accepted format={:?} planes={} y_inverted={}",
                    format,
                    dmabuf.num_planes(),
                    dmabuf.y_inverted()
                ));
            }
            let _ = notifier.successful::<DesktopState>();
        } else {
            flog(format!(
                "linux-dmabuf rejected unsupported format={:?} planes={} y_inverted={}",
                format,
                dmabuf.num_planes(),
                dmabuf.y_inverted()
            ));
            notifier.failed();
        }
    }
}
