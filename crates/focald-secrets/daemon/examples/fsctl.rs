//! fsctl — minimal synchronous client for the focald-secrets native surface.
//! Doubles as reference code for other Focaldesk daemons (std-only, no deps
//! beyond what focald-secrets already ships).
//!
//!   fsctl set google/oauth-refresh "the-token"
//!   fsctl get google/oauth-refresh
//!   fsctl list [prefix]
//!   fsctl delete google/oauth-refresh

use std::io::{Read, Write};
use std::os::unix::net::UnixStream;

fn b64_encode(data: &[u8]) -> String {
    const T: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for chunk in data.chunks(3) {
        let b = [
            chunk[0],
            *chunk.get(1).unwrap_or(&0),
            *chunk.get(2).unwrap_or(&0),
        ];
        let n = ((b[0] as u32) << 16) | ((b[1] as u32) << 8) | b[2] as u32;
        out.push(T[(n >> 18) as usize & 63] as char);
        out.push(T[(n >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 {
            T[(n >> 6) as usize & 63] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            T[n as usize & 63] as char
        } else {
            '='
        });
    }
    out
}

fn b64_decode(s: &str) -> Vec<u8> {
    let val = |c: u8| -> i32 {
        match c {
            b'A'..=b'Z' => (c - b'A') as i32,
            b'a'..=b'z' => (c - b'a' + 26) as i32,
            b'0'..=b'9' => (c - b'0' + 52) as i32,
            b'+' => 62,
            b'/' => 63,
            _ => -1,
        }
    };
    let mut out = Vec::new();
    let mut buf = 0u32;
    let mut bits = 0;
    for &c in s.as_bytes() {
        let v = val(c);
        if v < 0 {
            continue;
        }
        buf = (buf << 6) | v as u32;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((buf >> bits) as u8);
        }
    }
    out
}

fn rpc(json: &str) -> String {
    let path =
        std::env::var("XDG_RUNTIME_DIR").expect("XDG_RUNTIME_DIR") + "/focaldesk/secrets.sock";
    let mut s = UnixStream::connect(path).expect("connect focald-secrets");
    s.write_all(&(json.len() as u32).to_be_bytes()).unwrap();
    s.write_all(json.as_bytes()).unwrap();
    let mut len = [0u8; 4];
    s.read_exact(&mut len).unwrap();
    let mut body = vec![0u8; u32::from_be_bytes(len) as usize];
    s.read_exact(&mut body).unwrap();
    String::from_utf8(body).unwrap()
}

/// Extremely small JSON string escaper for keys/values we send.
fn esc(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    match args.get(1).map(String::as_str) {
        Some("get") => {
            let resp = rpc(&format!(r#"{{"op":"get","key":"{}"}}"#, esc(&args[2])));
            // naive extraction; a real daemon uses serde
            if let Some(v) = resp
                .split(r#""value_b64":""#)
                .nth(1)
                .and_then(|r| r.split('"').next())
            {
                std::io::stdout().write_all(&b64_decode(v)).unwrap();
            } else {
                eprintln!("{resp}");
                std::process::exit(1);
            }
        }
        Some("set") => {
            let value = args.get(3).cloned().unwrap_or_else(|| {
                let mut s = String::new();
                std::io::stdin().read_to_string(&mut s).unwrap();
                s.trim_end_matches('\n').to_string()
            });
            let resp = rpc(&format!(
                r#"{{"op":"set","key":"{}","value_b64":"{}"}}"#,
                esc(&args[2]),
                b64_encode(value.as_bytes())
            ));
            println!("{resp}");
        }
        Some("delete") => println!(
            "{}",
            rpc(&format!(r#"{{"op":"delete","key":"{}"}}"#, esc(&args[2])))
        ),
        Some("list") => {
            let p = args
                .get(2)
                .map(|p| format!(r#","prefix":"{}""#, esc(p)))
                .unwrap_or_default();
            println!("{}", rpc(&format!(r#"{{"op":"list"{p}}}"#)));
        }
        _ => {
            eprintln!("usage: fsctl get|set|delete|list ...");
            std::process::exit(2);
        }
    }
}
