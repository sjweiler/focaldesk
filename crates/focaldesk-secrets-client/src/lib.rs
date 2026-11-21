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
    Get {
        key: &'a str,
    },
    Set {
        key: &'a str,
        value_b64: &'a str,
        label: &'a str,
        attributes: std::collections::BTreeMap<String, String>,
        content_type: &'a str,
    },
    Delete {
        key: &'a str,
    },
}

pub fn delete(key: &str) -> Result<()> {
    let path = socket_path()?;
    let mut stream = match UnixStream::connect(&path) {
        Ok(stream) => stream,
        Err(error) => {
            return secret_service_delete(key)
                .context("connect to Focaldesk broker or desktop Secret Service")
                .map_err(|e| anyhow!("{} (broker socket {}: {})", e, path.display(), error));
        }
    };
    let request = Zeroizing::new(serde_json::to_vec(&Request::Delete { key })?);
    write_frame(&mut stream, &request)?;
    let response = Zeroizing::new(read_frame(&mut stream)?);
    let decoded: Response = serde_json::from_slice(&response)?;
    if !decoded.ok {
        bail!(
            "credential broker rejected delete for {key:?}: {}",
            decoded.error.as_deref().unwrap_or("unknown error")
        );
    }
    Ok(())
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
    let path = socket_path()?;
    match UnixStream::connect(&path) {
        Ok(mut stream) => get_on_stream(&mut stream, key),
        Err(error) => secret_service_get(key)
            .context("connect to Focaldesk broker or desktop Secret Service")
            .map_err(|e| anyhow!("{} (broker socket {}: {})", e, path.display(), error)),
    }
}

/// Retrieve a UTF-8 credential from an explicit socket path.
pub fn get_from(socket: &Path, key: &str) -> Result<Zeroizing<String>> {
    let mut stream = UnixStream::connect(socket)
        .with_context(|| format!("connect to credential broker at {}", socket.display()))?;
    get_on_stream(&mut stream, key)
}

fn get_on_stream(stream: &mut UnixStream, key: &str) -> Result<Zeroizing<String>> {
    let request = Zeroizing::new(serde_json::to_vec(&Request::Get { key })?);
    write_frame(stream, &request)?;

    let response = Zeroizing::new(read_frame(stream)?);
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

/// Store a UTF-8 credential through the broker's ACL-protected native API.
/// The encoded request buffer and base64 intermediary are zeroized on drop.
pub fn set(key: &str, value: &str, label: &str) -> Result<()> {
    let path = socket_path()?;
    match UnixStream::connect(&path) {
        Ok(stream) => set_on_stream(stream, key, value, label),
        Err(error) => secret_service_set(key, value, label)
            .context("connect to Focaldesk broker or desktop Secret Service")
            .map_err(|e| anyhow!("{} (broker socket {}: {})", e, path.display(), error)),
    }
}

/// Store a UTF-8 credential through an explicit broker socket.
pub fn set_at(socket: &Path, key: &str, value: &str, label: &str) -> Result<()> {
    let stream = UnixStream::connect(socket)
        .with_context(|| format!("connect to credential broker at {}", socket.display()))?;
    set_on_stream(stream, key, value, label)
}

fn set_on_stream(mut stream: UnixStream, key: &str, value: &str, label: &str) -> Result<()> {
    let encoded = Zeroizing::new(base64::engine::general_purpose::STANDARD.encode(value));
    let request = Zeroizing::new(serde_json::to_vec(&Request::Set {
        key,
        value_b64: &encoded,
        label,
        attributes: std::collections::BTreeMap::new(),
        content_type: "application/octet-stream",
    })?);
    write_frame(&mut stream, &request)?;
    let response = Zeroizing::new(read_frame(&mut stream)?);
    let decoded: Response = serde_json::from_slice(&response).context("decode broker response")?;
    if !decoded.ok {
        bail!(
            "credential broker rejected write to {key:?}: {}",
            decoded.error.as_deref().unwrap_or("unknown error")
        );
    }
    Ok(())
}

// KDE Wallet and GNOME Keyring expose the same Secret Service interface. Use
// secret-tool only when the Focaldesk broker socket is absent; this keeps the
// native broker's service-unit ACLs authoritative where it is installed.
fn secret_service_get(key: &str) -> Result<Zeroizing<String>> {
    let output = std::process::Command::new("secret-tool")
        .args(["lookup", "focaldesk-key", key])
        .output()
        .context(
            "launch secret-tool (install libsecret-tools and enable KDE Wallet Secret Service)",
        )?;
    if !output.status.success() {
        bail!(
            "Secret Service lookup failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    let value =
        String::from_utf8(output.stdout).context("Secret Service returned non-UTF-8 data")?;
    Ok(Zeroizing::new(
        value.trim_end_matches(['\r', '\n']).to_owned(),
    ))
}

fn secret_service_set(key: &str, value: &str, label: &str) -> Result<()> {
    let mut child = std::process::Command::new("secret-tool")
        .args(["store", "--label", label, "focaldesk-key", key])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .context(
            "launch secret-tool (install libsecret-tools and enable KDE Wallet Secret Service)",
        )?;
    use std::io::Write as _;
    child
        .stdin
        .take()
        .context("open secret-tool input")?
        .write_all(value.as_bytes())?;
    let output = child.wait_with_output()?;
    if !output.status.success() {
        bail!(
            "Secret Service store failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(())
}

fn secret_service_delete(key: &str) -> Result<()> {
    let output = std::process::Command::new("secret-tool")
        .args(["clear", "focaldesk-key", key])
        .output()
        .context(
            "launch secret-tool (install libsecret-tools and enable KDE Wallet Secret Service)",
        )?;
    if !output.status.success() {
        bail!(
            "Secret Service delete failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(())
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
    use super::{get_from, set_at};
    use base64::Engine as _;
    use std::io::{Read, Write};
    use std::os::unix::net::UnixListener;
    use std::thread;

    #[test]
    fn retrieves_utf8_secret() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("secrets.sock");
        let listener = match UnixListener::bind(&path) {
            Ok(listener) => listener,
            Err(err) if err.kind() == std::io::ErrorKind::PermissionDenied => {
                // Some restricted test sandboxes prohibit AF_UNIX entirely.
                return;
            }
            Err(err) => panic!("bind test credential socket: {err}"),
        };
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

    #[test]
    fn stores_secret_without_plaintext_wire_value() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("secrets-set.sock");
        let listener = match UnixListener::bind(&path) {
            Ok(listener) => listener,
            Err(err) if err.kind() == std::io::ErrorKind::PermissionDenied => return,
            Err(err) => panic!("bind test credential socket: {err}"),
        };
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut len = [0_u8; 4];
            stream.read_exact(&mut len).unwrap();
            let mut request = vec![0; u32::from_be_bytes(len) as usize];
            stream.read_exact(&mut request).unwrap();
            let request: serde_json::Value = serde_json::from_slice(&request).unwrap();
            assert_eq!(request["op"], "set");
            assert_eq!(request["key"], "aios/signers/local-dev/ed25519");
            assert_ne!(request["value_b64"], "private-seed");
            let response = serde_json::to_vec(&serde_json::json!({"ok": true})).unwrap();
            stream
                .write_all(&(response.len() as u32).to_be_bytes())
                .unwrap();
            stream.write_all(&response).unwrap();
        });
        set_at(
            &path,
            "aios/signers/local-dev/ed25519",
            "private-seed",
            "Local signer",
        )
        .unwrap();
        server.join().unwrap();
    }
}
