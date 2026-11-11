#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.get(1).is_some_and(|arg| arg == "--check-image") {
        use base64::Engine as _;
        use focaldesk_ai::{send_ai_request, AiIpcRequest, AiIpcResponse, ChatRequest};
        let path = args
            .get(2)
            .expect("--check-image requires a synthetic PNG path");
        let bytes = std::fs::read(path).expect("read test image");
        assert!(bytes.starts_with(b"\x89PNG\r\n\x1a\n") && bytes.len() < 1024 * 1024);
        let mut request = ChatRequest::from_prompt(
            "What is the dominant color in this image? Reply with the color only.",
        );
        request.messages[0].images.push(format!(
            "data:image/png;base64,{}",
            base64::engine::general_purpose::STANDARD.encode(bytes)
        ));
        request.max_tokens = Some(2048);
        match send_ai_request(&AiIpcRequest::Chat { request }) {
            Ok(AiIpcResponse::Chat { response }) => {
                println!("{}", serde_json::to_string(&response).unwrap());
                std::process::exit(if response.content.to_lowercase().contains("red") {
                    0
                } else {
                    1
                });
            }
            other => {
                eprintln!("{other:?}");
                std::process::exit(1);
            }
        }
    }
    if args.get(1).is_some_and(|arg| arg == "--check-stream-stop") {
        use focaldesk_ai::{cancel_ai_stream, stream_ai_chat, AiStreamEvent, ChatRequest};
        let mut request = ChatRequest::from_prompt(
            "Write 100 numbered lines about Rust programming, with a newline after each line. Start immediately with line 1.",
        );
        request.max_tokens = Some(4096);
        let mut saw_delta = false;
        let mut cancelled = false;
        let result = stream_ai_chat(request, |event| {
            match event {
                AiStreamEvent::Delta {
                    request_id,
                    content,
                } if !saw_delta && !content.is_empty() => {
                    saw_delta = true;
                    if !cancel_ai_stream(&request_id)? {
                        anyhow::bail!("Cancellation was not accepted");
                    }
                }
                AiStreamEvent::Cancelled { .. } => cancelled = true,
                AiStreamEvent::Failed { message, .. } => anyhow::bail!(message),
                _ => {}
            }
            Ok(())
        });
        println!(
            "{}",
            serde_json::json!({"streamed_delta":saw_delta,"cancelled":cancelled,"error":result.as_ref().err().map(|e|format!("{e:#}"))})
        );
        std::process::exit(if result.is_ok() && saw_delta && cancelled {
            0
        } else {
            1
        });
    }
    if args.get(1).is_some_and(|arg| arg == "--check-services") {
        use focaldesk_ai::{send_ai_request, AiIpcRequest, AiIpcResponse};
        let mut failed = false;
        for request in [
            AiIpcRequest::ListProviders,
            AiIpcRequest::Status,
            AiIpcRequest::MemoryStatus,
            AiIpcRequest::ListAgents,
            AiIpcRequest::ListAgentRuns,
            AiIpcRequest::ListConnectors,
            AiIpcRequest::ListIndexedDocuments,
        ] {
            match send_ai_request(&request) {
                Ok(response) => {
                    if matches!(response, AiIpcResponse::Error { .. }) {
                        failed = true;
                    }
                    println!("{}", serde_json::to_string(&response).unwrap());
                }
                Err(error) => {
                    eprintln!("{request:?}: {error:#}");
                    failed = true;
                }
            }
        }
        std::process::exit(if failed { 1 } else { 0 });
    }
    if args
        .get(1)
        .is_some_and(|arg| arg == "--check-browser-agent")
    {
        use focaldesk_ai::{
            send_ai_request, AgentRequest, AgentRunState, AiIpcRequest, AiIpcResponse,
        };
        let run_id = match send_ai_request(&AiIpcRequest::StartAgent { request: AgentRequest {
            agent_id: Some("browser".into()), provider: None, model: None,
            objective: "Open https://example.com using browser_open. Report the actual page title and the first sentence of its rendered text. Use no clicks or typing. Close the page after reading it.".into(),
        }}) {
            Ok(AiIpcResponse::AgentStarted { run_id }) => run_id,
            result => { eprintln!("Could not start browser agent: {result:?}"); std::process::exit(1); }
        };
        println!("Browser agent started: {run_id}");
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(125);
        while std::time::Instant::now() < deadline {
            match send_ai_request(&AiIpcRequest::GetAgentRun {
                run_id: run_id.clone(),
            }) {
                Ok(AiIpcResponse::AgentRun {
                    status: Some(run), ..
                }) if run.state.is_terminal() => {
                    let observed = run.observations.iter().any(|step| {
                        step.tool == "browser_open"
                            && step
                                .result
                                .get("title")
                                .and_then(serde_json::Value::as_str)
                                .is_some()
                    });
                    println!(
                        "{}",
                        serde_json::json!({"run_id":run_id,"state":run.state,"observed_browser_open":observed,"tool_steps":run.completed_tool_steps,"answer":run.result.as_ref().map(|r|&r.answer),"error":run.error})
                    );
                    std::process::exit(if run.state == AgentRunState::Completed && observed {
                        0
                    } else {
                        1
                    });
                }
                Ok(_) => {}
                Err(error) => {
                    eprintln!("{error:#}");
                    std::process::exit(1);
                }
            }
            std::thread::sleep(std::time::Duration::from_secs(1));
        }
        let _ = send_ai_request(&AiIpcRequest::CancelAgentRun { run_id });
        eprintln!("Browser agent test timed out");
        std::process::exit(1);
    }
    if args.get(1).is_some_and(|arg| arg == "--check-capabilities") {
        use focaldesk_ai::{
            send_ai_request, AiIpcRequest, AiIpcResponse, ChatMessage, ChatRequest,
        };
        let mut request = ChatRequest::from_prompt(
            "In this running FocalDesk session, what RAG and browser capabilities are actually available? Distinguish an empty index from missing RAG, and ordinary Chat from the Browser Assistant. Answer briefly from the supplied runtime facts.",
        );
        request.messages.insert(
            0,
            ChatMessage::assistant("I have no RAG or browser capability."),
        );
        request.use_memory = true;
        match send_ai_request(&AiIpcRequest::Chat { request }) {
            Ok(AiIpcResponse::Chat { response }) if !response.content.trim().is_empty() => {
                println!("{}", serde_json::to_string(&response).unwrap());
                return;
            }
            result => {
                eprintln!("Capability check failed: {result:?}");
                std::process::exit(1);
            }
        }
    }
    if args.get(1).is_some_and(|arg| arg == "--check-chat") {
        use focaldesk_ai::{send_ai_request, AiIpcRequest, AiIpcResponse, ChatRequest};
        let mut request =
            ChatRequest::from_prompt("Reply with exactly: FocalDesk connection works");
        request.max_tokens = None;
        match send_ai_request(&AiIpcRequest::Chat { request }) {
            Ok(AiIpcResponse::Chat { response }) if !response.content.trim().is_empty() => {
                println!("{}", serde_json::to_string(&response).unwrap());
                return;
            }
            result => {
                eprintln!("{result:?}");
                std::process::exit(1);
            }
        }
    }
    focaldesk_ai_console_lib::run()
}
