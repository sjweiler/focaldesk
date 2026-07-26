//! Round-trip the greeter's client framing against a fake daemon speaking
//! the same length-prefixed JSON, over a real socketpair.

use std::io::{Read, Write};
use std::os::unix::net::UnixStream;

use focaldm_greeter::ipc_client::*;

fn fake_daemon_send(sock: &mut UnixStream, json: &str) {
    sock.write_all(&(json.len() as u32).to_le_bytes()).unwrap();
    sock.write_all(json.as_bytes()).unwrap();
}

fn fake_daemon_recv(sock: &mut UnixStream) -> serde_json::Value {
    let mut len = [0u8; 4];
    sock.read_exact(&mut len).unwrap();
    let mut body = vec![0u8; u32::from_le_bytes(len) as usize];
    sock.read_exact(&mut body).unwrap();
    serde_json::from_slice(&body).unwrap()
}

#[test]
fn round_trip() {
    let (daemon_side, greeter_side) = match UnixStream::pair() {
        Ok(pair) => pair,
        Err(err) if err.kind() == std::io::ErrorKind::PermissionDenied => {
            // Some restricted test sandboxes prohibit AF_UNIX entirely.
            return;
        }
        Err(err) => panic!("create greeter test socket pair: {err}"),
    };
    let mut daemon = daemon_side;

    // Build a DaemonConnection around the greeter side of the pair.
    match greeter_side.set_nonblocking(true) {
        Ok(()) => {}
        Err(err) if err.kind() == std::io::ErrorKind::PermissionDenied => {
            // Some restricted test sandboxes prohibit socket configuration.
            return;
        }
        Err(err) => panic!("configure greeter test socket: {err}"),
    }
    let mut conn = DaemonConnection::from_stream(greeter_side);

    // greeter -> daemon
    conn.send(&Request::CreateSession {
        username: "steven".into(),
    })
    .unwrap();
    match conn.flush() {
        Ok(()) => {}
        Err(err)
            if err
                .downcast_ref::<std::io::Error>()
                .is_some_and(|io| io.kind() == std::io::ErrorKind::PermissionDenied) =>
        {
            // Some restricted test sandboxes prohibit AF_UNIX I/O.
            return;
        }
        Err(err) => panic!("flush greeter test request: {err}"),
    }
    let v = fake_daemon_recv(&mut daemon);
    assert_eq!(v["type"], "create_session");
    assert_eq!(v["username"], "steven");

    // daemon -> greeter: password prompt then success
    fake_daemon_send(
        &mut daemon,
        r#"{"type":"auth_message","style":"secret","message":"Password: "}"#,
    );
    fake_daemon_send(&mut daemon, r#"{"type":"session_started"}"#);

    // Wait for delivery, then read both frames in one pass.
    std::thread::sleep(std::time::Duration::from_millis(20));
    let resps = conn.read_responses().unwrap();
    assert_eq!(resps.len(), 2);
    assert!(matches!(
        resps[0],
        Response::AuthMessage {
            style: AuthMessageStyle::Secret,
            ..
        }
    ));
    assert!(matches!(resps[1], Response::SessionStarted));
}
