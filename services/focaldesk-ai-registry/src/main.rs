use anyhow::{Context, Result, anyhow, bail};
use focaldesk_ai::{FaiPrivateRegistry, FaiSigner};
use rand::RngCore;
use serde::{Deserialize, Serialize};
use std::fs::OpenOptions;
use std::io::Write as _;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use subtle::ConstantTimeEq;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use zeroize::Zeroizing;

const MAX_HEADER_BYTES: usize = 16 * 1024;
const MAX_BODY_BYTES: usize = 2 * 1024 * 1024;

#[derive(Deserialize)]
struct RevokeRequest {
    package_id: String,
    version: String,
    reason: String,
}

#[derive(Serialize)]
struct AuditEvent<'a> {
    at_unix: u64,
    action: &'a str,
    subject: &'a str,
    outcome: &'a str,
}

#[tokio::main]
async fn main() -> Result<()> {
    let mut args = std::env::args().skip(1);
    let command = args
        .next()
        .or_else(|| std::env::var("FOCALDESK_AI_REGISTRY_COMMAND").ok())
        .unwrap_or_else(|| "serve".into());
    let root = registry_root()?;
    let registry_id =
        std::env::var("FOCALDESK_AI_REGISTRY_ID").unwrap_or_else(|_| "private".into());
    match command.as_str() {
        "init" => {
            if args.next().is_some() {
                bail!("init accepts no arguments; configure with environment variables");
            }
            let secret = load_or_generate_secret(&catalog_key_handle(&registry_id), 32)?;
            let key = decode_key(&secret)?;
            let registry = FaiPrivateRegistry::open(&root, &key)?;
            registry.initialize_policy(&registry_id)?;
            load_or_seed_token(
                &read_token_handle(&registry_id),
                "FOCALDESK_AI_REGISTRY_READ_TOKEN_FILE",
            )?;
            load_or_seed_token(
                &publish_token_handle(&registry_id),
                "FOCALDESK_AI_REGISTRY_PUBLISH_TOKEN_FILE",
            )?;
            println!(
                "registry={} catalog_public_key={}",
                registry_id,
                registry.signing_public_key_hex()
            );
            Ok(())
        }
        "generate-token" => {
            let path = PathBuf::from(
                args.next()
                    .context("generate-token requires a new output path")?,
            );
            if args.next().is_some() {
                bail!("generate-token accepts exactly one output path");
            }
            let mut bytes = Zeroizing::new([0_u8; 32]);
            rand::rngs::OsRng.fill_bytes(bytes.as_mut());
            let token = Zeroizing::new(encode_hex(bytes.as_ref()));
            let mut file = OpenOptions::new()
                .create_new(true)
                .write(true)
                .mode(0o600)
                .open(&path)?;
            writeln!(file, "{}", token.as_str())?;
            file.sync_all()?;
            println!("created private token file {}", path.display());
            Ok(())
        }
        "approve" => {
            let signer_id = args.next().context("approve requires a signer id")?;
            let public_key_hex = args.next().context("approve requires a public key")?;
            if args.next().is_some() {
                bail!("approve accepts a signer id and public key");
            }
            let registry = open_registry(&root, &registry_id)?;
            registry.approve_signer(FaiSigner {
                id: signer_id.clone(),
                public_key_hex,
            })?;
            audit(&root, "approve_signer", &signer_id, "accepted")?;
            println!("approved {signer_id}");
            Ok(())
        }
        "revoke" => {
            let package_id = args.next().context("revoke requires a package id")?;
            let version = args.next().context("revoke requires a version")?;
            let reason = args.collect::<Vec<_>>().join(" ");
            let registry = open_registry(&root, &registry_id)?;
            registry.revoke(&package_id, &version, &reason)?;
            audit(
                &root,
                "revoke",
                &format!("{package_id}@{version}"),
                "accepted",
            )?;
            println!("revoked {package_id} {version}");
            Ok(())
        }
        "serve" => serve(root, registry_id).await,
        _ => bail!("expected init, generate-token, approve, revoke, or serve"),
    }
}

async fn serve(root: PathBuf, registry_id: String) -> Result<()> {
    let bind =
        std::env::var("FOCALDESK_AI_REGISTRY_BIND").unwrap_or_else(|_| "127.0.0.1:9473".into());
    if !bind.starts_with("127.0.0.1:") && !bind.starts_with("[::1]:") {
        bail!("non-loopback registry binding requires a TLS reverse proxy");
    }
    let registry = std::sync::Arc::new(open_registry(&root, &registry_id)?);
    let read_token = std::sync::Arc::new(focaldesk_secrets_client::get(&read_token_handle(
        &registry_id,
    ))?);
    let publish_token = std::sync::Arc::new(focaldesk_secrets_client::get(&publish_token_handle(
        &registry_id,
    ))?);
    let listener = TcpListener::bind(&bind).await?;
    println!("private AIOS registry listening on {bind}");
    loop {
        let (stream, _) = listener.accept().await?;
        let registry = registry.clone();
        let root = root.clone();
        let read_token = read_token.clone();
        let publish_token = publish_token.clone();
        tokio::spawn(async move {
            let mut stream = stream;
            if handle(&mut stream, &registry, &root, &read_token, &publish_token)
                .await
                .is_err()
            {
                let _ = respond(&mut stream, 400, b"request rejected", "text/plain").await;
            }
        });
    }
}

async fn handle(
    stream: &mut TcpStream,
    registry: &FaiPrivateRegistry,
    root: &Path,
    read_token: &str,
    publish_token: &str,
) -> Result<()> {
    let request = read_request(stream).await?;
    let needs_publish = request.method == "PUT" || request.method == "POST";
    let expected = if needs_publish {
        publish_token
    } else {
        read_token
    };
    if !token_matches(request.authorization.as_deref(), expected) {
        return respond(stream, 401, b"unauthorized", "text/plain").await;
    }
    let response = match (request.method.as_str(), request.path.as_str()) {
        ("GET", "/v1/catalog") => serde_json::to_vec(&registry.catalog()?)?,
        ("PUT", "/v1/packages") => {
            let bundle: focaldesk_ai::FaiBundle = serde_json::from_slice(&request.body)?;
            let entry = registry.publish(&bundle)?;
            audit(
                root,
                "publish",
                &format!("{}@{}", entry.package_id, entry.version),
                "accepted",
            )?;
            serde_json::to_vec(&entry)?
        }
        ("POST", "/v1/revoke") => {
            let request: RevokeRequest = serde_json::from_slice(&request.body)?;
            let revocation =
                registry.revoke(&request.package_id, &request.version, &request.reason)?;
            audit(
                root,
                "revoke",
                &format!("{}@{}", request.package_id, request.version),
                "accepted",
            )?;
            serde_json::to_vec(&revocation)?
        }
        ("POST", "/v1/signers") => {
            let signer: FaiSigner = serde_json::from_slice(&request.body)?;
            let policy = registry.approve_signer(signer.clone())?;
            audit(root, "approve_signer", &signer.id, "accepted")?;
            serde_json::to_vec(&policy)?
        }
        ("GET", path) if path.starts_with("/v1/packages/") => {
            let parts = path
                .trim_start_matches("/v1/packages/")
                .split('/')
                .collect::<Vec<_>>();
            if parts.len() != 2 {
                return respond(stream, 404, b"not found", "text/plain").await;
            }
            serde_json::to_vec(&registry.package(parts[0], parts[1])?)?
        }
        _ => return respond(stream, 404, b"not found", "text/plain").await,
    };
    respond(stream, 200, &response, "application/json").await
}

struct HttpRequest {
    method: String,
    path: String,
    authorization: Option<String>,
    body: Vec<u8>,
}

async fn read_request(stream: &mut TcpStream) -> Result<HttpRequest> {
    let mut buffer = Vec::new();
    let header_end = loop {
        if buffer.len() >= MAX_HEADER_BYTES {
            bail!("HTTP headers exceed limit");
        }
        let mut chunk = [0_u8; 2048];
        let read = stream.read(&mut chunk).await?;
        if read == 0 {
            bail!("HTTP request ended before headers");
        }
        buffer.extend_from_slice(&chunk[..read]);
        if let Some(index) = buffer.windows(4).position(|window| window == b"\r\n\r\n") {
            break index + 4;
        }
    };
    let headers = std::str::from_utf8(&buffer[..header_end])?;
    let mut lines = headers.split("\r\n");
    let request_line = lines
        .next()
        .ok_or_else(|| anyhow!("missing request line"))?;
    let parts = request_line.split_whitespace().collect::<Vec<_>>();
    if parts.len() != 3 || parts[2] != "HTTP/1.1" {
        bail!("unsupported HTTP request line");
    }
    if !matches!(parts[0], "GET" | "PUT" | "POST") {
        bail!("unsupported HTTP method");
    }
    let mut content_length = 0usize;
    let mut content_length_seen = false;
    let mut authorization = None;
    for line in lines.filter(|line| !line.is_empty()) {
        let (name, value) = line
            .split_once(':')
            .ok_or_else(|| anyhow!("malformed HTTP header"))?;
        match name.to_ascii_lowercase().as_str() {
            "content-length" => {
                if content_length_seen {
                    bail!("duplicate Content-Length header");
                }
                content_length_seen = true;
                content_length = value.trim().parse()?;
            }
            "transfer-encoding" => bail!("Transfer-Encoding is not supported"),
            "authorization" => authorization = Some(value.trim().to_string()),
            _ => {}
        }
    }
    if content_length > MAX_BODY_BYTES {
        bail!("HTTP request body exceeds 2 MiB");
    }
    let mut body = buffer[header_end..].to_vec();
    if body.len() > content_length {
        body.truncate(content_length);
    }
    while body.len() < content_length {
        let remaining = content_length - body.len();
        let mut chunk = vec![0_u8; remaining.min(16 * 1024)];
        let read = stream.read(&mut chunk).await?;
        if read == 0 {
            bail!("HTTP request body ended early");
        }
        body.extend_from_slice(&chunk[..read]);
    }
    Ok(HttpRequest {
        method: parts[0].to_string(),
        path: parts[1].to_string(),
        authorization,
        body,
    })
}

async fn respond(
    stream: &mut TcpStream,
    status: u16,
    body: &[u8],
    content_type: &str,
) -> Result<()> {
    let reason = match status {
        200 => "OK",
        401 => "Unauthorized",
        404 => "Not Found",
        _ => "Error",
    };
    let headers = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\nX-Content-Type-Options: nosniff\r\n\r\n",
        body.len()
    );
    stream.write_all(headers.as_bytes()).await?;
    stream.write_all(body).await?;
    stream.shutdown().await?;
    Ok(())
}

fn token_matches(header: Option<&str>, expected: &str) -> bool {
    let Some(token) = header.and_then(|value| value.strip_prefix("Bearer ")) else {
        return false;
    };
    token.len() == expected.len() && bool::from(token.as_bytes().ct_eq(expected.as_bytes()))
}

fn registry_root() -> Result<PathBuf> {
    std::env::var_os("FOCALDESK_AI_REGISTRY_ROOT")
        .map(PathBuf::from)
        .or_else(|| dirs_fallback().map(|path| path.join("focaldesk/private-registry")))
        .context("cannot resolve private registry root")
}

fn dirs_fallback() -> Option<PathBuf> {
    std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".local/share")))
}

fn open_registry(root: &Path, registry_id: &str) -> Result<FaiPrivateRegistry> {
    let secret = focaldesk_secrets_client::get(&catalog_key_handle(registry_id))?;
    let key = decode_key(&secret)?;
    FaiPrivateRegistry::open(root, &key)
}

fn load_or_generate_secret(handle: &str, byte_count: usize) -> Result<Zeroizing<String>> {
    match focaldesk_secrets_client::get(handle) {
        Ok(value) => Ok(value),
        Err(error) if error.to_string().contains("not found") => {
            let mut bytes = Zeroizing::new(vec![0_u8; byte_count]);
            rand::rngs::OsRng.fill_bytes(&mut bytes);
            let encoded = Zeroizing::new(encode_hex(&bytes));
            focaldesk_secrets_client::set(handle, &encoded, "FocalDesk private AIOS registry")?;
            Ok(encoded)
        }
        Err(error) => Err(error),
    }
}

fn load_or_seed_token(handle: &str, file_environment: &str) -> Result<Zeroizing<String>> {
    match focaldesk_secrets_client::get(handle) {
        Ok(value) => Ok(value),
        Err(error) if error.to_string().contains("not found") => {
            let path =
                PathBuf::from(std::env::var_os(file_environment).with_context(|| {
                    format!("{file_environment} is required for initial setup")
                })?);
            let metadata = std::fs::symlink_metadata(&path)?;
            use std::os::unix::fs::PermissionsExt;
            if metadata.file_type().is_symlink()
                || !metadata.is_file()
                || metadata.len() > 4096
                || metadata.permissions().mode() & 0o077 != 0
            {
                bail!("registry bootstrap token file is unsafe");
            }
            let token = Zeroizing::new(std::fs::read_to_string(path)?.trim().to_string());
            if token.len() < 32 || token.len() > 512 {
                bail!("registry bootstrap token length is out of bounds");
            }
            focaldesk_secrets_client::set(handle, &token, "FocalDesk private AIOS registry")?;
            Ok(token)
        }
        Err(error) => Err(error),
    }
}

fn decode_key(value: &str) -> Result<Zeroizing<[u8; 32]>> {
    if value.len() != 64 || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        bail!("registry signing key is malformed");
    }
    let mut key = Zeroizing::new([0_u8; 32]);
    for (index, byte) in key.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&value[index * 2..index * 2 + 2], 16)?;
    }
    Ok(key)
}

fn encode_hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn catalog_key_handle(id: &str) -> String {
    format!("aios/registries/{id}/catalog-ed25519")
}

fn read_token_handle(id: &str) -> String {
    format!("aios/registries/{id}/read-token")
}

fn publish_token_handle(id: &str) -> String {
    format!("aios/registries/{id}/publish-token")
}

fn audit(root: &Path, action: &str, subject: &str, outcome: &str) -> Result<()> {
    let event = AuditEvent {
        at_unix: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs(),
        action,
        subject,
        outcome,
    };
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .mode(0o600)
        .open(root.join("audit.jsonl"))?;
    serde_json::to_writer(&mut file, &event)?;
    writeln!(file)?;
    file.sync_data()?;
    Ok(())
}
