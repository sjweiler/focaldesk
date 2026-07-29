use focaldesk_mcp::{IpcBackend, McpServer, StdioTransport};

fn main() {
    if let Err(err) = StdioTransport::run(McpServer::new(IpcBackend)) {
        eprintln!("focaldesk-mcp: {err}");
        std::process::exit(1);
    }
}
