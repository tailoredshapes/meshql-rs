use super::*;
use crate::capability::CapabilityHandler;
use std::sync::atomic::{AtomicUsize, Ordering};
fn server(count: Arc<AtomicUsize>) -> MeshqlMcpServer {
    MeshqlMcpServer::new(McpServerConfig {
        server_name: "test".into(),
        server_version: "1".into(),
        client: Arc::new(MeshqlClient::new("http://localhost:1")),
        capabilities: vec![Capability {
            name: "write",
            description: "counter",
            input_schema: json!({"type":"object","properties":{"value":{"type":"string"}},"required":["value"],"additionalProperties":false}),
            handler: CapabilityHandler::Custom(Arc::new(move |_, args| {
                count.fetch_add(1, Ordering::SeqCst);
                Box::pin(async move { Ok(args) })
            })),
        }],
    })
}
async fn ready(server: &MeshqlMcpServer) {
    let response=server.handle_request(json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":PROTOCOL_VERSION,"capabilities":{},"clientInfo":{"name":"test","version":"1"}}})).await.unwrap();
    assert_eq!(response["result"]["protocolVersion"], PROTOCOL_VERSION);
    assert!(server
        .handle_request(json!({"jsonrpc":"2.0","method":"notifications/initialized"}))
        .await
        .is_none());
}
#[tokio::test]
async fn lifecycle_validation_and_idless_calls_have_no_effect() {
    let count = Arc::new(AtomicUsize::new(0));
    let server = server(count.clone());
    let request = json!({"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"write","arguments":{"value":"x"}}});
    assert_eq!(
        server.handle_request(request.clone()).await.unwrap()["error"]["code"],
        -32600
    );
    ready(&server).await;
    let mut notification = request.clone();
    notification.as_object_mut().unwrap().remove("id");
    assert!(server.handle_request(notification).await.is_none());
    assert_eq!(count.load(Ordering::SeqCst), 0);
    assert_eq!(
        server
            .handle_request(json!([request.clone()]))
            .await
            .unwrap()["error"]["code"],
        -32600
    );
    let mut invalid = request.clone();
    invalid["params"]["arguments"]["value"] = json!({"attack":true});
    assert_eq!(
        server.handle_request(invalid).await.unwrap()["error"]["code"],
        -32602
    );
    assert_eq!(count.load(Ordering::SeqCst), 0);
    let result = server.handle_request(request).await.unwrap();
    assert_eq!(result["result"]["structuredContent"], json!({"value":"x"}));
    assert_eq!(count.load(Ordering::SeqCst), 1);
}
#[tokio::test]
async fn malformed_envelopes_and_unknown_tools_are_rejected() {
    let server = server(Arc::new(AtomicUsize::new(0)));
    ready(&server).await;
    for request in [
        json!(null),
        json!({"jsonrpc":"1.0","id":1,"method":"ping"}),
        json!({"jsonrpc":"2.0","id":{},"method":"ping"}),
    ] {
        assert_eq!(
            server.handle_request(request).await.unwrap()["error"]["code"],
            -32600
        );
    }
    assert_eq!(
        server.handle_line("{").await.unwrap()["error"]["code"],
        -32700
    );
    assert_eq!(
        server
            .handle_request(
                json!({"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"missing"}})
            )
            .await
            .unwrap()["error"]["code"],
        -32602
    );
}
#[tokio::test]
async fn frames_are_bounded_and_recover_after_oversize() {
    let input = b"12345\n{}\n";
    let mut reader = BufReader::new(&input[..]);
    assert!(matches!(
        frame::read(&mut reader, 4).await.unwrap(),
        Frame::Oversized
    ));
    assert!(matches!(frame::read(&mut reader,4).await.unwrap(),Frame::Bytes(v) if v==b"{}"));
    assert!(matches!(
        frame::read(&mut reader, 4).await.unwrap(),
        Frame::Eof
    ));
}
#[test]
fn structured_and_text_content_are_the_same_object() {
    for value in [json!([1, 2]), json!(null), json!(3), json!({"a":1})] {
        let result = wrap_text_result(&value);
        let text: Value =
            serde_json::from_str(result["content"][0]["text"].as_str().unwrap()).unwrap();
        assert_eq!(text, result["structuredContent"]);
        assert!(text.is_object());
    }
}

#[tokio::test]
async fn custom_errors_are_wrapped_and_blank_frames_are_silent() {
    let capability = Capability {
        name: "boom",
        description: "custom failure",
        input_schema: json!({"type":"object"}),
        handler: CapabilityHandler::Custom(Arc::new(|_, _| {
            Box::pin(async { Err(anyhow::anyhow!("kaboom")) })
        })),
    };
    let server = MeshqlMcpServer::new(McpServerConfig {
        server_name: "test".into(),
        server_version: "1".into(),
        client: Arc::new(MeshqlClient::new("http://localhost:1")),
        capabilities: vec![capability],
    });
    ready(&server).await;
    for line in ["", "   ", "\t"] {
        assert!(server.handle_line(line).await.is_none());
    }
    let response = server
        .handle_request(
            json!({"jsonrpc":"2.0","id":5,"method":"tools/call","params":{"name":"boom"}}),
        )
        .await
        .unwrap();
    assert_eq!(response["result"]["isError"], true);
    assert!(response["result"]["content"][0]["text"]
        .as_str()
        .unwrap()
        .contains("kaboom"));
    assert_eq!(
        server
            .handle_request(json!({"jsonrpc":"2.0","id":"alive","method":"ping"}))
            .await
            .unwrap()["result"],
        json!({})
    );
}
