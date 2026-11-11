use anyhow::{Context, Result};
use focaldesk_browserd::{
    BrowserRequest, BrowserResponse, MAX_REQUEST, MAX_RESPONSE, engine::BrowserEngine,
};
use focaldesk_ipc::transport::{self, PeerPolicy};
use std::{sync::Arc, time::Duration};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    sync::{Mutex, Semaphore},
};

#[tokio::main]
async fn main() -> Result<()> {
    let args = std::env::args().collect::<Vec<_>>();
    if args
        .get(1)
        .is_some_and(|a| a == "--check" || a == "--check-interactions")
    {
        let url = args
            .get(2)
            .context("Usage: focaldesk-browserd --check URL")?
            .clone();
        let interactions = args[1] == "--check-interactions";
        return tokio::task::spawn_blocking(move || check(url, interactions)).await?;
    }
    let path = focaldesk_browserd::socket_path()?;
    let listener = transport::bind_user_socket(&path).map_err(anyhow::Error::msg)?;
    listener.set_nonblocking(true)?;
    let listener = tokio::net::UnixListener::from_std(listener)?;
    let engine = Arc::new(Mutex::new(BrowserEngine::default()));
    let slots = Arc::new(Semaphore::new(16));
    println!("Browser IPC listening on {}", path.display());
    loop {
        tokio::select! {
         signal=tokio::signal::ctrl_c()=>{signal?;engine.lock().await.shutdown().await;return Ok(());}
         connection=listener.accept()=>{
          let(mut stream,_)=connection?;
          let policy=PeerPolicy{endpoint:"browser",allowed_executables:&["focaldesk-server","focaldesk-ai-console","focaldesk-browserd"],allowed_units:&["focaldesk-server.service","focaldesk-ai-console-app.service","focaldesk-browserd.service"]};
          if let Err(error)=transport::require_authorized_peer(&stream,policy){eprintln!("Rejected browser peer: {error}");continue;}
          let Ok(slot)=slots.clone().try_acquire_owned() else{continue;};let engine=engine.clone();
          tokio::spawn(async move{
           let _slot=slot;
           let work=async {
            let mut bytes=Vec::new();(&mut stream).take(MAX_REQUEST+1).read_to_end(&mut bytes).await?;
            if bytes.len() as u64>MAX_REQUEST{anyhow::bail!("Browser request exceeds 64 KB");}
            let request:BrowserRequest=transport::decode_message(&bytes).map_err(anyhow::Error::msg)?;
            engine.lock().await.handle(request).await
           };
           let response=match tokio::time::timeout(Duration::from_secs(40),work).await{Ok(Ok(result))=>BrowserResponse{result:Some(result),error:None},Ok(Err(e))=>BrowserResponse{result:None,error:Some(format!("{e:#}"))},Err(_)=>BrowserResponse{result:None,error:Some("Browser operation timed out".into())}};
           if let Ok(bytes)=transport::encode_message(&response){if bytes.len() as u64<=MAX_RESPONSE{let _=tokio::time::timeout(Duration::from_secs(5),stream.write_all(&bytes)).await;}}
          });
         }
        }
    }
}
fn check(url: String, interactions: bool) -> Result<()> {
    use focaldesk_browserd::send;
    let opened = send(&BrowserRequest::Open { url })?;
    let id = opened["page_id"]
        .as_str()
        .context("No page ID")?
        .to_string();
    let result = (|| -> Result<()> {
        let read = send(&BrowserRequest::Read {
            page_id: id.clone(),
        })?;
        if read["text"].as_str().unwrap_or("").trim().is_empty() {
            anyhow::bail!("No rendered text returned");
        }
        if interactions {
            if !read["text"]
                .as_str()
                .unwrap_or("")
                .contains("Rendered by JavaScript")
            {
                anyhow::bail!("Page JavaScript did not render");
            }
            send(&BrowserRequest::Type {
                page_id: id.clone(),
                selector: "#field".into(),
                text: "Typed from Rust".into(),
            })?;
            send(&BrowserRequest::Click {
                page_id: id.clone(),
                selector: "#apply".into(),
            })?;
            let after = send(&BrowserRequest::Read {
                page_id: id.clone(),
            })?;
            if !after["text"]
                .as_str()
                .unwrap_or("")
                .contains("Result: Typed from Rust")
            {
                anyhow::bail!("JavaScript input/click test failed");
            }
        }
        let screenshot = send(&BrowserRequest::Screenshot {
            page_id: id.clone(),
        })?;
        use base64::Engine as _;
        let png = base64::engine::general_purpose::STANDARD.decode(
            screenshot["data_url"]
                .as_str()
                .context("No screenshot")?
                .trim_start_matches("data:image/png;base64,"),
        )?;
        if !png.starts_with(b"\x89PNG\r\n\x1a\n") {
            anyhow::bail!("Invalid PNG");
        }
        std::fs::write("/tmp/focaldesk-browser-check.png", &png)?;
        println!(
            "{}",
            serde_json::json!({"url":read["url"],"title":read["title"],"text_chars":read["text"].as_str().unwrap().len(),"elements":read["elements"].as_array().map(Vec::len),"screenshot_bytes":png.len(),"screenshot":"/tmp/focaldesk-browser-check.png"})
        );
        Ok(())
    })();
    let _ = send(&BrowserRequest::Close { page_id: id });
    result
}
