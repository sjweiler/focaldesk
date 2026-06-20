//! Client bindings for [`focaldesk_color_v1`](../../crates/focaldesk-engine/protocols/focaldesk-color-v1.xml).

pub mod client {
    #![allow(dead_code, non_camel_case_types, unused_unsafe, unused_variables)]
    #![allow(non_upper_case_globals, non_snake_case, unused_imports)]
    #![allow(missing_docs, clippy::all)]

    use wayland_client;
    use wayland_client::protocol::*;

    pub mod __interfaces {
        use wayland_client::protocol::__interfaces::*;
        wayland_scanner::generate_interfaces!(
            "../../crates/focaldesk-engine/protocols/focaldesk-color-v1.xml"
        );
    }
    use self::__interfaces::*;

    wayland_scanner::generate_client_code!(
        "../../crates/focaldesk-engine/protocols/focaldesk-color-v1.xml"
    );
}
