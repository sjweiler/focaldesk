use crate::core::wayland::client::ClientState;
use flowstate_logging::flog;
use smithay::backend::renderer::buffer_type;
use smithay::backend::renderer::utils::on_commit_buffer_handler;
use smithay::delegate_compositor;
use smithay::reexports::calloop::Interest;
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::reexports::wayland_server::Resource;
use smithay::wayland::compositor::{
    add_blocker, add_pre_commit_hook, with_states, BufferAssignment, CompositorClientState,
    SurfaceAttributes,
};
use smithay::wayland::compositor::{CompositorHandler, CompositorState as SmithayCompositorState};
use smithay::wayland::dmabuf::get_dmabuf;
#[cfg(feature = "xwayland")]
use smithay::xwayland::XWaylandClientData;
use std::sync::atomic::{AtomicUsize, Ordering};

use smithay::desktop::layer_map_for_output;

use crate::core::desktop::DesktopState;
use smithay::reexports::wayland_server::Client;

static XWAYLAND_BUFFER_LOGS: AtomicUsize = AtomicUsize::new(0);
static XWAYLAND_BLOCKER_LOGS: AtomicUsize = AtomicUsize::new(0);

impl CompositorHandler for DesktopState {
    fn compositor_state(&mut self) -> &mut SmithayCompositorState {
        &mut self.compositor_state
    }
    fn client_compositor_state<'a>(&self, client: &'a Client) -> &'a CompositorClientState {
        #[cfg(feature = "xwayland")]
        if let Some(state) = client.get_data::<XWaylandClientData>() {
            return &state.compositor_state;
        }
        &client
            .get_data::<ClientState>()
            .expect("unknown Wayland client data type")
            .compositor_state
    }

    fn new_surface(&mut self, surface: &WlSurface) {
        add_pre_commit_hook::<DesktopState, _>(surface, |state, _dh, surface| {
            let maybe_dmabuf = with_states(surface, |states| {
                states
                    .cached_state
                    .get::<SurfaceAttributes>()
                    .pending()
                    .buffer
                    .as_ref()
                    .and_then(|assignment| match assignment {
                        BufferAssignment::NewBuffer(buffer) => get_dmabuf(buffer).cloned().ok(),
                        _ => None,
                    })
            });

            if let Some(dmabuf) = maybe_dmabuf {
                #[cfg(feature = "xwayland")]
                if let (Some(client), Some(handle)) =
                    (surface.client(), state.xwayland_loop_handle.clone())
                {
                    if let Ok((blocker, source)) = dmabuf.generate_blocker(Interest::READ) {
                        let res = handle.insert_source(source, move |_, _, data| {
                            let seq = XWAYLAND_BLOCKER_LOGS.fetch_add(1, Ordering::Relaxed);
                            if seq < 100 {
                                flog("XWayland dmabuf read blocker cleared");
                            }
                            let dh = data.display_handle.clone();
                            data.client_compositor_state(&client)
                                .blocker_cleared(data, &dh);
                            Ok(())
                        });
                        if res.is_ok() {
                            let seq = XWAYLAND_BLOCKER_LOGS.fetch_add(1, Ordering::Relaxed);
                            if seq < 100 {
                                flog("XWayland dmabuf read blocker added");
                            }
                            add_blocker(surface, blocker);
                        } else {
                            let seq = XWAYLAND_BLOCKER_LOGS.fetch_add(1, Ordering::Relaxed);
                            if seq < 100 {
                                flog("XWayland dmabuf read blocker source insert failed");
                            }
                        }
                    } else {
                        let seq = XWAYLAND_BLOCKER_LOGS.fetch_add(1, Ordering::Relaxed);
                        if seq < 100 {
                            flog("XWayland dmabuf read blocker already ready");
                        }
                    }
                }
            }

            #[cfg(feature = "xwayland")]
            {
                let Some(client) = surface.client() else {
                    return;
                };
                if client.get_data::<XWaylandClientData>().is_none() {
                    return;
                }

                let seq = XWAYLAND_BUFFER_LOGS.fetch_add(1, Ordering::Relaxed);
                if seq >= 200 {
                    return;
                }

                let buffer_kind = with_states(surface, |states| {
                    states
                        .cached_state
                        .get::<SurfaceAttributes>()
                        .pending()
                        .buffer
                        .as_ref()
                        .map(|assignment| match assignment {
                            BufferAssignment::Removed => "removed".to_string(),
                            BufferAssignment::NewBuffer(buffer) => {
                                format!("{:?}", buffer_type(buffer))
                            }
                        })
                        .unwrap_or_else(|| "unchanged".to_string())
                });
                flog(&format!("XWayland pre-commit buffer={buffer_kind}"));
            }
        });
    }

    fn commit(&mut self, surface: &WlSurface) {
        on_commit_buffer_handler::<DesktopState>(surface);
        // Import deferred to the render path (`import_mapped_surfaces_for_output`). Importing
        // during `dispatch_clients` wedged the GPU when GTK apps commit many subsurfaces at once.
        self.handle_commit(surface);
        for output in self.space.outputs() {
            layer_map_for_output(output).arrange();
        }
    }
}

delegate_compositor!(DesktopState);
