//! Explicit, bounded, credential-free image loading. The webview never fetches URLs.
use crate::{html_layout, Message};
use anyhow::{bail, ensure, Context, Result};
use serde::Serialize;
use std::{
    io::Read,
    net::{IpAddr, SocketAddr, ToSocketAddrs},
    time::{Duration, Instant},
};
#[derive(Serialize)]
pub struct Loaded {
    pub index: usize,
    pub data_url: Option<String>,
    pub error: Option<String>,
}
fn public_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => {
            let [a, b, c, _] = ip.octets();
            !(ip.is_private()
                || ip.is_loopback()
                || ip.is_link_local()
                || ip.is_unspecified()
                || ip.is_multicast()
                || ip.is_broadcast()
                || a == 0
                || a >= 224
                || (a == 100 && (64..=127).contains(&b))
                || (a == 192 && b == 0)
                || (a == 198 && (b == 18 || b == 19 || b == 51))
                || (a == 203 && b == 0 && c == 113))
        }
        IpAddr::V6(ip) => {
            // Only global unicast; reject mapped, local, documentation and transition ranges.
            let s = ip.segments();
            s[0] & 0xe000 == 0x2000
                && s[0] != 0x2002
                && !(s[0] == 0x2001 && (s[1] < 0x200 || s[1] == 0xdb8))
        }
    }
}
fn target(value: &str) -> Result<(url::Url, String, u16)> {
    let clean = crate::html::safe_link(value).context("Invalid image URL")?;
    let u = url::Url::parse(&clean)?;
    ensure!(
        u.scheme() == "https" && u.port_or_known_default() == Some(443),
        "Only public HTTPS images on port 443 are supported"
    );
    let host = u.host_str().context("Image host is missing")?.to_string();
    ensure!(
        !host.eq_ignore_ascii_case("localhost")
            && !host.ends_with(".localhost")
            && !host.ends_with(".local"),
        "Local image hosts are blocked"
    );
    if let Some(url::Host::Ipv4(ip)) = u.host() {
        ensure!(public_ip(IpAddr::V4(ip)), "Private image hosts are blocked");
    }
    if let Some(url::Host::Ipv6(ip)) = u.host() {
        ensure!(public_ip(IpAddr::V6(ip)), "Private image hosts are blocked");
    }
    Ok((u, host, 443))
}
fn fetch(value: &str, remaining: usize, deadline: Instant) -> Result<String> {
    ensure!(remaining > 128, "The image display budget is exhausted");
    let mut next = value.to_owned();
    for _ in 0..=3 {
        ensure!(Instant::now() < deadline, "Image loading timed out");
        let (u, host, port) = target(&next)?;
        // Resolve once, validate every result, and pin this request to that resolution.
        // No environment proxy can bypass the address checks or add credentials.
        let addrs = (host.trim_matches(['[', ']']), port)
            .to_socket_addrs()
            .context("Image host could not be resolved")?
            .collect::<Vec<SocketAddr>>();
        ensure!(
            !addrs.is_empty() && addrs.iter().all(|a| public_ip(a.ip())),
            "Image host resolved to a private address"
        );
        let client = reqwest::blocking::Client::builder()
            .no_proxy()
            .referer(false)
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(Duration::from_secs(4))
            .timeout(
                deadline
                    .saturating_duration_since(Instant::now())
                    .min(Duration::from_secs(8)),
            )
            .resolve_to_addrs(host.trim_matches(['[', ']']), &addrs)
            .build()?;
        let response = client
            .get(u.clone())
            .header(
                reqwest::header::ACCEPT,
                "image/png,image/jpeg,image/gif,image/webp",
            )
            .send()
            .context("Image could not be downloaded")?;
        if response.status().is_redirection() {
            let location = response
                .headers()
                .get(reqwest::header::LOCATION)
                .context("Image redirect is missing its destination")?
                .to_str()?;
            next = u.join(location)?.to_string();
            continue;
        }
        ensure!(
            response.status().is_success(),
            "Image server returned an error"
        );
        let mime = response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|h| h.to_str().ok())
            .unwrap_or_default()
            .split(';')
            .next()
            .unwrap_or_default()
            .trim()
            .to_ascii_lowercase();
        ensure!(
            html_layout::raster_type(&mime),
            "Only PNG, JPEG, GIF and WebP images are supported"
        );
        let max = remaining.min(html_layout::MAX_INLINE_BYTES);
        ensure!(
            response.content_length().is_none_or(|n| n <= max as u64),
            "Image exceeds the size limit"
        );
        let mut bytes = vec![];
        response.take(max as u64 + 1).read_to_end(&mut bytes)?;
        ensure!(bytes.len() <= max, "Image exceeds the size limit");
        let data = html_layout::data_image(&mime, &bytes)
            .context("Image type and file contents do not match")?;
        ensure!(
            data.len() <= remaining,
            "The image display budget is exhausted"
        );
        return Ok(data);
    }
    bail!("Too many image redirects")
}
pub fn load(message: &Message) -> Result<Vec<Loaded>> {
    let doc = message
        .html
        .as_ref()
        .context("No formatted message is cached")?;
    let deadline = Instant::now() + Duration::from_secs(30);
    let inline = doc
        .images
        .iter()
        .filter_map(|i| i.inline_data.as_ref())
        .map(String::len)
        .sum::<usize>();
    let mut remaining = html_layout::MAX_IMAGE_TOTAL.saturating_sub(inline);
    Ok(doc
        .images
        .iter()
        .filter(|i| i.source.as_ref().is_some_and(|s| s.starts_with("https://")))
        .take(20)
        .map(
            |image| match fetch(image.source.as_ref().unwrap(), remaining, deadline) {
                Ok(data) => {
                    remaining = remaining.saturating_sub(data.len());
                    Loaded {
                        index: image.index,
                        data_url: Some(data),
                        error: None,
                    }
                }
                Err(error) => Loaded {
                    index: image.index,
                    data_url: None,
                    error: Some(error.to_string().chars().take(180).collect()),
                },
            },
        )
        .collect())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn image_targets_reject_local_credentials_schemes_ports_and_special_ranges() {
        for value in [
            "http://example.com/a.png",
            "https://localhost/a",
            "https://example.local/a",
            "https://127.0.0.1/a",
            "https://10.0.0.2/a",
            "https://169.254.169.254/a",
            "https://[::1]/a",
            "https://user:pass@example.com/a",
            "https://example.com:8443/a",
            "data:image/png;base64,aaaa",
            "https://100.64.0.1/a",
        ] {
            assert!(target(value).is_err(), "{value}");
        }
        assert!(target("https://example.com/image.png").is_ok());
        assert!(!public_ip("::ffff:127.0.0.1".parse().unwrap()));
        assert!(!public_ip("2001:db8::1".parse().unwrap()));
        assert!(public_ip("8.8.8.8".parse().unwrap()));
    }
}
