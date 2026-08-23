use std::collections::BTreeMap;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::oneshot;

#[derive(Debug)]
pub(crate) struct RecordedRequest {
    pub method: String,
    pub path: String,
    pub headers: BTreeMap<String, String>,
    pub body: Vec<u8>,
}

impl RecordedRequest {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .get(&name.to_ascii_lowercase())
            .map(String::as_str)
    }

    pub fn json(&self) -> serde_json::Value {
        serde_json::from_slice(&self.body).expect("mock request body must be JSON")
    }
}

pub(crate) async fn serve_once(
    status: &str,
    headers: &[(&str, &str)],
    body: impl Into<Vec<u8>>,
) -> (String, oneshot::Receiver<RecordedRequest>) {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind deterministic provider test server");
    let address = listener.local_addr().expect("read mock server address");
    let status = status.to_string();
    let headers = headers
        .iter()
        .map(|(name, value)| ((*name).to_string(), (*value).to_string()))
        .collect::<Vec<_>>();
    let body = body.into();
    let (request_tx, request_rx) = oneshot::channel();

    tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.expect("accept provider request");
        let mut buffer = Vec::new();
        let header_end = loop {
            let mut chunk = [0_u8; 4096];
            let read = stream
                .read(&mut chunk)
                .await
                .expect("read provider request");
            assert!(read > 0, "provider disconnected before sending headers");
            buffer.extend_from_slice(&chunk[..read]);
            if let Some(index) = buffer.windows(4).position(|bytes| bytes == b"\r\n\r\n") {
                break index + 4;
            }
            assert!(
                buffer.len() <= 1024 * 1024,
                "provider request headers too large"
            );
        };

        let header_text = std::str::from_utf8(&buffer[..header_end])
            .expect("provider request headers must be UTF-8");
        let mut lines = header_text.split("\r\n");
        let request_line = lines.next().expect("request line");
        let mut request_parts = request_line.split_whitespace();
        let method = request_parts.next().expect("request method").to_string();
        let path = request_parts.next().expect("request path").to_string();
        let mut request_headers = BTreeMap::new();
        for line in lines.filter(|line| !line.is_empty()) {
            let (name, value) = line.split_once(':').expect("valid request header");
            request_headers.insert(name.trim().to_ascii_lowercase(), value.trim().to_string());
        }
        let content_length = request_headers
            .get("content-length")
            .and_then(|value| value.parse::<usize>().ok())
            .unwrap_or(0);
        while buffer.len() - header_end < content_length {
            let mut chunk = [0_u8; 4096];
            let read = stream.read(&mut chunk).await.expect("read provider body");
            assert!(read > 0, "provider disconnected before sending body");
            buffer.extend_from_slice(&chunk[..read]);
        }
        let request = RecordedRequest {
            method,
            path,
            headers: request_headers,
            body: buffer[header_end..header_end + content_length].to_vec(),
        };
        let _ = request_tx.send(request);

        let mut response = format!(
            "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n",
            body.len()
        );
        for (name, value) in headers {
            response.push_str(&format!("{name}: {value}\r\n"));
        }
        response.push_str("\r\n");
        stream
            .write_all(response.as_bytes())
            .await
            .expect("write provider response headers");
        stream
            .write_all(&body)
            .await
            .expect("write provider response body");
        stream.shutdown().await.expect("close provider response");
    });

    (format!("http://{address}"), request_rx)
}
