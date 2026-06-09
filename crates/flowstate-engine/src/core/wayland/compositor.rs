use crate::core::wayland::client::ClientState;
#[cfg(feature = "xwayland")]
use flowstate_logging::flog;
#[allow(unused_imports)]
use smithay::backend::renderer::buffer_type;
use smithay::backend::renderer::utils::on_commit_buffer_handler;
use smithay::delegate_compositor;
#[allow(unused_imports)]
use smithay::reexports::calloop::Interest;
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
#[allow(unused_imports)]
use smithay::reexports::wayland_server::Resource;
#[allow(unused_imports)]
use smithay::wayland::compositor::{
    add_blocker, add_pre_commit_hook, with_states, BufferAssignment, CompositorClientState,
    SurfaceAttributes,
};
use smithay::wayland::compositor::{CompositorHandler, CompositorState as SmithayCompositorState};
use smithay::wayland::dmabuf::get_dmabuf;
#[cfg(feature = "xwayland")]
use smithay::xwayland::XWaylandClientData;
use std::sync::atomic::AtomicUsize;
#[allow(unused_imports)]
use std::sync::atomic::Ordering;

use smithay::desktop::layer_map_for_output;

use crate::core::desktop::DesktopState;
use smithay::reexports::wayland_server::Client;

#[cfg_attr(not(feature = "xwayland"), allow(dead_code))]
static XWAYLAND_BUFFER_LOGS: AtomicUsize = AtomicUsize::new(0);
#[cfg_attr(not(feature = "xwayland"), allow(dead_code))]
static DMABUF_BLOCKER_LOGS: AtomicUsize = AtomicUsize::new(0);

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
            #[cfg(not(feature = "xwayland"))]
            let _ = state;
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
                #[cfg(not(feature = "xwayland"))]
                let _ = &dmabuf;
                #[cfg(feature = "xwayland")]
                if let Some(client) = surface.client() {
                    if let Ok((blocker, source)) = dmabuf.generate_blocker(Interest::READ) {
                        let Some(handle) = state.xwayland_loop_handle.clone() else {
                            let seq = DMABUF_BLOCKER_LOGS.fetch_add(1, Ordering::Relaxed);
                            if seq < 100 {
                                flog("dmabuf read blocker skipped: no compositor source loop");
                            }
                            return;
                        };

                        let res = handle.insert_source(source, move |_, _, data| {
                            let seq = DMABUF_BLOCKER_LOGS.fetch_add(1, Ordering::Relaxed);
                            if seq < 100 {
                                flog("dmabuf read blocker cleared");
                            }
                            let dh = data.display_handle.clone();
                            data.client_compositor_state(&client)
                                .blocker_cleared(data, &dh);
                            Ok(())
                        });
                        if res.is_ok() {
                            let seq = DMABUF_BLOCKER_LOGS.fetch_add(1, Ordering::Relaxed);
                            if seq < 100 {
                                flog("dmabuf read blocker added");
                            }
                            add_blocker(surface, blocker);
                        } else {
                            let seq = DMABUF_BLOCKER_LOGS.fetch_add(1, Ordering::Relaxed);
                            if seq < 100 {
                                flog("dmabuf read blocker source insert failed");
                            }
                        }
                    } else {
                        let seq = DMABUF_BLOCKER_LOGS.fetch_add(1, Ordering::Relaxed);
                        if seq < 100 {
                            flog("dmabuf read blocker already ready");
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
