//! Blocking client for focald-secrets' native, ACL-protected Unix socket.
//!
//! The client is intentionally small so Focaldesk daemons can retrieve
//! credentials during startup without depending on the Secret Service D-Bus
//! API. Authorization is performed by the broker from the peer's systemd unit.

use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow, bail};
use base64::Engine as _;
use serde::{Deserialize, Serialize};
use zeroize::Zeroizing;

const MAX_FRAME: u32 = 1 << 20;

#[derive(Debug, Serialize)]
#[serde(tag = "op", rename_all = "snake_case")]
enum Request<'a> {
    Get { key: &'a str },
}

#[derive(Debug, Deserialize)]
struct Response {
    ok: bool,
    #[serde(default)]
    value_b64: Option<String>,
    #[serde(default)]
    error: Option<String>,
}

/// Resolve the native broker socket from `FOCALD_SECRETS_SOCKET`, then
/// `XDG_RUNTIME_DIR`, matching the daemon's default.
pub fn socket_path() -> Result<PathBuf> {
    if let Some(path) = std::env::var_os("FOCALD_SECRETS_SOCKET") {
        return Ok(PathBuf::from(path));
    }
    let runtime = std::env::var_os("XDG_RUNTIME_DIR")
        .context("XDG_RUNTIME_DIR is not set and FOCALD_SECRETS_SOCKET was not provided")?;
    Ok(PathBuf::from(runtime).join("focaldesk/secrets.sock"))
}

/// Retrieve a UTF-8 credential. The returned buffer is zeroized on drop.
pub fn get(key: &str) -> Result<Zeroizing<String>> {
    get_from(&socket_path()?, key)
}

/// Retrieve a UTF-8 credential from an explicit socket path.
pub fn get_from(socket: &Path, key: &str) -> Result<Zeroizing<String>> {
    let mut stream = UnixStream::connect(socket)
        .with_context(|| format!("connect to credential broker at {}", socket.display()))?;
    let request = Zeroizing::new(serde_json::to_vec(&Request::Get { key })?);
    write_frame(&mut stream, &request)?;

    let response = Zeroizing::new(read_frame(&mut stream)?);
    let decoded: Response = serde_json::from_slice(&response).context("decode broker response")?;

    if !decoded.ok {
        bail!(
            "credential broker rejected {key:?}: {}",
            decoded.error.as_deref().unwrap_or("unknown error")
        );
    }
    let encoded = Zeroizing::new(
        decoded
            .value_b64
            .ok_or_else(|| anyhow!("credential broker returned no value for {key:?}"))?,
    );
    let bytes = Zeroizing::new(
        base64::engine::general_purpose::STANDARD
            .decode(encoded.as_bytes())
            .context("decode broker credential")?,
    );
    let value = std::str::from_utf8(&bytes)
        .context("broker credential is not UTF-8")?
        .to_owned();
    Ok(Zeroizing::new(value))
}

fn write_frame(stream: &mut UnixStream, body: &[u8]) -> Result<()> {
    let len = u32::try_from(body.len()).context("broker request is too large")?;
    if len == 0 || len > MAX_FRAME {
        bail!("broker request length is out of bounds");
    }
    stream.write_all(&len.to_be_bytes())?;
    stream.write_all(body)?;
    stream.flush()?;
    Ok(())
}

fn read_frame(stream: &mut UnixStream) -> Result<Vec<u8>> {
    let mut len = [0_u8; 4];
    stream.read_exact(&mut len)?;
    let len = u32::from_be_bytes(len);
    if len == 0 || len > MAX_FRAME {
        bail!("broker response length is out of bounds");
    }
    let mut body = vec![0_u8; len as usize];
    stream.read_exact(&mut body)?;
    Ok(body)
}

#[cfg(test)]
mod tests {
    use super::get_from;
    use base64::Engine as _;
    use std::io::{Read, Write};
    use std::os::unix::net::UnixListener;
    use std::thread;

    #[test]
    fn retrieves_utf8_secret() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("secrets.sock");
        let listener = UnixListener::bind(&path).unwrap();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut len = [0_u8; 4];
            stream.read_exact(&mut len).unwrap();
            let mut request = vec![0; u32::from_be_bytes(len) as usize];
            stream.read_exact(&mut request).unwrap();
            let request: serde_json::Value = serde_json::from_slice(&request).unwrap();
            assert_eq!(request["key"], "ai/openai-api-key");

            let response = serde_json::json!({
                "ok": true,
                "value_b64": base64::engine::general_purpose::STANDARD.encode("secret-value")
            });
            let response = serde_json::to_vec(&response).unwrap();
            stream
                .write_all(&(response.len() as u32).to_be_bytes())
                .unwrap();
            stream.write_all(&response).unwrap();
        });

        let value = get_from(&path, "ai/openai-api-key").unwrap();
        assert_eq!(value.as_str(), "secret-value");
        server.join().unwrap();
    }
}
