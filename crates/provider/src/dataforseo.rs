//! Thin Google Organic Standard asynchronous adapter over maintained reqwest.
//! Raw evidence must be saved before the separate pure response decoders run.
use std::time::Duration;

use reqwest::{Client, Method, redirect::Policy};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::serp::{
    MAX_SERP_RESPONSE_BYTES, SerpAdapterError, SerpItemKind, SerpOperation, SerpProviderItem,
    SerpProviderResult, SerpRawResponse, SerpSentCertainty, SerpTransportError,
};

const ORIGIN: &str = "https://api.dataforseo.com";
const BASE: &str = "/v3/serp/google/organic";
pub const DATAFORSEO_SERP_CONNECTOR_VERSION: &str = "dataforseo.google.organic.standard.v1";
pub const DATAFORSEO_SERP_PARSER_VERSION: &str = "dataforseo.google.organic.advanced.v1";

/// Fixed, redacted credential diagnostic. It is not a search-capability receipt.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DataForSeoConnectionStatus {
    Connected,
    AuthenticationFailed,
    Unavailable,
    InvalidResponse,
}

/// Construct only from a persisted, frozen attempt. Tag is correlation, not
/// provider idempotency. Paid POST requests must never be retried blindly.
#[derive(Clone, Serialize, Deserialize)]
pub struct DataForSeoTaskRequest {
    pub keyword: String,
    pub location_code: u32,
    pub language_code: String,
    pub tag: String,
}

impl DataForSeoTaskRequest {
    pub fn prepare(&self) -> Result<DataForSeoPreparedTask, SerpAdapterError> {
        let body = self.body().ok_or(SerpAdapterError::InvalidConfiguration)?;
        let bytes =
            serde_json::to_vec(&body).map_err(|_| SerpAdapterError::InvalidConfiguration)?;
        DataForSeoPreparedTask::from_bytes(bytes)
    }

    fn body(&self) -> Option<Value> {
        if self.keyword.trim().is_empty()
            || self.keyword.chars().count() > 700
            || self.keyword.chars().any(char::is_control)
            || self.location_code == 0
            || self.language_code.is_empty()
            || self.language_code.len() > 16
            || !self
                .language_code
                .bytes()
                .all(|c| c.is_ascii_alphabetic() || c == b'-')
            || self.tag.is_empty()
            || self.tag.len() > 255
            || self.tag.chars().any(char::is_control)
        {
            return None;
        }
        // The provider URL-decodes these characters even inside JSON. Preserve
        // the exact user query (including whitespace) through that decoding.
        let encoded = self.keyword.replace('%', "%25").replace('+', "%2B");
        Some(json!([{
            "keyword": encoded, "location_code": self.location_code,
            "language_code": self.language_code, "tag": self.tag,
            "device": "desktop", "os": "windows", "depth": 10,
            "max_crawl_pages": 1, "priority": 1,
        }]))
    }
}

/// Immutable validated wire bytes. Hash before recording the sending intent,
/// then send these same bytes; rebuilding an equivalent JSON value is not enough.
#[derive(Clone)]
pub struct DataForSeoPreparedTask {
    body: Vec<u8>,
    request_sha256: String,
    tag: String,
}

impl DataForSeoPreparedTask {
    pub fn from_bytes(body: Vec<u8>) -> Result<Self, SerpAdapterError> {
        if body.len() > 16_384 {
            return Err(SerpAdapterError::InvalidConfiguration);
        }
        let value: Value =
            serde_json::from_slice(&body).map_err(|_| SerpAdapterError::InvalidConfiguration)?;
        let array = value
            .as_array()
            .filter(|array| array.len() == 1)
            .ok_or(SerpAdapterError::InvalidConfiguration)?;
        let task = &array[0];
        let request = DataForSeoTaskRequest {
            keyword: task["keyword"]
                .as_str()
                .ok_or(SerpAdapterError::InvalidConfiguration)?
                .replace("%2B", "+")
                .replace("%25", "%"),
            location_code: task["location_code"]
                .as_u64()
                .and_then(|code| u32::try_from(code).ok())
                .ok_or(SerpAdapterError::InvalidConfiguration)?,
            language_code: task["language_code"]
                .as_str()
                .ok_or(SerpAdapterError::InvalidConfiguration)?
                .into(),
            tag: task["tag"]
                .as_str()
                .ok_or(SerpAdapterError::InvalidConfiguration)?
                .into(),
        };
        if request.body().as_ref() != Some(&value) {
            return Err(SerpAdapterError::InvalidConfiguration);
        }
        Ok(Self {
            request_sha256: sha256(&body),
            body,
            tag: request.tag,
        })
    }

    pub fn body(&self) -> &[u8] {
        &self.body
    }
    pub fn request_sha256(&self) -> &str {
        &self.request_sha256
    }
    pub fn tag(&self) -> &str {
        &self.tag
    }
}

fn sha256(bytes: &[u8]) -> String {
    hex::encode(ring::digest::digest(&ring::digest::SHA256, bytes))
}

pub fn dataforseo_read_request_sha256(task_id: &str) -> Result<String, SerpAdapterError> {
    if !valid_task_id(task_id) {
        return Err(SerpAdapterError::TaskIdentityMismatch);
    }
    Ok(sha256(
        format!("GET {BASE}/task_get/advanced/{task_id}").as_bytes(),
    ))
}

/// Credentials deliberately have no Debug or Serialize implementation.
pub struct DataForSeoClient {
    client: Client,
    origin: String,
    login: String,
    password: String,
    response_limit: usize,
}

impl DataForSeoClient {
    /// Official read-only account endpoint. Never sends a task, returns account
    /// data, follows redirects, retries, or records the response in tenant evidence.
    pub async fn test_connection(&self) -> DataForSeoConnectionStatus {
        let request = async {
            let mut response = self
                .client
                .get(format!("{}/v3/appendix/user_data", self.origin))
                .basic_auth(&self.login, Some(&self.password))
                .send()
                .await
                .map_err(|_| DataForSeoConnectionStatus::Unavailable)?;
            if matches!(response.status().as_u16(), 401 | 403) {
                return Err(DataForSeoConnectionStatus::AuthenticationFailed);
            }
            if response.status().as_u16() != 200 {
                return Err(DataForSeoConnectionStatus::Unavailable);
            }
            let mut bytes = Vec::new();
            while let Some(chunk) = response
                .chunk()
                .await
                .map_err(|_| DataForSeoConnectionStatus::Unavailable)?
            {
                if bytes.len() + chunk.len() > 256 * 1024 {
                    return Err(DataForSeoConnectionStatus::InvalidResponse);
                }
                bytes.extend_from_slice(&chunk);
            }
            let value: Value = serde_json::from_slice(&bytes)
                .map_err(|_| DataForSeoConnectionStatus::InvalidResponse)?;
            if value["status_code"] != 20000
                || value["tasks_count"] != 1
                || !value["tasks"]
                    .as_array()
                    .is_some_and(|tasks| tasks.len() == 1)
                || value["tasks"][0]["status_code"] != 20000
            {
                return Err(DataForSeoConnectionStatus::InvalidResponse);
            }
            Ok(DataForSeoConnectionStatus::Connected)
        };
        match tokio::time::timeout(Duration::from_secs(15), request).await {
            Ok(Ok(status)) => status,
            Ok(Err(status)) => status,
            Err(_) => DataForSeoConnectionStatus::Unavailable,
        }
    }
    pub fn new(login: String, password: String) -> Result<Self, SerpAdapterError> {
        Self::build(ORIGIN.into(), login, password, MAX_SERP_RESPONSE_BYTES)
    }

    fn build(
        origin: String,
        login: String,
        password: String,
        response_limit: usize,
    ) -> Result<Self, SerpAdapterError> {
        if login.is_empty()
            || password.is_empty()
            || login.len() > 512
            || password.len() > 512
            || login.contains(':')
            || login.chars().any(char::is_control)
            || password.chars().any(char::is_control)
        {
            return Err(SerpAdapterError::InvalidConfiguration);
        }
        let client = Client::builder()
            .redirect(Policy::none())
            .retry(reqwest::retry::never())
            .timeout(Duration::from_secs(60))
            .build()
            .map_err(|_| SerpAdapterError::InvalidConfiguration)?;
        Ok(Self {
            client,
            origin,
            login,
            password,
            response_limit,
        })
    }

    #[cfg(test)]
    fn loopback(origin: String, response_limit: usize) -> Result<Self, SerpAdapterError> {
        let url = url::Url::parse(&origin).map_err(|_| SerpAdapterError::InvalidConfiguration)?;
        if url.scheme() != "http"
            || !url.host_str().is_some_and(|host| {
                host.parse::<std::net::IpAddr>()
                    .is_ok_and(|ip| ip.is_loopback())
            })
            || !url.username().is_empty()
            || url.password().is_some()
            || url.path() != "/"
            || url.query().is_some()
            || url.fragment().is_some()
        {
            return Err(SerpAdapterError::InvalidConfiguration);
        }
        Self::build(
            origin.trim_end_matches('/').into(),
            "synthetic-login".into(),
            "synthetic-password".into(),
            response_limit,
        )
    }

    pub async fn post_task(&self, request: &DataForSeoTaskRequest) -> SerpRawResponse {
        let Ok(prepared) = request.prepare() else {
            return invalid_request(SerpOperation::PostTask);
        };
        self.post_prepared(&prepared).await
    }

    pub async fn post_prepared(&self, prepared: &DataForSeoPreparedTask) -> SerpRawResponse {
        self.send(
            SerpOperation::PostTask,
            Method::POST,
            format!("{BASE}/task_post"),
            Some(prepared.body.clone()),
        )
        .await
    }

    pub async fn get_task(&self, task_id: &str) -> SerpRawResponse {
        if !valid_task_id(task_id) {
            return invalid_request(SerpOperation::GetTask);
        }
        self.send(
            SerpOperation::GetTask,
            Method::GET,
            format!("{BASE}/task_get/advanced/{task_id}"),
            None,
        )
        .await
    }

    pub async fn tasks_ready(&self) -> SerpRawResponse {
        self.send(
            SerpOperation::TasksReady,
            Method::GET,
            format!("{BASE}/tasks_ready"),
            None,
        )
        .await
    }

    async fn send(
        &self,
        operation: SerpOperation,
        method: Method,
        path: String,
        body: Option<Vec<u8>>,
    ) -> SerpRawResponse {
        let mut request = self
            .client
            .request(method, format!("{}{path}", self.origin))
            .basic_auth(&self.login, Some(&self.password));
        if let Some(body) = body {
            request = request
                .header(reqwest::header::CONTENT_TYPE, "application/json")
                .body(body);
        }
        // Exactly one send. Even connect/timeout failures are conservative
        // unknown sends, not evidence that a paid POST can safely be repeated.
        let response = request.send().await;
        let mut response = match response {
            Ok(response) => response,
            Err(error) => {
                return SerpRawResponse {
                    operation,
                    http_status: None,
                    body: vec![],
                    body_complete: false,
                    sent: SerpSentCertainty::PossiblySent,
                    error: Some(if error.is_timeout() {
                        SerpTransportError::Timeout
                    } else {
                        SerpTransportError::TransportFailed
                    }),
                };
            }
        };
        let mut raw = SerpRawResponse {
            operation,
            http_status: Some(response.status().as_u16()),
            body: vec![],
            body_complete: false,
            sent: SerpSentCertainty::ResponseReceived,
            error: None,
        };
        loop {
            match response.chunk().await {
                Ok(Some(chunk)) => {
                    let remaining = self.response_limit - raw.body.len();
                    raw.body
                        .extend_from_slice(&chunk[..chunk.len().min(remaining)]);
                    if chunk.len() > remaining {
                        raw.error = Some(SerpTransportError::BodyTooLarge);
                        break;
                    }
                }
                Ok(None) => {
                    raw.body_complete = true;
                    break;
                }
                Err(error) => {
                    raw.error = Some(if error.is_timeout() {
                        SerpTransportError::Timeout
                    } else {
                        SerpTransportError::BodyReadFailed
                    });
                    break;
                }
            }
        }
        raw
    }
}

fn invalid_request(operation: SerpOperation) -> SerpRawResponse {
    SerpRawResponse {
        operation,
        http_status: None,
        body: vec![],
        body_complete: false,
        sent: SerpSentCertainty::NotSent,
        error: Some(SerpTransportError::InvalidRequest),
    }
}

fn valid_task_id(id: &str) -> bool {
    (8..=128).contains(&id.len()) && id.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'-')
}

fn envelope(raw: &SerpRawResponse, operation: SerpOperation) -> Result<Value, SerpAdapterError> {
    if raw.operation != operation {
        return Err(SerpAdapterError::OperationMismatch);
    }
    if !raw.body_complete || raw.error.is_some() {
        return Err(SerpAdapterError::IncompleteResponse);
    }
    if raw.http_status != Some(200) {
        return Err(SerpAdapterError::HttpRejected);
    }
    let value: Value =
        serde_json::from_slice(&raw.body).map_err(|_| SerpAdapterError::InvalidJson)?;
    if value["status_code"] != 20000
        || value["tasks_count"] != 1
        || !value["tasks"]
            .as_array()
            .is_some_and(|tasks| tasks.len() == 1)
    {
        return Err(SerpAdapterError::InvalidEnvelope);
    }
    Ok(value)
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DataForSeoPostedTask {
    pub task_id: String,
}

pub fn decode_dataforseo_post(
    raw: &SerpRawResponse,
    expected_tag: &str,
) -> Result<DataForSeoPostedTask, SerpAdapterError> {
    let value = envelope(raw, SerpOperation::PostTask)?;
    let task = &value["tasks"][0];
    let id = task["id"]
        .as_str()
        .filter(|id| valid_task_id(id))
        .ok_or(SerpAdapterError::TaskIdentityMismatch)?;
    if task["data"]["tag"] != expected_tag {
        return Err(SerpAdapterError::TaskIdentityMismatch);
    }
    if task["status_code"] != 20100 {
        return Err(SerpAdapterError::TaskStatusRejected);
    }
    Ok(DataForSeoPostedTask { task_id: id.into() })
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum DataForSeoTaskOutcome {
    Pending { provider_status_code: u32 },
    Failed { provider_status_code: u32 },
    Completed { result: SerpProviderResult },
}

pub fn decode_dataforseo_get(
    raw: &SerpRawResponse,
    expected_task_id: &str,
) -> Result<DataForSeoTaskOutcome, SerpAdapterError> {
    let value = envelope(raw, SerpOperation::GetTask)?;
    let task = &value["tasks"][0];
    if !valid_task_id(expected_task_id) || task["id"] != expected_task_id {
        return Err(SerpAdapterError::TaskIdentityMismatch);
    }
    let status = task["status_code"]
        .as_u64()
        .and_then(|value| u32::try_from(value).ok())
        .ok_or(SerpAdapterError::InvalidEnvelope)?;
    match status {
        40601 | 40602 => {
            return Ok(DataForSeoTaskOutcome::Pending {
                provider_status_code: status,
            });
        }
        20000 => {}
        _ => {
            return Ok(DataForSeoTaskOutcome::Failed {
                provider_status_code: status,
            });
        }
    }
    let results = task["result"]
        .as_array()
        .filter(|results| results.len() == 1)
        .ok_or(SerpAdapterError::InvalidResult)?;
    let result = &results[0];
    let raw_items = result["items"]
        .as_array()
        .ok_or(SerpAdapterError::InvalidResult)?;
    if raw_items.len() > 1000 {
        return Err(SerpAdapterError::InvalidResult);
    }
    let mut items = Vec::with_capacity(raw_items.len());
    for (index, item) in raw_items.iter().enumerate() {
        let raw_kind = item["type"]
            .as_str()
            .filter(|kind| !kind.is_empty() && kind.len() <= 128)
            .ok_or(SerpAdapterError::InvalidResult)?;
        let kind = match raw_kind {
            "organic" => SerpItemKind::Organic,
            "paid" => SerpItemKind::Paid,
            "featured_snippet" => SerpItemKind::FeaturedSnippet,
            "local_pack" => SerpItemKind::LocalPack,
            "ai_overview" => SerpItemKind::AiOverview,
            _ => SerpItemKind::Other,
        };
        let organic_rank = if kind == SerpItemKind::Organic {
            Some(positive_number(item, "rank_group")?.ok_or(SerpAdapterError::InvalidResult)?)
        } else {
            None
        };
        items.push(SerpProviderItem {
            kind,
            raw_kind: raw_kind.into(),
            raw_url: optional_string(item, "url", 8192)?,
            title: optional_string(item, "title", 16_384)?,
            page: positive_number(item, "page")?,
            organic_rank,
            absolute_position: positive_number(item, "rank_absolute")?,
            evidence_pointer: format!("/tasks/0/result/0/items/{index}"),
        });
    }
    let returned_organic_count = items
        .iter()
        .filter(|item| item.kind == SerpItemKind::Organic)
        .count();
    let max_returned_organic_rank = items.iter().filter_map(|item| item.organic_rank).max();
    Ok(DataForSeoTaskOutcome::Completed {
        result: SerpProviderResult {
            provider_reported_datetime: optional_string(result, "datetime", 128)?,
            request_echo: task.get("data").cloned().unwrap_or(Value::Null),
            provider_reported_pages: nonnegative_number(result, "pages_count")?,
            provider_reported_items: nonnegative_number(result, "items_count")?,
            requested_depth: 10,
            returned_organic_count,
            max_returned_organic_rank,
            coverage_verified: false,
            items,
        },
    })
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DataForSeoReadyTask {
    pub task_id: String,
    pub tag: Option<String>,
}

/// At most the provider's uncollected recent tasks; absence is inconclusive.
/// These are recovery candidates, never authorization to resubmit a paid POST.
pub fn decode_dataforseo_ready(
    raw: &SerpRawResponse,
) -> Result<Vec<DataForSeoReadyTask>, SerpAdapterError> {
    let value = envelope(raw, SerpOperation::TasksReady)?;
    let task = &value["tasks"][0];
    if task["status_code"] != 20000 {
        return Err(SerpAdapterError::TaskStatusRejected);
    }
    if task["result"].is_null() && task["result_count"] == 0 {
        return Ok(vec![]);
    }
    let tasks = task["result"]
        .as_array()
        .ok_or(SerpAdapterError::InvalidResult)?;
    if tasks.len() > 1000 {
        return Err(SerpAdapterError::InvalidResult);
    }
    tasks
        .iter()
        .map(|task| {
            Ok(DataForSeoReadyTask {
                task_id: task["id"]
                    .as_str()
                    .filter(|id| valid_task_id(id))
                    .ok_or(SerpAdapterError::TaskIdentityMismatch)?
                    .into(),
                tag: optional_string(task, "tag", 255)?,
            })
        })
        .collect()
}

fn optional_string(
    value: &Value,
    key: &str,
    max: usize,
) -> Result<Option<String>, SerpAdapterError> {
    match value.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(value)) if value.len() <= max => Ok(Some(value.clone())),
        _ => Err(SerpAdapterError::InvalidResult),
    }
}

fn nonnegative_number(value: &Value, key: &str) -> Result<Option<u32>, SerpAdapterError> {
    match value.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(number) => number
            .as_u64()
            .and_then(|number| u32::try_from(number).ok())
            .map(Some)
            .ok_or(SerpAdapterError::InvalidResult),
    }
}

fn positive_number(value: &Value, key: &str) -> Result<Option<u32>, SerpAdapterError> {
    let number = nonnegative_number(value, key)?;
    if number == Some(0) {
        return Err(SerpAdapterError::InvalidResult);
    }
    Ok(number)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::{TcpListener, TcpStream};

    const TASK_ID: &str = "00000000-0000-0000-0000-000000000001";

    fn request() -> DataForSeoTaskRequest {
        DataForSeoTaskRequest {
            keyword: "  C++ 100% + rain gauge  ".into(),
            location_code: 2840,
            language_code: "en".into(),
            tag: "synthetic-attempt".into(),
        }
    }

    fn raw(operation: SerpOperation, task: Value) -> SerpRawResponse {
        SerpRawResponse {
            operation,
            http_status: Some(200),
            body: serde_json::to_vec(
                &json!({"status_code":20000,"tasks_count":1,"tasks_error":0,"tasks":[task]}),
            )
            .unwrap(),
            body_complete: true,
            sent: SerpSentCertainty::ResponseReceived,
            error: None,
        }
    }

    async fn listen() -> (TcpListener, String) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        (listener, origin)
    }

    async fn read_request(stream: &mut TcpStream) -> Vec<u8> {
        let mut bytes = vec![];
        loop {
            let mut buffer = [0; 4096];
            let count = stream.read(&mut buffer).await.unwrap();
            assert!(count > 0);
            bytes.extend_from_slice(&buffer[..count]);
            assert!(bytes.len() < 16_384);
            if let Some(end) = bytes.windows(4).position(|part| part == b"\r\n\r\n") {
                let headers = String::from_utf8_lossy(&bytes[..end]).to_ascii_lowercase();
                let length: usize = headers
                    .lines()
                    .find_map(|line| {
                        line.strip_prefix("content-length:")
                            .map(|length| length.trim().parse().unwrap())
                    })
                    .unwrap_or(0);
                if bytes.len() >= end + 4 + length {
                    break;
                }
            }
        }
        bytes
    }

    #[tokio::test]
    async fn post_is_single_raw_first_request_with_exact_keyword_and_fixed_scope() {
        let (listener, origin) = listen().await;
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let request = read_request(&mut socket).await;
            let response = b"not-json-synthetic-raw-evidence";
            socket
                .write_all(
                    format!(
                        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                        response.len()
                    )
                    .as_bytes(),
                )
                .await
                .unwrap();
            socket.write_all(response).await.unwrap();
            request
        });
        let client = DataForSeoClient::loopback(origin, MAX_SERP_RESPONSE_BYTES).unwrap();
        let frozen_bytes = serde_json::to_vec_pretty(&request().body().unwrap()).unwrap();
        let prepared = DataForSeoPreparedTask::from_bytes(frozen_bytes.clone()).unwrap();
        assert_eq!(prepared.request_sha256(), sha256(&frozen_bytes));
        let response = client.post_prepared(&prepared).await;
        assert!(response.body_complete);
        assert_eq!(response.body, b"not-json-synthetic-raw-evidence");
        assert_eq!(
            decode_dataforseo_post(&response, "synthetic-attempt"),
            Err(SerpAdapterError::InvalidJson)
        );
        let sent = server.await.unwrap();
        let header_end = sent
            .windows(4)
            .position(|part| part == b"\r\n\r\n")
            .unwrap();
        let headers = String::from_utf8_lossy(&sent[..header_end]).to_ascii_lowercase();
        assert!(headers.starts_with("post /v3/serp/google/organic/task_post http/1.1"));
        assert!(headers.contains("authorization: basic "));
        let body: Value = serde_json::from_slice(&sent[header_end + 4..]).unwrap();
        assert_eq!(&sent[header_end + 4..], frozen_bytes);
        assert_eq!(
            body,
            json!([{
                "keyword":"  C%2B%2B 100%25 %2B rain gauge  ",
                "location_code":2840,"language_code":"en","tag":"synthetic-attempt",
                "device":"desktop","os":"windows","depth":10,"max_crawl_pages":1,"priority":1,
            }])
        );
        assert!(!format!("{response:?}").contains("synthetic-raw"));
    }

    #[tokio::test]
    async fn connection_diagnostic_only_gets_account_endpoint_and_redacts_response() {
        for (status, body, expected) in [
            (
                200,
                r#"{"status_code":20000,"tasks_count":1,"tasks":[{"status_code":20000,"result":[{"login":"synthetic-account","balance":123}]}]}"#,
                DataForSeoConnectionStatus::Connected,
            ),
            (
                401,
                "private diagnostic",
                DataForSeoConnectionStatus::AuthenticationFailed,
            ),
            (200, "not json", DataForSeoConnectionStatus::InvalidResponse),
            (302, "", DataForSeoConnectionStatus::Unavailable),
        ] {
            let (listener, origin) = listen().await;
            let server = tokio::spawn(async move {
                let (mut socket, _) = listener.accept().await.unwrap();
                let request = read_request(&mut socket).await;
                socket.write_all(format!("HTTP/1.1 {status} Synthetic\r\nContent-Length: {}\r\nLocation: /paid-task\r\nConnection: close\r\n\r\n{body}",body.len()).as_bytes()).await.unwrap();
                request
            });
            let client = DataForSeoClient::loopback(origin, MAX_SERP_RESPONSE_BYTES).unwrap();
            let result = client.test_connection().await;
            assert_eq!(result, expected);
            let request = server.await.unwrap();
            assert!(
                String::from_utf8_lossy(&request)
                    .starts_with("GET /v3/appendix/user_data HTTP/1.1")
            );
            let serialized = serde_json::to_string(&result).unwrap();
            assert!(!serialized.contains("synthetic-account"));
            assert!(!serialized.contains("balance"));
        }
    }

    #[test]
    fn prepared_request_freezes_exact_bytes_and_rejects_unsupported_mutations() {
        let mut input = request();
        input.keyword = "literal %2B and + and %25".into();
        let prepared = input.prepare().unwrap();
        assert_eq!(prepared.tag(), input.tag);
        assert_eq!(prepared.request_sha256(), sha256(prepared.body()));
        let copied = DataForSeoPreparedTask::from_bytes(prepared.body().to_vec()).unwrap();
        assert_eq!(copied.body(), prepared.body());
        let mut altered: Value = serde_json::from_slice(prepared.body()).unwrap();
        altered[0]["priority"] = json!(2);
        assert!(DataForSeoPreparedTask::from_bytes(serde_json::to_vec(&altered).unwrap()).is_err());
        altered[0]["priority"] = json!(1);
        altered[0]["pingback_url"] = json!("https://example.org/callback");
        assert!(DataForSeoPreparedTask::from_bytes(serde_json::to_vec(&altered).unwrap()).is_err());
        assert_ne!(
            dataforseo_read_request_sha256(TASK_ID).unwrap(),
            prepared.request_sha256()
        );
    }

    #[tokio::test]
    async fn partial_and_oversized_bodies_keep_bounded_evidence_before_decoding() {
        for (declared, body, limit, expected) in [
            (
                100,
                b"partial-evidence".as_slice(),
                1024,
                SerpTransportError::BodyReadFailed,
            ),
            (
                16,
                b"0123456789abcdef".as_slice(),
                8,
                SerpTransportError::BodyTooLarge,
            ),
        ] {
            let (listener, origin) = listen().await;
            let body = body.to_vec();
            let expected_bytes = body[..body.len().min(limit)].to_vec();
            let server = tokio::spawn(async move {
                let (mut socket, _) = listener.accept().await.unwrap();
                read_request(&mut socket).await;
                socket.write_all(format!("HTTP/1.1 200 OK\r\nContent-Length: {declared}\r\nConnection: close\r\n\r\n").as_bytes()).await.unwrap();
                socket.write_all(&body).await.unwrap();
                socket.shutdown().await.unwrap();
            });
            let client = DataForSeoClient::loopback(origin, limit).unwrap();
            let response = client.get_task(TASK_ID).await;
            server.await.unwrap();
            assert_eq!(response.body, expected_bytes);
            assert!(!response.body_complete);
            assert_eq!(response.http_status, Some(200));
            assert_eq!(response.sent, SerpSentCertainty::ResponseReceived);
            assert_eq!(response.error, Some(expected));
            assert_eq!(
                decode_dataforseo_get(&response, TASK_ID),
                Err(SerpAdapterError::IncompleteResponse)
            );
        }
    }

    #[tokio::test]
    async fn post_redirect_and_lost_receipt_never_trigger_another_send() {
        for redirect in [true, false] {
            let (listener, origin) = listen().await;
            let redirect_url = format!("{origin}/must-not-follow");
            let server = tokio::spawn(async move {
                let (mut socket, _) = listener.accept().await.unwrap();
                read_request(&mut socket).await;
                if redirect {
                    socket.write_all(format!("HTTP/1.1 307 Temporary Redirect\r\nLocation: {redirect_url}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").as_bytes()).await.unwrap();
                }
                socket.shutdown().await.unwrap();
                assert!(
                    tokio::time::timeout(Duration::from_millis(100), listener.accept())
                        .await
                        .is_err()
                );
            });
            let response = DataForSeoClient::loopback(origin, 1024)
                .unwrap()
                .post_task(&request())
                .await;
            if redirect {
                assert_eq!(response.http_status, Some(307));
                assert_eq!(
                    decode_dataforseo_post(&response, "synthetic-attempt"),
                    Err(SerpAdapterError::HttpRejected)
                );
            } else {
                assert_eq!(response.sent, SerpSentCertainty::PossiblySent);
                assert_eq!(response.error, Some(SerpTransportError::TransportFailed));
            }
            server.await.unwrap();
        }
    }

    #[tokio::test]
    async fn validation_precedes_any_send_and_test_origins_cannot_be_remote() {
        let client = DataForSeoClient::loopback("http://127.0.0.1:1".into(), 1024).unwrap();
        let mut invalid = request();
        invalid.keyword = "x".repeat(701);
        assert_eq!(
            client.post_task(&invalid).await.sent,
            SerpSentCertainty::NotSent
        );
        assert_eq!(
            client.get_task("../other?token=secret").await.sent,
            SerpSentCertainty::NotSent
        );
        for origin in [
            "https://example.org",
            "http://example.org",
            "http://user:secret@127.0.0.1",
            "http://127.0.0.1/path",
        ] {
            assert!(DataForSeoClient::loopback(origin.into(), 1024).is_err());
        }
        assert!(DataForSeoClient::new("".into(), "synthetic".into()).is_err());
    }

    #[test]
    fn decoders_verify_envelope_identity_and_separate_organic_from_absolute_ranks() {
        let posted = raw(
            SerpOperation::PostTask,
            json!({"id":TASK_ID,"status_code":20100,"data":{"tag":"synthetic-attempt"}}),
        );
        assert_eq!(
            decode_dataforseo_post(&posted, "synthetic-attempt")
                .unwrap()
                .task_id,
            TASK_ID
        );
        assert_eq!(
            decode_dataforseo_post(&posted, "wrong"),
            Err(SerpAdapterError::TaskIdentityMismatch)
        );
        let get = raw(
            SerpOperation::GetTask,
            json!({
                "id":TASK_ID,"status_code":20000,
                "data":{"keyword":"rain gauge","location_code":2840,"device":"desktop"},
                "result":[{
                    "datetime":"2026-01-01 00:00:00 +00:00","pages_count":1,"items_count":5,
                    "items":[
                        {"type":"paid","rank_group":1,"rank_absolute":1,"url":"https://example.org/ad"},
                        {"type":"organic","rank_group":1,"rank_absolute":2,"url":"https://example.org/page","title":"First","page":1},
                        {"type":"featured_snippet","rank_group":1,"rank_absolute":3},
                        {"type":"organic","rank_group":2,"rank_absolute":4,"url":"https://example.org/page","title":"Duplicate","page":1},
                        {"type":"new_provider_kind","rank_group":1,"rank_absolute":5}
                    ]
                }]
            }),
        );
        let DataForSeoTaskOutcome::Completed { result } =
            decode_dataforseo_get(&get, TASK_ID).unwrap()
        else {
            panic!("completed")
        };
        assert_eq!(result.items.len(), 5);
        assert_eq!(result.returned_organic_count, 2);
        assert_eq!(result.max_returned_organic_rank, Some(2));
        assert!(!result.coverage_verified);
        assert_eq!(result.items[0].organic_rank, None);
        assert_eq!(result.items[1].organic_rank, Some(1));
        assert_eq!(result.items[1].absolute_position, Some(2));
        assert_eq!(result.items[1].raw_url, result.items[3].raw_url);
        assert_eq!(result.items[4].kind, SerpItemKind::Other);
        assert_eq!(result.items[4].raw_kind, "new_provider_kind");
        assert_eq!(
            result.items[3].evidence_pointer,
            "/tasks/0/result/0/items/3"
        );
        assert_eq!(
            decode_dataforseo_get(&get, "00000000-wrong"),
            Err(SerpAdapterError::TaskIdentityMismatch)
        );
        assert_eq!(
            decode_dataforseo_get(&posted, TASK_ID),
            Err(SerpAdapterError::OperationMismatch)
        );
    }

    #[test]
    fn pending_failures_and_ready_absence_are_not_successful_coverage_or_repost_permission() {
        for status in [40601, 40602] {
            let response = raw(
                SerpOperation::GetTask,
                json!({"id":TASK_ID,"status_code":status}),
            );
            assert_eq!(
                decode_dataforseo_get(&response, TASK_ID).unwrap(),
                DataForSeoTaskOutcome::Pending {
                    provider_status_code: status
                }
            );
        }
        for status in [40102, 40103, 40401, 40403, 50000] {
            let response = raw(
                SerpOperation::GetTask,
                json!({"id":TASK_ID,"status_code":status,"status_message":"synthetic-private-provider-text"}),
            );
            let decoded = decode_dataforseo_get(&response, TASK_ID).unwrap();
            assert_eq!(
                decoded,
                DataForSeoTaskOutcome::Failed {
                    provider_status_code: status
                }
            );
            assert!(!format!("{decoded:?}").contains("private"));
        }
        let empty = raw(
            SerpOperation::TasksReady,
            json!({"status_code":20000,"result_count":0,"result":null}),
        );
        assert!(decode_dataforseo_ready(&empty).unwrap().is_empty());
        let ready = raw(
            SerpOperation::TasksReady,
            json!({"status_code":20000,"result":[{"id":TASK_ID,"tag":"synthetic-attempt"}]}),
        );
        assert_eq!(
            decode_dataforseo_ready(&ready).unwrap(),
            vec![DataForSeoReadyTask {
                task_id: TASK_ID.into(),
                tag: Some("synthetic-attempt".into())
            }]
        );
        let mut mismatch = ready.clone();
        mismatch.body =
            serde_json::to_vec(&json!({"status_code":20000,"tasks_count":2,"tasks":[]})).unwrap();
        assert_eq!(
            decode_dataforseo_ready(&mismatch),
            Err(SerpAdapterError::InvalidEnvelope)
        );
    }
}
