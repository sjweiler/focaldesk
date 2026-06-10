use wayland_client::globals::{registry_queue_init, GlobalListContents};
use wayland_client::protocol::{wl_output, wl_registry};
use wayland_client::{Connection, Dispatch, Proxy, QueueHandle};

/// Query output names from the compositor identified by `WAYLAND_DISPLAY`.
pub fn query_wayland_outputs() -> Vec<String> {
    let Ok(conn) = Connection::connect_to_env() else {
        return Vec::new();
    };

    struct State {
        names: Vec<String>,
    }

    impl Dispatch<wl_registry::WlRegistry, GlobalListContents> for State {
        fn event(
            _: &mut Self,
            _: &wl_registry::WlRegistry,
            _: wl_registry::Event,
            _: &GlobalListContents,
            _: &Connection,
            _: &QueueHandle<Self>,
        ) {
        }
    }

    impl Dispatch<wl_output::WlOutput, ()> for State {
        fn event(
            state: &mut Self,
            _: &wl_output::WlOutput,
            event: wl_output::Event,
            _: &(),
            _: &Connection,
            _: &QueueHandle<Self>,
        ) {
            if let wl_output::Event::Name { name } = event {
                state.names.push(name);
            }
        }
    }

    let Ok((globals, mut event_queue)) = registry_queue_init::<State>(&conn) else {
        return Vec::new();
    };

    let mut state = State {
        names: Vec::new(),
    };
    let qh = event_queue.handle();
    let registry = globals.registry();

    for global in globals.contents().clone_list() {
        if global.interface == wl_output::WlOutput::interface().name {
            let version = global.version.min(4);
            let _ = registry.bind(global.name, version, &qh, ());
        }
    }

    let _ = event_queue.roundtrip(&mut state);
    if state.names.is_empty() {
        let _ = event_queue.roundtrip(&mut state);
    }

    state.names
}
