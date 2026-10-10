//! Tenant-supplied endpoints must connect directly to validated public addresses.
//! Operator-owned inherited transports deliberately do not use this policy.

use std::{
    net::{IpAddr, SocketAddr},
    sync::Arc,
    time::Duration,
};

use reqwest::{
    Client,
    dns::{Name, Resolve, Resolving},
    redirect::Policy,
};
use url::{Host, Url};

use crate::ProviderError;

const DNS_TIMEOUT: Duration = Duration::from_secs(5);

fn rejected() -> ProviderError {
    ProviderError::InvalidRequest("custom model endpoint must use a public address".into())
}

/// Save-time checks do not contact DNS or a provider. DNS names are checked again
/// at execution time; a saved hostname is never an authorization to connect.
pub fn validate_public_endpoint(value: &str) -> Result<Url, ProviderError> {
    let url = Url::parse(value).map_err(|_| rejected())?;
    if value.len() > 2048
        || !matches!(url.scheme(), "http" | "https")
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
        || url.port_or_known_default().is_none_or(|port| port == 0)
    {
        return Err(rejected());
    }
    match url.host().ok_or_else(rejected)? {
        Host::Ipv4(ip) if !public_address(ip.into()) => return Err(rejected()),
        Host::Ipv6(ip) if !public_address(ip.into()) => return Err(rejected()),
        Host::Domain(name) => {
            let name = name.trim_end_matches('.');
            if name.is_empty() || name == "localhost" || name.ends_with(".localhost") {
                return Err(rejected());
            }
        }
        _ => {}
    }
    Ok(url)
}

/// Conservative global-unicast policy. Standard-library URL/IP parsing handles
/// alternate numeric spellings; special-use ranges are not model destinations.
/// IPv6 transition/translation ranges are excluded, not trusted as public IPv4.
fn public_address(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => {
            let [a, b, c, _] = ip.octets();
            !(ip.is_private()
                || ip.is_loopback()
                || ip.is_link_local()
                || ip.is_documentation()
                || a == 0
                || a >= 224
                || (a == 100 && (64..=127).contains(&b))
                || (a == 192 && b == 0 && c == 0)
                || (a == 192 && b == 88 && c == 99)
                || (a == 198 && (18..=19).contains(&b)))
        }
        IpAddr::V6(ip) => {
            if let Some(ip) = ip.to_ipv4_mapped() {
                return public_address(IpAddr::V4(ip));
            }
            let [a, b, ..] = ip.segments();
            (a & 0xe000 == 0x2000)
                && !(a == 0x2001 && (b < 0x0200 || b == 0x0db8))
                && a != 0x2002
                && !(a == 0x3fff && b < 0x1000)
        }
    }
}

/// Defense in depth: the client's override map is the only permitted resolver.
/// A hostname mismatch must fail rather than silently fall back to system DNS.
struct NoUnpinnedDns;
impl Resolve for NoUnpinnedDns {
    fn resolve(&self, _: Name) -> Resolving {
        Box::pin(async { Err(std::io::Error::other("unbound model endpoint").into()) })
    }
}

#[derive(Clone, Copy)]
enum AddressPolicy {
    Public,
    #[cfg(test)]
    LoopbackForTest,
}

fn pinned_client(
    url: &Url,
    addresses: Vec<SocketAddr>,
    policy: AddressPolicy,
) -> Result<Client, ProviderError> {
    if addresses.is_empty()
        || addresses.iter().any(|address| {
            let allowed = match policy {
                AddressPolicy::Public => public_address(address.ip()),
                #[cfg(test)]
                AddressPolicy::LoopbackForTest => address.ip().is_loopback(),
            };
            !allowed || Some(address.port()) != url.port_or_known_default()
        })
    {
        return Err(rejected());
    }
    Client::builder()
        .no_proxy()
        .redirect(Policy::none())
        .retry(reqwest::retry::never())
        .dns_resolver(Arc::new(NoUnpinnedDns))
        .resolve_to_addrs(url.host_str().ok_or_else(rejected)?, &addresses)
        .build()
        .map_err(|_| ProviderError::Transport("HTTP client initialization failed".into()))
}

async fn resolve_and_pin<F>(
    url: &Url,
    resolution: F,
    policy: AddressPolicy,
) -> Result<Client, ProviderError>
where
    F: std::future::Future<Output = Result<Vec<SocketAddr>, ProviderError>>,
{
    let addresses = tokio::time::timeout(DNS_TIMEOUT, resolution)
        .await
        .map_err(|_| ProviderError::Timeout)??;
    pinned_client(url, addresses, policy)
}

/// Use a fresh client per request: validate every DNS answer, then pin exactly
/// those addresses. Keep the original URL for TLS certificate checks and SNI.
/// Callers must bound the entire request, including resolution and body reads.
pub async fn public_endpoint_client(url: &Url) -> Result<Client, ProviderError> {
    validate_public_endpoint(url.as_str())?;
    let port = url.port_or_known_default().ok_or_else(rejected)?;
    let resolution = async {
        match url.host().ok_or_else(rejected)? {
            Host::Ipv4(ip) => Ok(vec![SocketAddr::new(ip.into(), port)]),
            Host::Ipv6(ip) => Ok(vec![SocketAddr::new(ip.into(), port)]),
            Host::Domain(name) => tokio::net::lookup_host((name, port))
                .await
                .map(|addresses| addresses.collect())
                .map_err(|_| {
                    ProviderError::Transport("custom model endpoint resolution failed".into())
                }),
        }
    };
    resolve_and_pin(url, resolution, AddressPolicy::Public).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::TcpListener,
    };

    #[test]
    fn special_use_addresses_and_alternate_spellings_are_rejected() {
        for address in [
            "0.0.0.0",
            "0.1.2.3",
            "10.0.0.1",
            "100.64.0.1",
            "100.127.255.255",
            "127.0.0.1",
            "169.254.169.254",
            "172.16.0.1",
            "172.31.255.255",
            "192.0.0.9",
            "192.0.2.1",
            "192.88.99.1",
            "192.168.1.1",
            "198.18.0.1",
            "198.19.255.255",
            "198.51.100.1",
            "203.0.113.1",
            "224.0.0.1",
            "239.1.1.1",
            "240.0.0.1",
            "255.255.255.255",
            "::",
            "::1",
            "::ffff:127.0.0.1",
            "::ffff:10.0.0.1",
            "::127.0.0.1",
            "64:ff9b::a00:1",
            "64:ff9b:1::1",
            "100::1",
            "2001::1",
            "2001:2::1",
            "2001:20::1",
            "2001:db8::1",
            "2002:a00:1::1",
            "3fff::1",
            "fc00::1",
            "fd00::1",
            "fe80::1",
            "fec0::1",
            "ff02::1",
        ] {
            assert!(!public_address(address.parse().unwrap()), "{address}");
        }
        for address in ["8.8.8.8", "1.1.1.1", "2606:4700::1111", "::ffff:8.8.8.8"] {
            assert!(public_address(address.parse().unwrap()), "{address}");
        }
        for endpoint in [
            "http://127.1/v1",
            "http://2130706433/v1",
            "http://0x7f000001/v1",
            "http://0177.0.0.1/v1",
            "http://[::ffff:127.0.0.1]/v1",
            "http://LOCALHOST./v1",
            "http://sub.localhost/v1",
            "http://user:secret@example.org/v1",
            "https://example.org/v1?key=secret",
            "https://example.org/v1#fragment",
            "ftp://example.org/v1",
        ] {
            assert!(validate_public_endpoint(endpoint).is_err(), "{endpoint}");
        }
        assert!(validate_public_endpoint("https://models.example.org/v1").is_ok());
    }

    #[tokio::test]
    async fn empty_or_mixed_dns_answers_fail_closed_without_connecting() {
        let url = validate_public_endpoint("https://models.example.invalid/v1").unwrap();
        for addresses in [
            vec![],
            vec![
                "8.8.8.8:443".parse().unwrap(),
                "127.0.0.1:443".parse().unwrap(),
            ],
            vec!["[::ffff:169.254.169.254]:443".parse().unwrap()],
            vec!["8.8.8.8:80".parse().unwrap()],
        ] {
            assert!(
                resolve_and_pin(&url, async { Ok(addresses) }, AddressPolicy::Public)
                    .await
                    .is_err()
            );
        }
    }

    // The loopback policy exists only inside this crate's unit tests. It is not
    // an environment switch, feature, or callable production constructor.
    #[tokio::test]
    async fn pinned_get_and_post_preserve_host_and_never_follow_redirects() {
        for method in [reqwest::Method::GET, reqwest::Method::POST] {
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let address = listener.local_addr().unwrap();
            let url = validate_public_endpoint(&format!(
                "http://models.example.invalid:{}/v1/models",
                address.port()
            ))
            .unwrap();
            let server = tokio::spawn(async move {
                let (mut stream, _) = listener.accept().await.unwrap();
                let mut bytes = Vec::new();
                while !bytes.windows(4).any(|bytes| bytes == b"\r\n\r\n") {
                    let mut chunk = [0; 1024];
                    let count = stream.read(&mut chunk).await.unwrap();
                    assert!(count > 0);
                    bytes.extend_from_slice(&chunk[..count]);
                }
                stream.write_all(b"HTTP/1.1 302 Found\r\nLocation: http://127.0.0.1:1/private\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").await.unwrap();
                String::from_utf8(bytes).unwrap()
            });
            let resolutions = std::sync::atomic::AtomicUsize::new(0);
            let client = resolve_and_pin(
                &url,
                async {
                    resolutions.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    Ok(vec![address])
                },
                AddressPolicy::LoopbackForTest,
            )
            .await
            .unwrap();
            let response = client
                .request(method.clone(), url)
                .bearer_auth("synthetic-key")
                .send()
                .await
                .unwrap();
            assert_eq!(response.status(), 302);
            assert_eq!(resolutions.load(std::sync::atomic::Ordering::SeqCst), 1);
            let headers = server.await.unwrap().to_ascii_lowercase();
            assert!(
                headers.starts_with(&format!("{} /v1/models ", method.as_str().to_lowercase()))
            );
            assert!(headers.contains(&format!(
                "host: models.example.invalid:{}\r\n",
                address.port()
            )));
            assert!(headers.contains("authorization: bearer synthetic-key\r\n"));
            // Even this client's other hostnames cannot use fallback DNS.
            assert!(
                client
                    .get("http://unbound.example.invalid/")
                    .send()
                    .await
                    .is_err()
            );
        }
    }

    #[test]
    fn system_proxy_cannot_override_pinned_destination() {
        const CHILD: &str = "GEO_PUBLIC_ENDPOINT_PROXY_TEST_CHILD";
        if std::env::var_os(CHILD).is_none() {
            // Child-process environment avoids unsound process-wide mutation
            // while the rest of the Rust tests run concurrently.
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "public_endpoint::tests::system_proxy_cannot_override_pinned_destination",
                ])
                .env(CHILD, "1")
                .env("HTTP_PROXY", "http://127.0.0.1:1")
                .env("HTTPS_PROXY", "http://127.0.0.1:1")
                .env("ALL_PROXY", "http://127.0.0.1:1")
                .env("http_proxy", "http://127.0.0.1:1")
                .env("https_proxy", "http://127.0.0.1:1")
                .env("all_proxy", "http://127.0.0.1:1")
                .env("NO_PROXY", "")
                .env("no_proxy", "")
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stdout)
            );
            return;
        }
        tokio::runtime::Runtime::new().unwrap().block_on(async {
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let address = listener.local_addr().unwrap();
            let url = validate_public_endpoint(&format!(
                "http://models.example.invalid:{}/v1/models",
                address.port()
            ))
            .unwrap();
            let server = tokio::spawn(async move {
                let (mut stream, _) = listener.accept().await.unwrap();
                let mut bytes = [0; 4096];
                assert!(stream.read(&mut bytes).await.unwrap() > 0);
                stream
                    .write_all(b"HTTP/1.1 204 No Content\r\nConnection: close\r\n\r\n")
                    .await
                    .unwrap();
            });
            let client =
                pinned_client(&url, vec![address], AddressPolicy::LoopbackForTest).unwrap();
            let response = client
                .get(url)
                .timeout(Duration::from_secs(2))
                .send()
                .await
                .unwrap();
            assert_eq!(response.status(), 204);
            server.await.unwrap();
        });
    }
}
