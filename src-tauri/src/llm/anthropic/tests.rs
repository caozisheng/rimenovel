use super::*;
use crate::llm::TaskKind;
use std::sync::Arc;
use std::time::Duration;
use wiremock::matchers::{body_json, header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn client(uri: String) -> AnthropicClient {
    AnthropicClient {
        cfg: ProviderConfig {
            id: 1,
            name: "test-anthropic".into(),
            protocol: "anthropic".into(),
            base_url: uri,
            model_extract: Some("claude-test".into()),
            model_write: Some("claude-test".into()),
            model_merge: Some("claude-test".into()),
            api_key: "ak-test".into(),
        },
        http: reqwest::Client::new(),
    }
}

fn schema() -> Value {
    json!({
        "type": "object", "additionalProperties": false, "required": ["ok"],
        "properties": {"ok": {"type": "boolean"}}
    })
}

fn structured_request(schema: &Value) -> StructuredRequest<'_> {
    StructuredRequest {
        task: TaskKind::Extract, messages: vec![], schema,
        schema_name: "test", temperature: 0.0, max_tokens: 321,
    }
}

fn text_request() -> TextRequest {
    TextRequest {
        task: TaskKind::Write, messages: vec![], temperature: 0.8, max_tokens: Some(456),
    }
}

fn tool_response(input: Value) -> Value {
    json!({
        "id": "msg-test", "type": "message", "role": "assistant", "model": "claude-test",
        "content": [{"type": "tool_use", "id": "tool-test", "name": "test", "input": input}],
        "stop_reason": "tool_use", "stop_sequence": null,
        "usage": {"input_tokens": 127, "output_tokens": 31}
    })
}

async fn mount_json(server: &MockServer, body: Value) {
    Mock::given(method("POST")).and(path("/v1/messages"))
        .respond_with(ResponseTemplate::new(200).set_body_json(body))
        .mount(server).await;
}

async fn mount_sse(server: &MockServer, sse: &str) {
    Mock::given(method("POST")).and(path("/v1/messages"))
        .respond_with(ResponseTemplate::new(200)
            .insert_header("content-type", "text/event-stream").set_body_string(sse))
        .mount(server).await;
}

#[tokio::test]
async fn structured_forces_named_tool_with_explicit_budget_system_and_custom_prefix() {
    let server = MockServer::start().await;
    let shape = schema();
    Mock::given(method("POST")).and(path("/proxy/v1/messages"))
        .and(header("x-api-key", "ak-test"))
        .and(header("anthropic-version", "2023-06-01"))
        .and(body_json(json!({
            "model": "claude-test", "temperature": 0.0, "max_tokens": 321,
            "system": "first\nsecond",
            "messages": [{"role": "user", "content": "extract"}, {"role": "assistant", "content": "ready"}],
            "tools": [{"name": "test", "description": "structured output", "input_schema": shape.clone()}],
            "tool_choice": {"type": "tool", "name": "test"}
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(tool_response(json!({"ok": true}))))
        .mount(&server).await;
    let adapter = client(format!("{}/proxy/", server.uri()));
    let mut req = structured_request(&shape);
    req.messages = vec![
        ChatMessage { role: "system".into(), content: "first".into() },
        ChatMessage { role: "user".into(), content: "extract".into() },
        ChatMessage { role: "system".into(), content: "second".into() },
        ChatMessage { role: "assistant".into(), content: "ready".into() },
    ];
    let response = adapter.structured_with_usage(req).await.unwrap();
    assert_eq!(response.value, json!({"ok": true}));
    assert_eq!(response.usage, Some((127, 31)));
    assert_eq!(server.received_requests().await.unwrap().len(), 1);
}

#[tokio::test]
async fn full_schema_validation_never_downgrades_or_retries() {
    let shape = json!({
        "type": "object", "additionalProperties": false, "required": ["items"],
        "properties": {"items": {"type": "array", "minItems": 1,
            "items": {"$ref": "#/$defs/item"}}},
        "$defs": {"item": {"type": "object", "additionalProperties": false,
            "required": ["name", "kind"], "properties": {
                "name": {"type": "string", "pattern": "^[A-Z]+$"},
                "kind": {"enum": ["person", "place"]}
            }}}
    });
    for invalid in [
        json!({"items": []}),
        json!({"items": [{"name": "lowercase", "kind": "person"}]}),
        json!({"items": [{"name": "NAME", "kind": "unknown"}]}),
        json!({"items": [{"name": "NAME", "kind": "person", "extra": true}]}),
    ] {
        let server = MockServer::start().await;
        mount_json(&server, tool_response(invalid)).await;
        let result = client(server.uri()).structured(structured_request(&shape)).await;
        assert!(matches!(result, Err(LlmError::Schema(_))), "{result:?}");
        assert_eq!(server.received_requests().await.unwrap().len(), 1);
    }
}

#[tokio::test]
async fn structured_rejects_truncation_refusal_and_malformed_tool_payloads() {
    let mut truncated = tool_response(json!({"ok": true}));
    truncated["stop_reason"] = json!("max_tokens");
    let mut refused = tool_response(json!({"ok": true}));
    refused["stop_reason"] = json!("refusal");
    let mut wrong_tool = tool_response(json!({"ok": true}));
    wrong_tool["content"][0]["name"] = json!("other");
    let mut missing_input = tool_response(json!({"ok": true}));
    missing_input["content"][0].as_object_mut().unwrap().remove("input");
    for body in [truncated, refused, wrong_tool, missing_input,
        tool_response(json!("{malformed")),
        json!({"stop_reason": "end_turn", "content": [{"type": "text", "text": "{\"ok\":true}"}]}),
    ] {
        let server = MockServer::start().await;
        mount_json(&server, body).await;
        let shape = schema();
        assert!(client(server.uri()).structured(structured_request(&shape)).await.is_err());
        assert_eq!(server.received_requests().await.unwrap().len(), 1);
    }
}

#[tokio::test]
async fn malformed_response_json_is_a_schema_failure_without_retry() {
    let server = MockServer::start().await;
    Mock::given(method("POST")).and(path("/v1/messages"))
        .respond_with(ResponseTemplate::new(200).set_body_string("{malformed"))
        .mount(&server).await;
    let shape = schema();
    assert!(matches!(client(server.uri()).structured(structured_request(&shape)).await,
        Err(LlmError::Schema(_))));
    assert_eq!(server.received_requests().await.unwrap().len(), 1);
}

#[tokio::test]
async fn http_failures_preserve_metadata_without_adapter_retries() {
    for status in [429, 503] {
        for streaming in [false, true] {
            let server = MockServer::start().await;
            Mock::given(method("POST")).and(path("/v1/messages"))
                .respond_with(ResponseTemplate::new(status)
                    .insert_header("retry-after", "7").insert_header("request-id", "req-anthropic")
                    .set_body_string("overloaded"))
                .mount(&server).await;
            let adapter = client(server.uri());
            let shape = schema();
            let err = if streaming {
                adapter.stream_text(text_request(), Arc::new(|_| {})).await.unwrap_err()
            } else {
                adapter.structured(structured_request(&shape)).await.unwrap_err()
            };
            match err {
                LlmError::Server { status: got, body, retry_after, request_id } => {
                    assert_eq!(got, status);
                    assert_eq!(body, "overloaded");
                    assert_eq!(retry_after, Some(Duration::from_secs(7)));
                    assert_eq!(request_id.as_deref(), Some("req-anthropic"));
                }
                other => panic!("unexpected error: {other:?}"),
            }
            assert_eq!(server.received_requests().await.unwrap().len(), 1);
        }
    }
}

fn delta(text: &str) -> String {
    format!("event: content_block_delta\ndata: {}\n\n", json!({
        "type": "content_block_delta", "index": 0, "delta": {"type": "text_delta", "text": text}
    }))
}

fn stop(reason: &str) -> String {
    format!("event: message_delta\ndata: {}\n\nevent: message_stop\ndata: {{\"type\":\"message_stop\"}}\n\n",
        json!({"type": "message_delta", "delta": {"stop_reason": reason}, "usage": {"output_tokens": 9}}))
}

#[tokio::test]
async fn prose_completion_and_callback_preserve_unicode_with_multiline_crlf_frames() {
    let server = MockServer::start().await;
    let sse = format!(": keepalive\r\n\r\nevent: content_block_delta\r\ndata: {{\"type\":\"content_block_delta\",\r\ndata: \"delta\":{{\"type\":\"text_delta\",\"text\":\"山雨🌧\"}}}}\r\n\r\n{}{}", delta("欲来"), stop("end_turn"));
    mount_sse(&server, &sse).await;
    let got = Arc::new(parking_lot::Mutex::new(String::new()));
    let sink = got.clone();
    let full = client(server.uri()).stream_text(text_request(), Arc::new(move |d| sink.lock().push_str(d))).await.unwrap();
    assert_eq!(full, "山雨🌧欲来");
    assert_eq!(*got.lock(), full);
    let requests = server.received_requests().await.unwrap();
    let body: Value = serde_json::from_slice(&requests[0].body).unwrap();
    assert_eq!(body["max_tokens"], 456);
    assert_eq!(body["stream"], true);
}

#[tokio::test]
async fn prose_stop_sequence_is_complete() {
    let server = MockServer::start().await;
    mount_sse(&server, &(delta("complete") + &stop("stop_sequence"))).await;
    assert_eq!(client(server.uri()).stream_text(text_request(), Arc::new(|_| {})).await.unwrap(), "complete");
}

#[tokio::test]
async fn prose_never_accepts_errors_truncation_or_premature_eof() {
    for ending in [
        "".to_string(),
        stop("max_tokens"), stop("refusal"), stop("tool_use"),
        "event: error\ndata: {\"type\":\"error\",\"error\":{\"type\":\"overloaded_error\",\"message\":\"busy\"}}\n\n".to_string() + &stop("end_turn"),
        "event: error\ndata: {\"error\":{\"message\":\"busy\"}}\n\n".to_string() + &stop("end_turn"),
        "data: {malformed}\n\n".to_string() + &stop("end_turn"),
        "data: {\"type\":\"message_stop\"}".to_string(),
    ] {
        let server = MockServer::start().await;
        mount_sse(&server, &(delta("partial") + &ending)).await;
        let got = Arc::new(parking_lot::Mutex::new(String::new()));
        let sink = got.clone();
        let result = client(server.uri()).stream_text(text_request(), Arc::new(move |d| sink.lock().push_str(d))).await;
        assert!(matches!(result, Err(LlmError::Stream(_))), "{result:?}");
        assert_eq!(*got.lock(), "partial");
        assert_eq!(server.received_requests().await.unwrap().len(), 1);
    }
}

// A real HTTP chunked response fragments both SSE fields and multibyte UTF-8.
fn fragmented_server(payload: Vec<u8>) -> (String, std::thread::JoinHandle<()>) {
    use std::io::{Read, Write};
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let uri = format!("http://{}", listener.local_addr().unwrap());
    let handle = std::thread::spawn(move || {
        let (mut socket, _) = listener.accept().unwrap();
        socket.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
        socket.set_write_timeout(Some(Duration::from_secs(10))).unwrap();
        socket.set_nodelay(true).unwrap();
        let mut request = Vec::new();
        let header_end = loop {
            let mut byte = [0];
            socket.read_exact(&mut byte).unwrap();
            request.push(byte[0]);
            if request.ends_with(b"\r\n\r\n") { break request.len(); }
        };
        let headers = std::str::from_utf8(&request[..header_end]).unwrap();
        let length = headers.lines().find_map(|line| {
            let (name, value) = line.split_once(':')?;
            name.eq_ignore_ascii_case("content-length").then(|| value.trim().parse::<usize>().unwrap())
        }).unwrap();
        let mut body = vec![0; length];
        socket.read_exact(&mut body).unwrap();
        socket.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n").unwrap();
        for byte in payload {
            // One-byte HTTP chunks force codepoint and event-boundary fragmentation.
            if socket.write_all(&[b'1', b'\r', b'\n', byte, b'\r', b'\n']).is_err() { return; }
            socket.flush().unwrap();
            std::thread::sleep(Duration::from_millis(1));
        }
        let _ = socket.write_all(b"0\r\n\r\n");
    });
    (uri, handle)
}

#[tokio::test]
async fn real_fragmented_http_stream_preserves_split_utf8_and_complete_frames() {
    let payload = format!("{}{}{}", delta("山🌧"), delta("雨欲来"), stop("end_turn"));
    let (uri, server) = fragmented_server(payload.into_bytes());
    let got = Arc::new(parking_lot::Mutex::new(String::new()));
    let sink = got.clone();
    let result = client(uri).stream_text(text_request(), Arc::new(move |d| sink.lock().push_str(d))).await;
    server.join().unwrap();
    assert_eq!(result.unwrap(), "山🌧雨欲来");
    assert_eq!(*got.lock(), "山🌧雨欲来");
}

#[tokio::test]
async fn invalid_utf8_stream_fails_instead_of_emitting_replacement_characters() {
    let mut payload = b"data: {\"type\":\"content_block_delta\",\"delta\":{\"type\":\"text_delta\",\"text\":\"".to_vec();
    payload.push(0xff);
    payload.extend_from_slice(b"\"}}\n\n");
    payload.extend_from_slice(stop("end_turn").as_bytes());
    let (uri, server) = fragmented_server(payload);
    let got = Arc::new(parking_lot::Mutex::new(String::new()));
    let sink = got.clone();
    let result = client(uri).stream_text(text_request(), Arc::new(move |d| sink.lock().push_str(d))).await;
    server.join().unwrap();
    assert!(matches!(result, Err(LlmError::Stream(_))));
    assert!(got.lock().is_empty());
}

#[tokio::test]
async fn provider_errors_never_expose_api_keys() {
    let server = MockServer::start().await;
    Mock::given(method("POST")).and(path("/v1/messages"))
        .respond_with(ResponseTemplate::new(503).insert_header("request-id", "ak-test-request")
            .set_body_string("credential ak-test is rejected"))
        .mount(&server).await;
    let shape = schema();
    let error = client(server.uri()).structured(structured_request(&shape)).await.unwrap_err();
    assert!(!format!("{error:?}").contains("ak-test"));

    let server = MockServer::start().await;
    mount_sse(&server, "event: error\ndata: {\"type\":\"error\",\"error\":{\"message\":\"bad key ak-test\"}}\n\n").await;
    let error = client(server.uri()).stream_text(text_request(), Arc::new(|_| {})).await.unwrap_err();
    assert!(!format!("{error:?}").contains("ak-test"));
}

#[tokio::test]
async fn bare_cr_event_boundaries_are_supported() {
    let server = MockServer::start().await;
    let payload = (delta("complete") + &stop("end_turn")).replace('\n', "\r");
    mount_sse(&server, &payload).await;
    assert_eq!(client(server.uri()).stream_text(text_request(), Arc::new(|_| {})).await.unwrap(), "complete");
}

#[tokio::test]
async fn invalid_schema_is_an_error_before_any_http_request() {
    let server = MockServer::start().await;
    let shape = json!({"type": 42});
    let result = client(server.uri()).structured(structured_request(&shape)).await;
    assert!(matches!(result, Err(LlmError::Schema(_))));
    assert!(server.received_requests().await.unwrap().is_empty());
}

#[tokio::test]
async fn missing_or_zero_output_budgets_fail_before_any_http_request() {
    let server = MockServer::start().await;
    let adapter = client(server.uri());
    let shape = schema();
    let mut req = structured_request(&shape);
    req.max_tokens = 0;
    assert!(matches!(adapter.structured(req).await, Err(LlmError::Config(_))));
    for budget in [None, Some(0)] {
        let mut req = text_request();
        req.max_tokens = budget;
        assert!(matches!(adapter.stream_text(req, Arc::new(|_| {})).await, Err(LlmError::Config(_))));
    }
    assert!(server.received_requests().await.unwrap().is_empty());
}

#[tokio::test]
async fn malformed_text_deltas_and_terminal_length_are_failures() {
    for event in [
        "data: {\"type\":\"content_block_delta\",\"delta\":{\"type\":\"text_delta\",\"text\":42}}\n\n".to_string(),
        "data: {\"type\":\"message_stop\",\"stop_reason\":\"length\"}\n\n".to_string(),
        stop("length"),
    ] {
        let server = MockServer::start().await;
        mount_sse(&server, &(delta("partial") + &event + &stop("end_turn"))).await;
        assert!(matches!(client(server.uri()).stream_text(text_request(), Arc::new(|_| {})).await,
            Err(LlmError::Stream(_))));
    }
}

#[tokio::test]
async fn schema_error_messages_do_not_expose_echoed_api_key() {
    let server = MockServer::start().await;
    let shape = json!({"type": "object", "properties": {"value": {"enum": ["expected"]}}});
    mount_json(&server, tool_response(json!({"value": "ak-test"}))).await;
    let error = client(server.uri()).structured(structured_request(&shape)).await.unwrap_err();
    assert!(matches!(error, LlmError::Schema(_)));
    assert!(!format!("{error:?}").contains("ak-test"));
}
