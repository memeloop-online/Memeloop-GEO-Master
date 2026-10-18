//! Bounded, single-page capture. No browser execution or linked-page crawling.

use std::time::Duration;

use async_trait::async_trait;
use chrono::Utc;
use geo_domain::{AppError, ErrorCode, FetchedUrlPage, UrlPageFetcher};
use reqwest::{Url, header};

const FETCH_DEADLINE: Duration = Duration::from_secs(20);
const MAX_REDIRECTS: usize = 3;
const MAX_RESPONSE_BYTES: usize = 2 * 1024 * 1024;
const MAX_TEXT_BYTES: usize = 1024 * 1024;
pub(crate) const URL_PARSER_VERSION: &str = "html2text-0.17.4-v1";

#[derive(Debug)]
pub(crate) struct PublicUrlPageFetcher;

#[async_trait]
impl UrlPageFetcher for PublicUrlPageFetcher {
    async fn fetch(&self, url: &str) -> Result<FetchedUrlPage, AppError> {
        fetch_page(url).await
    }
}

fn unavailable(message: &'static str) -> AppError {
    AppError::new(ErrorCode::DependencyUnavailable, message)
}

/// The model endpoint policy deliberately rejects queries; web pages need them.
/// Only its validation/client URL is stripped. Requests retain the original URL
/// and therefore use the exact same host, port, DNS pins and TLS identity.
fn client_url(url: &Url) -> Result<Url, AppError> {
    if url.as_str().len() > 2048 {
        return Err(AppError::invalid_request("webpage URL is too long"));
    }
    let mut endpoint = url.clone();
    endpoint.set_query(None);
    endpoint.set_fragment(None);
    geo_provider::validate_public_endpoint(endpoint.as_str()).map_err(|_| {
        AppError::invalid_request("webpage URL must use a public HTTP or HTTPS address")
    })
}

fn redirect_target(current: &Url, location: &str) -> Result<Url, AppError> {
    let target = current
        .join(location)
        .map_err(|_| AppError::invalid_request("webpage returned an invalid redirect"))?;
    client_url(&target)?;
    Ok(target)
}

/// One deadline covers DNS, every redirect, body reads and parser completion.
/// Each hop gets a new client, which revalidates and pins every DNS answer and
/// disables automatic redirects, retries and inherited workstation proxies.
async fn fetch_page(value: &str) -> Result<FetchedUrlPage, AppError> {
    let requested =
        Url::parse(value).map_err(|_| AppError::invalid_request("invalid webpage URL"))?;
    tokio::time::timeout(FETCH_DEADLINE, fetch_page_inner(requested))
        .await
        .map_err(|_| unavailable("webpage capture exceeded its time limit"))?
}

async fn fetch_page_inner(requested: Url) -> Result<FetchedUrlPage, AppError> {
    let mut current = requested.clone();
    for redirects in 0..=MAX_REDIRECTS {
        let endpoint = client_url(&current)?;
        let client = geo_provider::public_endpoint_client(&endpoint)
            .await
            .map_err(|error| match error {
                geo_provider::ProviderError::InvalidRequest(_) => {
                    AppError::invalid_request("webpage URL must resolve only to public addresses")
                }
                _ => unavailable("webpage address could not be resolved"),
            })?;
        let mut response = client
            .get(current.clone())
            .header(
                header::ACCEPT,
                "text/html, application/xhtml+xml, text/plain, text/markdown",
            )
            .header(header::ACCEPT_ENCODING, "identity")
            .send()
            .await
            .map_err(|_| unavailable("webpage request failed"))?;
        if response.status().is_redirection() {
            if redirects == MAX_REDIRECTS {
                return Err(AppError::invalid_request(
                    "webpage exceeded its redirect limit",
                ));
            }
            let location = response
                .headers()
                .get(header::LOCATION)
                .and_then(|value| value.to_str().ok())
                .ok_or_else(|| {
                    AppError::invalid_request("webpage redirect has no valid location")
                })?;
            current = redirect_target(&current, location)?;
            continue;
        }
        if !response.status().is_success() {
            return Err(unavailable("webpage returned an unsuccessful response"));
        }
        let media_type = response
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.split(';').next())
            .unwrap_or("")
            .trim()
            .to_ascii_lowercase();
        validate_media_type(&media_type)?;
        if response
            .headers()
            .get(header::CONTENT_ENCODING)
            .is_some_and(|value| value.as_bytes() != b"identity")
        {
            return Err(AppError::invalid_request(
                "webpage response uses an unsupported content encoding",
            ));
        }
        if response
            .content_length()
            .is_some_and(|length| length > MAX_RESPONSE_BYTES as u64)
        {
            return Err(AppError::invalid_request(
                "webpage response exceeds the size limit",
            ));
        }
        let mut raw_bytes = Vec::new();
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|_| unavailable("webpage response could not be read"))?
        {
            if chunk.len() > MAX_RESPONSE_BYTES - raw_bytes.len() {
                return Err(AppError::invalid_request(
                    "webpage response exceeds the size limit",
                ));
            }
            raw_bytes.extend_from_slice(&chunk);
        }
        let captured_at = Utc::now();
        // HTML parsing is CPU work. The input is bounded before entering the
        // blocking pool; timing out drops its result rather than saving it.
        let (raw_bytes, text, media_type) = tokio::task::spawn_blocking(move || {
            let text = parse_page(&raw_bytes, &media_type)?;
            Ok::<_, AppError>((raw_bytes, text, media_type))
        })
        .await
        .map_err(|_| unavailable("webpage text could not be parsed"))??;
        return Ok(FetchedUrlPage {
            requested_url: requested.to_string(),
            final_url: current.to_string(),
            captured_at,
            raw_bytes,
            media_type,
            text,
            parser_version: URL_PARSER_VERSION.to_owned(),
        });
    }
    unreachable!("redirect limit returns before the loop ends")
}

fn validate_media_type(media_type: &str) -> Result<(), AppError> {
    if matches!(
        media_type,
        "text/html" | "application/xhtml+xml" | "text/plain" | "text/markdown"
    ) {
        Ok(())
    } else {
        Err(AppError::invalid_request(
            "webpage must return HTML or plain text",
        ))
    }
}

fn parse_page(bytes: &[u8], media_type: &str) -> Result<String, AppError> {
    validate_media_type(media_type)?;
    if bytes.len() > MAX_RESPONSE_BYTES {
        return Err(AppError::invalid_request(
            "webpage response exceeds the size limit",
        ));
    }
    let utf8 = std::str::from_utf8(bytes)
        .map_err(|_| AppError::invalid_request("webpage must contain UTF-8 text"))?
        .trim_start_matches('\u{feff}');
    let text = if matches!(media_type, "text/html" | "application/xhtml+xml") {
        html2text::from_read(utf8.as_bytes(), 120)
            .map_err(|_| AppError::invalid_request("webpage HTML could not be parsed"))?
    } else {
        utf8.to_owned()
    };
    let text = text.trim();
    if text.is_empty() {
        return Err(AppError::invalid_request("webpage has no readable text"));
    }
    if text.len() > MAX_TEXT_BYTES {
        return Err(AppError::invalid_request(
            "webpage text exceeds the size limit",
        ));
    }
    Ok(text.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maintained_html_parser_extracts_entities_structure_and_unicode() {
        let html = "<!doctype html><html><head><style>.x{color:red}</style><script>secretScript()</script></head><body><h1>产品 &amp; 说明</h1><p>First <strong>fact</strong>.</p><table><tr><th>Size</th><th>Unit</th></tr><tr><td>12</td><td>cm</td></tr></table></body></html>";
        let text = parse_page(html.as_bytes(), "text/html").unwrap();
        for expected in ["产品 & 说明", "First", "fact", "Size", "12", "cm"] {
            assert!(text.contains(expected), "missing {expected}: {text}");
        }
        assert!(!text.contains("secretScript"));
        assert!(!text.contains("color:red"));
        assert!(!text.contains("<strong>"));
    }

    #[test]
    fn text_preserves_content_without_treating_it_as_html() {
        assert_eq!(
            parse_page("\u{feff}  literal <tag>\n中文  ".as_bytes(), "text/plain").unwrap(),
            "literal <tag>\n中文"
        );
    }

    #[test]
    fn unsupported_empty_invalid_and_oversize_content_is_rejected() {
        assert!(parse_page(b"data", "application/pdf").is_err());
        assert!(parse_page(b" \n ", "text/plain").is_err());
        assert!(parse_page(b"<script>onlyCode()</script>", "text/html").is_err());
        assert!(parse_page(&[0xff], "text/plain").is_err());
        assert!(parse_page(&vec![b'a'; MAX_TEXT_BYTES + 1], "text/plain").is_err());
        assert!(parse_page(&vec![b'a'; MAX_RESPONSE_BYTES + 1], "text/html").is_err());
    }

    #[tokio::test]
    async fn public_url_query_is_preserved_for_request_but_not_client_validation() {
        // Literal public IP requires no DNS or connection in this boundary test.
        let url = Url::parse("https://8.8.8.8/page?view=full#section").unwrap();
        let endpoint = client_url(&url).unwrap();
        assert_eq!(endpoint.as_str(), "https://8.8.8.8/page");
        let client = geo_provider::public_endpoint_client(&endpoint)
            .await
            .unwrap();
        let request = client.get(url.clone()).build().unwrap();
        assert_eq!(request.url(), &url);
        assert_eq!(request.url().query(), Some("view=full"));
        assert_eq!(request.url().host_str(), endpoint.host_str());
    }

    #[test]
    fn relative_redirects_keep_queries_and_private_or_credentialed_targets_fail() {
        let current = Url::parse("https://www.example.org/docs/page?old=1").unwrap();
        let next = redirect_target(&current, "../article?lang=zh#intro").unwrap();
        assert_eq!(
            next.as_str(),
            "https://www.example.org/article?lang=zh#intro"
        );
        for location in [
            "http://127.0.0.1/private?x=1",
            "http://169.254.169.254/metadata",
            "//localhost/internal",
            "http://[::1]/internal",
            "https://user:password@www.example.org/",
            "file:///etc/passwd",
        ] {
            assert!(redirect_target(&current, location).is_err());
        }
        assert!(
            client_url(&Url::parse("https://user:password@www.example.org/?x=1").unwrap()).is_err()
        );
    }
}
