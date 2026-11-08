//! Mail tools use the service socket; no provider tokens or direct database access.
use serde_json::{Value, json};
use std::{
    io::{BufRead, BufReader, Read, Write},
    os::unix::net::UnixStream,
    path::PathBuf,
    time::Duration,
};

pub fn call(tool: &str, arguments: &Value) -> Result<Value, String> {
    let request = request(tool, arguments)?;
    let runtime = std::env::var_os("XDG_RUNTIME_DIR").ok_or("XDG_RUNTIME_DIR is unavailable")?;
    let stream = UnixStream::connect(PathBuf::from(runtime).join("focalmail/service.sock"))
        .map_err(|error| {
            format!("Mail service unavailable; start FocalMail or focalmaild.service: {error}")
        })?;
    exchange(stream, request).map_err(|error| {
        if tool == "send_mail" {
            format!("{error}. If delivery is uncertain, check Sent before retrying.")
        } else {
            error
        }
    })
}
fn request(tool: &str, arguments: &Value) -> Result<Value, String> {
    let string = |name: &str| {
        arguments[name]
            .as_str()
            .ok_or_else(|| format!("{name} is required"))
    };
    Ok(match tool {
        "get_mail_status" => json!({"method":"status"}),
        "search_mail" => {
            json!({"method":"search","query":string("query")?,"account":arguments.get("account"),"limit":arguments["limit"].as_u64().unwrap_or(20).clamp(1,100)})
        }
        "read_mail_message" => {
            json!({"method":"read","account":string("account")?,"id":string("id")?})
        }
        "send_mail" => {
            if arguments["confirmed"] != true {
                return Err("Explicit approval of this email is required".into());
            }
            // All context fields are explicit: this is a new message, not a reply.
            json!({"method":"send","files":[],"draft":{
                "id":"", "from":string("account")?, "to":string("to")?, "cc":"",
                "subject":string("subject")?, "body":string("body")?,
                "thread_id":"", "in_reply_to":"", "references":"", "reply_subject":"", "reply_account":"", "forward":null
            }})
        }
        _ => return Err("Unknown mail tool".into()),
    })
}
fn exchange(mut stream: UnixStream, request: Value) -> Result<Value, String> {
    let send = request["method"] == "send";
    stream
        .set_read_timeout(Some(Duration::from_secs(if send { 90 } else { 15 })))
        .map_err(|e| e.to_string())?;
    stream
        .set_write_timeout(Some(Duration::from_secs(10)))
        .map_err(|e| e.to_string())?;
    let envelope = json!({"id":"mcp", "request":request});
    writeln!(stream, "{envelope}").map_err(|e| e.to_string())?;
    let mut reader = BufReader::new(stream);
    loop {
        let mut bytes = Vec::new();
        reader
            .by_ref()
            .take(2_097_153)
            .read_until(b'\n', &mut bytes)
            .map_err(|e| e.to_string())?;
        if bytes.len() > 2_097_152 || bytes.last() != Some(&b'\n') {
            return Err("Invalid or oversized mail response".into());
        }
        let response: Value = serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
        match response["event"].as_str() {
            Some("done") => return Ok(response["value"].clone()),
            Some("error") => {
                return Err(response["detail"]
                    .as_str()
                    .unwrap_or("Mail operation failed")
                    .into());
            }
            Some("progress") => {}
            _ => return Err("Unknown mail response".into()),
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn sending_requires_confirmation_and_preserves_exact_content() {
        let mut args = json!({"account":"me@example.com","to":"you@example.com","subject":"Hello","body":"Exact body\nwith newline"});
        assert!(request("send_mail", &args).is_err());
        args["confirmed"] = true.into();
        let output = request("send_mail", &args).unwrap();
        assert_eq!(output["draft"]["body"], args["body"]);
        assert_eq!(output["draft"]["from"], args["account"]);
        assert_eq!(output["files"], json!([]));
    }
    #[test]
    fn service_errors_and_results_are_forwarded() {
        for response in [
            json!({"event":"done","value":{"accounts":[]}}),
            json!({"event":"error","detail":"Reconnect required"}),
        ] {
            let (client, mut server) = UnixStream::pair().unwrap();
            let expected = response.clone();
            let worker = std::thread::spawn(move || {
                let mut line = String::new();
                BufReader::new(server.try_clone().unwrap())
                    .read_line(&mut line)
                    .unwrap();
                let envelope: Value = serde_json::from_str(&line).unwrap();
                assert_eq!(envelope["request"]["method"], "status");
                writeln!(server, "{response}").unwrap();
            });
            let result = exchange(client, json!({"method":"status"}));
            if expected["event"] == "done" {
                assert_eq!(result.unwrap(), expected["value"]);
            } else {
                assert_eq!(result.unwrap_err(), "Reconnect required");
            }
            worker.join().unwrap();
        }
    }
}
