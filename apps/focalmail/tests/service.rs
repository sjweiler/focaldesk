//! Real daemon smoke test: isolated user data, no desktop or provider credentials.
use serde_json::{json, Value};
use std::{
    io::{BufRead, BufReader, Write},
    os::unix::{fs::PermissionsExt, net::UnixStream},
    path::PathBuf,
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};

struct Daemon {
    child: Child,
    root: PathBuf,
}
impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_dir_all(&self.root);
    }
}
fn command(root: &std::path::Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_focalmail"));
    command
        .arg("--mail-service")
        .env("XDG_RUNTIME_DIR", root.join("runtime"))
        .env("XDG_DATA_HOME", root.join("data"))
        .env_remove("DISPLAY")
        .env_remove("WAYLAND_DISPLAY")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    command
}
fn request(path: &std::path::Path, request: Value) -> Value {
    let mut stream = UnixStream::connect(path).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    writeln!(stream, "{}", json!({"id":"test","request":request})).unwrap();
    let mut line = String::new();
    BufReader::new(stream).read_line(&mut line).unwrap();
    serde_json::from_str(&line).unwrap()
}
#[test]
fn daemon_runs_without_a_window_and_preserves_single_owner_and_protocol() {
    let root =
        std::env::temp_dir().join(format!("focalmail-daemon-{:032x}", rand::random::<u128>()));
    std::fs::create_dir_all(root.join("runtime")).unwrap();
    let mut daemon = Daemon {
        child: command(&root).spawn().unwrap(),
        root,
    };
    let socket = daemon.root.join("runtime/focalmail/service.sock");
    let deadline = Instant::now() + Duration::from_secs(5);
    while !socket.exists() {
        assert!(
            daemon.child.try_wait().unwrap().is_none(),
            "Daemon exited before binding its socket"
        );
        assert!(Instant::now() < deadline, "Daemon did not start");
        std::thread::sleep(Duration::from_millis(20));
    }
    assert_eq!(
        std::fs::metadata(socket.parent().unwrap())
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o700
    );
    assert_eq!(
        std::fs::metadata(&socket).unwrap().permissions().mode() & 0o777,
        0o600
    );
    let status = request(&socket, json!({"method":"status"}));
    assert_eq!(status["event"], "done");
    assert_eq!(status["value"]["accounts"], json!([]));
    let instance = status["value"]["instance"].clone();
    let revision = status["value"]["revision"].clone();
    assert_eq!(
        request(
            &socket,
            json!({"method":"search","query":"anything","account":null,"limit":20})
        )["value"],
        json!([])
    );
    assert_eq!(
        request(
            &socket,
            json!({"method":"changes","after":revision,"wait_seconds":0})
        )["value"]["instance"],
        instance
    );
    assert!(
        !command(&daemon.root).status().unwrap().success(),
        "Second service must not steal the socket"
    );
    // Bad requests and failed operations must not bring down the daemon.
    let mut malformed = UnixStream::connect(&socket).unwrap();
    malformed.write_all(b"not-json\n").unwrap();
    drop(malformed);
    let error = request(
        &socket,
        json!({"method":"connect","path":"/nonexistent-focalmail-test-client.json"}),
    );
    assert_eq!(error["event"], "error");
    let status = request(&socket, json!({"method":"status"}));
    assert_eq!(status["value"]["instance"], instance);
    assert!(status["value"]["revision"].as_u64().unwrap() > revision.as_u64().unwrap());
    assert_eq!(status["value"]["active_operation"], Value::Null);
    assert!(daemon.child.try_wait().unwrap().is_none());
}
