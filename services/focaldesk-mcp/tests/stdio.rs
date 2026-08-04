use serde_json::Value;
use std::io::Write;
use std::process::{Command, Stdio};

fn run_server(input: &str) -> Vec<Value> {
    let mut child = Command::new(env!("CARGO_BIN_EXE_focaldesk-mcp"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn focaldesk-mcp");
    child
        .stdin
        .as_mut()
        .expect("piped stdin")
        .write_all(input.as_bytes())
        .expect("write MCP messages");
    drop(child.stdin.take());

    let output = child.wait_with_output().expect("wait for focaldesk-mcp");
    assert!(
        output.status.success(),
        "server failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout)
        .expect("UTF-8 stdout")
        .lines()
        .map(|line| serde_json::from_str(line).expect("JSON-RPC response"))
        .collect()
}

#[test]
fn stdio_enforces_the_initialization_lifecycle() {
    let responses = run_server(concat!(
        "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"tools/list\"}\n",
        "{\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"initialize\",\"params\":{\"protocolVersion\":\"2025-06-18\",\"capabilities\":{},\"clientInfo\":{\"name\":\"integration-test\",\"version\":\"1.0\"}}}\n",
        "{\"jsonrpc\":\"2.0\",\"id\":3,\"method\":\"tools/list\"}\n",
        "{\"jsonrpc\":\"2.0\",\"method\":\"notifications/initialized\"}\n",
        "{\"jsonrpc\":\"2.0\",\"id\":4,\"method\":\"tools/list\"}\n",
    ));

    assert_eq!(responses.len(), 4);
    assert_eq!(responses[0]["error"]["code"], -32002);
    assert_eq!(responses[1]["result"]["protocolVersion"], "2025-06-18");
    assert_eq!(responses[2]["error"]["code"], -32002);
    assert_eq!(
        responses[3]["result"]["tools"].as_array().unwrap().len(),
        12
    );
}

#[test]
fn stdio_reports_parse_and_invalid_request_errors() {
    let responses = run_server("{broken json}\n[]\n");
    assert_eq!(responses.len(), 2);
    assert_eq!(responses[0]["error"]["code"], -32700);
    assert_eq!(responses[1]["error"]["code"], -32600);
}
