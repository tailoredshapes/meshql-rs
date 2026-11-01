use super::*;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

/// Spin up a one-shot HTTP/1.1 responder on `127.0.0.1:0` that returns
/// `response_body` (already-serialized JSON) for the first request,
/// captures the request body, and shuts down. Returns the base URL and
/// a JoinHandle yielding the request body bytes.
async fn one_shot_server(response_body: String) -> (String, tokio::task::JoinHandle<String>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let url = format!("http://{addr}");
    let handle = tokio::spawn(async move {
        let (mut sock, _) = listener.accept().await.unwrap();
        let mut buf = vec![0u8; 8192];
        let mut total = Vec::new();
        // Read until we have the full request body. We rely on a
        // Content-Length header from reqwest's json() call.
        let mut content_length: Option<usize> = None;
        let mut header_end: Option<usize> = None;
        loop {
            let n = sock.read(&mut buf).await.unwrap();
            if n == 0 {
                break;
            }
            total.extend_from_slice(&buf[..n]);
            if header_end.is_none() {
                if let Some(idx) = total.windows(4).position(|w| w == b"\r\n\r\n") {
                    header_end = Some(idx + 4);
                    let header_str = std::str::from_utf8(&total[..idx]).unwrap_or("");
                    for line in header_str.split("\r\n") {
                        if let Some(rest) = line
                            .strip_prefix("Content-Length:")
                            .or_else(|| line.strip_prefix("content-length:"))
                        {
                            content_length = rest.trim().parse().ok();
                        }
                    }
                }
            }
            if let (Some(end), Some(cl)) = (header_end, content_length) {
                if total.len() >= end + cl {
                    break;
                }
            }
        }
        let body_start = header_end.unwrap_or(total.len());
        let body = String::from_utf8_lossy(&total[body_start..]).to_string();
        let resp = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                response_body.len(),
                response_body
            );
        sock.write_all(resp.as_bytes()).await.unwrap();
        sock.flush().await.unwrap();
        body
    });
    (url, handle)
}

#[tokio::test]
async fn gql_posts_query_and_returns_data() {
    let response = r#"{"data":{"getAll":[{"id":"d1","name":"first"}]}}"#;
    let (url, handle) = one_shot_server(response.to_string()).await;
    let client = MeshqlClient::new(url);
    let data = client
        .gql("/deployable/graph", "{ getAll { id name } }")
        .await
        .expect("gql ok");
    let body = handle.await.unwrap();
    // Body should be `{"query":"{ getAll { id name } }"}` — verify it
    // round-trips through serde_json so we don't lock down whitespace.
    let parsed: Value = serde_json::from_str(&body).expect("request was JSON");
    assert_eq!(parsed["query"], "{ getAll { id name } }");
    assert_eq!(data["getAll"][0]["id"], "d1");
    assert_eq!(data["getAll"][0]["name"], "first");
}

#[tokio::test]
async fn gql_returns_err_when_errors_array_present() {
    let response = r#"{"data":null,"errors":[{"message":"boom"},{"message":"again"}]}"#;
    let (url, _handle) = one_shot_server(response.to_string()).await;
    let client = MeshqlClient::new(url);
    let err = client
        .gql("/deployable/graph", "{ getAll { id } }")
        .await
        .expect_err("should fail");
    let msg = format!("{err}");
    assert!(msg.contains("boom"), "got: {msg}");
    assert!(msg.contains("again"), "got: {msg}");
}
