use anyhow::Result;
use focaldesk_ai::{AiService, ChatRequest};
use serde::Deserialize;
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

const PAGE: &str = include_str!("web_ui_v2.html");

#[derive(Deserialize)]
struct Input {
    prompt: String,
    #[serde(default = "default_use_memory")]
    use_memory: bool,
}

const fn default_use_memory() -> bool {
    true
}

pub async fn serve(ai: Arc<AiService>) -> Result<()> {
    let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 3847)).await?;
    loop {
        let (mut socket, _) = listener.accept().await?;
        let ai = ai.clone();
        tokio::spawn(async move {
            let mut data = vec![0u8; 256 * 1024];
            let Ok(size) = socket.read(&mut data).await else {
                return;
            };
            let request = String::from_utf8_lossy(&data[..size]);
            let body = if request.starts_with("POST /api/chat") {
                request.split("\r\n\r\n").nth(1).unwrap_or("")
            } else {
                ""
            };
            let (status, content_type, response) = if request.starts_with("GET /") {
                ("200 OK", "text/html", PAGE.to_string())
            } else if let Ok(input) = serde_json::from_str::<Input>(body) {
                let mut chat = ChatRequest::from_prompt(input.prompt);
                chat.use_memory = input.use_memory;
                match ai.chat(chat).await {
                    Ok(answer) => (
                        "200 OK",
                        "application/json",
                        serde_json::json!({
                            "content": answer.content,
                            "citations": answer.citations,
                        })
                        .to_string(),
                    ),
                    Err(error) => (
                        "400 Bad Request",
                        "application/json",
                        serde_json::json!({"error":error.to_string()}).to_string(),
                    ),
                }
            } else {
                (
                    "400 Bad Request",
                    "application/json",
                    "{\"error\":\"invalid request\"}".into(),
                )
            };
            let _ = socket.write_all(format!("HTTP/1.1 {status}\r\nContent-Type: {content_type}; charset=utf-8\r\nCache-Control: no-store\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{response}", response.len()).as_bytes()).await;
        });
    }
}
