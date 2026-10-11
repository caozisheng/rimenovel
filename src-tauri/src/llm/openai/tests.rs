use super::*;
use crate::llm::{ChatMessage, TaskKind};
use std::sync::Arc;
use wiremock::matchers::{body_partial_json, header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn cfg(base_url: String) -> ProviderConfig {
    ProviderConfig {
        id: 1,
        name: "test-openai".into(),
        protocol: "openai".into(),
        base_url,
        model_extract: Some("gpt-extract".into()),
        model_write: Some("gpt-write".into()),
        model_merge: Some("gpt-merge".into()),
        api_key: "sk-test".into(),
    }
}

fn client(base_url: String) -> OpenAiClient {
    OpenAiClient {
        cfg: cfg(base_url),
        http: reqwest::Client::builder().retry(reqwest::retry::never()).build().unwrap(),
    }
}

fn schema() -> Value {
    json!({
        "type": "object", "additionalProperties": false, "required": ["ok"],
        "properties": {"ok": {"type": "boolean"}}
    })
}

fn request(shape: &Value) -> StructuredRequest<'_> {
    StructuredRequest {
        task: TaskKind::Extract,
        messages: vec![ChatMessage { role: "user".into(), content: "抽取".into() }],
        schema: shape,
        schema_name: "test",
        temperature: 0.0,
        max_tokens: 4096,
    }
}

fn text_request() -> TextRequest {
    TextRequest {
        task: TaskKind::Write,
        messages: vec![ChatMessage { role: "user".into(), content: "写".into() }],
        temperature: 0.8,
        max_tokens: Some(8192),
    }
}

fn completion(content: &str) -> Value {
    json!({"choices": [{"delta": {"content": content}, "finish_reason": "stop"}]})
}

async fn mount_completion(server: &MockServer, body: Value) {
    let usage = body.get("usage").cloned().unwrap_or(Value::Null);
    mount_stream(server, &format!(
        "data: {body}\n\ndata: {{\"choices\":[],\"usage\":{usage}}}\n\ndata: [DONE]\n\n"
    )).await;
}

async fn mount_stream(server: &MockServer, sse: &str) {
    Mock::given(method("POST")).and(path("/chat/completions"))
        .respond_with(ResponseTemplate::new(200)
            .insert_header("content-type", "text/event-stream")
            .set_body_string(sse))
        .expect(1).mount(server).await;
}


// This service boundary reproduces the real gateway's strict-schema rejection,
// rather than accepting every request as a successful buffered fixture.
fn supported_typed_schema(value: &Value) -> bool {
    match value {
        Value::Object(object) => {
            if object.contains_key("const") { return false; }
            if object.get("enum").and_then(Value::as_array).is_some_and(|values| values.iter().all(Value::is_string))
                && object.get("type").and_then(Value::as_str) != Some("string") { return false; }
            object.values().all(supported_typed_schema)
        },
        Value::Array(values) => values.iter().all(supported_typed_schema),
        _ => true,
    }
}

#[tokio::test]
async fn compact_graph_works_with_provider_requiring_typed_scalar_constraints() {
    let shape = crate::core::kg_extract::graph_schema().clone();
    let value = json!({"summary":"本章交代旅途背景","entities":[],"relations":[],"events":[],"open_threads":[]});
    let server = MockServer::start().await;
    let reply = format!("data: {}\n\ndata: [DONE]\n\n", completion(&value.to_string()));
    Mock::given(method("POST")).and(path("/chat/completions"))
        .respond_with(move |request: &wiremock::Request| {
            let body: Value = serde_json::from_slice(&request.body).unwrap();
            let valid = supported_typed_schema(body.pointer("/response_format/json_schema/schema").unwrap());
            ResponseTemplate::new(200).insert_header("content-type","text/event-stream")
                .set_body_string(if valid { reply.clone() } else {
                    "data: {\"error\":{\"code\":\"bad_response\",\"message\":\"responses stream error: response.failed\",\"type\":\"bad_response\"}}\n\n".into()
                })
        }).expect(1).mount(&server).await;
    assert_eq!(client(server.uri()).structured(request(&shape)).await.unwrap(), value);
}

#[tokio::test]
async fn structured_preserves_reported_usage() {
    let server = MockServer::start().await;
    let mut body = completion("{\"ok\":true}");
    body["usage"] = json!({"prompt_tokens": 143, "completion_tokens": 29});
    mount_completion(&server, body).await;
    let response = client(server.uri()).structured_with_usage(request(&schema())).await.unwrap();
    assert_eq!(response.usage, Some((143, 29)));
    assert_eq!(response.value["ok"], true);
}

#[tokio::test]
async fn missing_or_partial_usage_is_not_fabricated() {
    for usage in [Value::Null, json!({"prompt_tokens": 143}), json!({"prompt_tokens": -1, "completion_tokens": 29})] {
        let server = MockServer::start().await;
        let mut body = completion("{\"ok\":true}");
        body["usage"] = usage;
        mount_completion(&server, body).await;
        assert_eq!(client(server.uri()).structured_with_usage(request(&schema())).await.unwrap().usage, None);
    }
}

#[tokio::test]
async fn schema_or_json_failure_never_falls_back_or_resends() {
    for content in ["{\"wrong\":1}", "{\"ok\":true,\"extra\":1}", "{\"ok\":\"yes\"}", "not json"] {
        let server = MockServer::start().await;
        mount_completion(&server, completion(content)).await;
        let error = client(server.uri()).structured(request(&schema())).await.unwrap_err();
        assert!(matches!(error, LlmError::Schema(_)), "{error:?}");
        assert_eq!(server.received_requests().await.unwrap().len(), 1);
    }
}

#[tokio::test]
async fn invalid_request_schema_is_an_error_before_transport() {
    let server = MockServer::start().await;
    let error = client(server.uri()).structured(request(&json!({"type": "not-a-type"}))).await.unwrap_err();
    assert!(matches!(error, LlmError::Schema(_)), "{error:?}");
    assert!(server.received_requests().await.unwrap().is_empty());
}

#[tokio::test]
async fn invalid_bearer_header_returns_config_without_panicking_or_sending() {
    for key in ["secret-token\n", "secret-token\r", "secret-token\0"] {
        let server = MockServer::start().await;
        let mut client = client(server.uri());
        client.cfg.api_key = key.into();
        let structured_error = client.structured(request(&schema())).await.unwrap_err();
        assert!(matches!(&structured_error, LlmError::Config(_)));
        assert!(!structured_error.to_string().contains("secret-token"));
        let stream_error = client.stream_text(text_request(), Arc::new(|_| {})).await.unwrap_err();
        assert!(matches!(&stream_error, LlmError::Config(_)));
        assert!(!stream_error.to_string().contains("secret-token"));
        assert!(server.received_requests().await.unwrap().is_empty());
    }
}

#[tokio::test]
async fn structured_rejects_truncation_refusal_and_nonstop_reasons() {
    for reason in [json!("length"), json!("content_filter"), json!("tool_calls"), Value::Null] {
        let server = MockServer::start().await;
        let mut body = completion("{\"ok\":true}");
        body["choices"][0]["finish_reason"] = reason;
        mount_completion(&server, body).await;
        let error = client(server.uri()).structured(request(&schema())).await.unwrap_err();
        assert!(matches!(error, LlmError::Stream(_)), "{error:?}");
    }
    let server = MockServer::start().await;
    let mut body = completion("{\"ok\":true}");
    body["choices"][0]["delta"]["refusal"] = json!("Cannot comply");
    mount_completion(&server, body).await;
    assert!(matches!(client(server.uri()).structured(request(&schema())).await, Err(LlmError::Stream(_))));
}

#[tokio::test]
async fn structured_rejects_missing_content() {
    let server = MockServer::start().await;
    mount_completion(&server, json!({"choices": [{"delta": {}, "finish_reason": "stop"}]})).await;
    assert!(matches!(client(server.uri()).structured(request(&schema())).await, Err(LlmError::Stream(_))));
}

#[tokio::test]
async fn server_errors_keep_html_headers_and_send_exactly_once() {
    for status in [408, 429, 500, 502, 503, 504] {
        let server = MockServer::start().await;
        let raw = "<html><body>upstream unavailable</body></html>";
        Mock::given(method("POST")).and(path("/chat/completions"))
            .respond_with(ResponseTemplate::new(status)
                .insert_header("content-type", "text/html")
                .insert_header("retry-after", "2")
                .insert_header("x-request-id", "req-test")
                .set_body_string(raw))
            .expect(1).mount(&server).await;
        match client(server.uri()).structured(request(&schema())).await.unwrap_err() {
            LlmError::Server { status: actual, body, retry_after, request_id } => {
                assert_eq!(actual, status);
                assert_eq!(body, raw);
                assert_eq!(retry_after, Some(std::time::Duration::from_secs(2)));
                assert_eq!(request_id.as_deref(), Some("req-test"));
            }
            error => panic!("lost server metadata: {error:?}"),
        }
        assert_eq!(server.received_requests().await.unwrap().len(), 1);
    }
}

#[tokio::test]
async fn transport_timeout_is_one_attempt_and_drops_url_details() {
    let server = MockServer::start().await;
    Mock::given(method("POST")).and(path("/chat/completions"))
        .respond_with(ResponseTemplate::new(200).insert_header("content-type", "text/event-stream")
            .set_body_string(format!("data: {}\n\ndata: [DONE]\n\n", completion("{\"ok\":true}")))
            .set_delay(std::time::Duration::from_millis(200)))
        .expect(1).mount(&server).await;
    let client = OpenAiClient {
        cfg: cfg(server.uri()),
        http: reqwest::Client::builder().retry(reqwest::retry::never())
            .timeout(std::time::Duration::from_millis(50)).build().unwrap(),
    };
    let error = client.structured(request(&schema())).await.unwrap_err();
    assert!(matches!(&error, LlmError::Network(_)), "{error:?}");
    assert!(!error.to_string().contains(&server.uri()));
    assert_eq!(server.received_requests().await.unwrap().len(), 1);
}

#[tokio::test]
async fn malformed_success_event_is_stream_failure_without_resend() {
    let server = MockServer::start().await;
    mount_stream(&server, "data: not-json\n\ndata: [DONE]\n\n").await;
    assert!(matches!(client(server.uri()).structured(request(&schema())).await, Err(LlmError::Stream(_))));
    assert_eq!(server.received_requests().await.unwrap().len(), 1);
}

#[tokio::test]
async fn provider_echoes_never_expose_the_configured_token() {
    let server = MockServer::start().await;
    Mock::given(method("POST")).and(path("/chat/completions"))
        .respond_with(ResponseTemplate::new(503).insert_header("x-request-id", "sk-test")
            .set_body_string("<html>token sk-test unavailable</html>"))
        .expect(1).mount(&server).await;
    let error = client(server.uri()).structured(request(&schema())).await.unwrap_err();
    assert!(!error.to_string().contains("sk-test"));
    match error {
        LlmError::Server { body, request_id, .. } => {
            assert_eq!(body, "<html>token [REDACTED] unavailable</html>");
            assert_eq!(request_id.as_deref(), Some("[REDACTED]"));
        }
        error => panic!("lost server metadata: {error:?}"),
    }
    let server = MockServer::start().await;
    mount_stream(&server, "data: {\"error\":{\"message\":\"token sk-test unavailable\"}}\n\n").await;
    let error = client(server.uri()).stream_text(text_request(), Arc::new(|_| {})).await.unwrap_err();
    assert!(!error.to_string().contains("sk-test"));
}

#[tokio::test]
async fn stream_setup_errors_keep_server_metadata_and_do_not_retry() {
    let server = MockServer::start().await;
    Mock::given(method("POST")).and(path("/chat/completions"))
        .respond_with(ResponseTemplate::new(503).insert_header("x-request-id", "stream-request")
            .set_body_string("<html>unavailable</html>"))
        .expect(1).mount(&server).await;
    let error = client(server.uri()).stream_text(text_request(), Arc::new(|_| {})).await.unwrap_err();
    assert!(matches!(&error, LlmError::Server { status: 503, request_id: Some(id), .. } if id == "stream-request"));
    assert_eq!(server.received_requests().await.unwrap().len(), 1);
}

#[tokio::test]
async fn stream_sends_configured_model_and_limit_and_exact_deltas() {
    let server = MockServer::start().await;
    Mock::given(method("POST")).and(path("/chat/completions"))
        .and(header("authorization", "Bearer sk-test"))
        .and(body_partial_json(json!({"model": "gpt-write", "max_tokens": 8192, "stream": true})))
        .respond_with(ResponseTemplate::new(200).insert_header("content-type", "text/event-stream")
            .set_body_string("data: {\"choices\":[{\"delta\":{\"content\":\"你好\"}}]}\n\n\
                data: {\"choices\":[{\"delta\":{\"content\":\"，世界\"}}]}\n\n\
                data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n\
                data: {\"choices\":[],\"usage\":{\"prompt_tokens\":1,\"completion_tokens\":2}}\n\n\
                data: [DONE]\n\n"))
        .expect(1).mount(&server).await;
    let deltas = Arc::new(parking_lot::Mutex::new(Vec::<String>::new()));
    let sink = deltas.clone();
    let full = client(server.uri()).stream_text(text_request(), Arc::new(move |delta| sink.lock().push(delta.to_owned()))).await.unwrap();
    assert_eq!(full, "你好，世界");
    assert_eq!(*deltas.lock(), vec!["你好", "，世界"]);
}

#[tokio::test]
async fn eof_and_done_without_finish_are_incomplete_even_after_deltas() {
    for end in ["", "data: [DONE]\n\n"] {
        let server = MockServer::start().await;
        mount_stream(&server, &format!("data: {{\"choices\":[{{\"delta\":{{\"content\":\"half\"}}}}]}}\n\n{end}")).await;
        let deltas = Arc::new(parking_lot::Mutex::new(String::new()));
        let sink = deltas.clone();
        let error = client(server.uri()).stream_text(text_request(), Arc::new(move |delta| sink.lock().push_str(delta))).await.unwrap_err();
        assert!(matches!(error, LlmError::Stream(_)));
        assert_eq!(*deltas.lock(), "half");
    }
}

#[tokio::test]
async fn finish_stop_is_completion_evidence_even_without_done_sentinel() {
    let server = MockServer::start().await;
    mount_stream(&server, "data: {\"choices\":[{\"delta\":{\"content\":\"complete\"},\"finish_reason\":\"stop\"}]}\n\n").await;
    assert_eq!(client(server.uri()).stream_text(text_request(), Arc::new(|_| {})).await.unwrap(), "complete");
}

#[tokio::test]
async fn stream_rejects_nonstop_refusal_and_malformed_events() {
    for event in [
        json!({"choices": [{"delta": {}, "finish_reason": "length"}]}),
        json!({"choices": [{"delta": {}, "finish_reason": "content_filter"}]}),
        json!({"choices": [{"delta": {}, "finish_reason": "tool_calls"}]}),
        json!({"choices": [{"delta": {"refusal": "no"}, "finish_reason": "stop"}]}),
        json!({"error": {"message": "upstream failed"}}),
    ] {
        let server = MockServer::start().await;
        mount_stream(&server, &format!("data: {event}\n\ndata: [DONE]\n\n")).await;
        assert!(matches!(client(server.uri()).stream_text(text_request(), Arc::new(|_| {})).await, Err(LlmError::Stream(_))));
    }
    let server = MockServer::start().await;
    mount_stream(&server, "data: not-json\n\ndata: [DONE]\n\n").await;
    assert!(matches!(client(server.uri()).stream_text(text_request(), Arc::new(|_| {})).await, Err(LlmError::Stream(_))));
}

#[tokio::test]
async fn malformed_sse_diagnostics_do_not_expose_raw_prompt_payloads() {
    let server = MockServer::start().await;
    let private_prompt = "PRIVATE_PROMPT_do-not-persist_小说正文";
    mount_stream(&server, &format!(
        "data: {{\"choices\":[{{\"delta\":{{\"content\":\"already-delivered\"}}}}]}}\n\ndata: {private_prompt}\n\ndata: [DONE]\n\n"
    )).await;
    let deltas = Arc::new(parking_lot::Mutex::new(String::new()));
    let sink = deltas.clone();
    let error = client(server.uri()).stream_text(text_request(), Arc::new(move |delta| sink.lock().push_str(delta))).await.unwrap_err();
    assert!(matches!(&error, LlmError::Stream(_)));
    assert!(!error.to_string().contains(private_prompt));
    assert!(!format!("{error:?}").contains(private_prompt));
    assert_eq!(*deltas.lock(), "already-delivered");
    assert_eq!(server.received_requests().await.unwrap().len(), 1);
}

// Real chunked HTTP exercises UTF-8 and event boundaries independently of Wiremock's body buffering.
fn fragmented_server(body: String) -> (String, std::thread::JoinHandle<()>) {
    use std::io::{Read, Write};
    use std::net::TcpListener;
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let base_url = format!("http://{}", listener.local_addr().unwrap());
    let worker = std::thread::spawn(move || {
        let (mut socket, _) = listener.accept().unwrap();
        socket.set_read_timeout(Some(std::time::Duration::from_secs(5))).unwrap();
        socket.set_write_timeout(Some(std::time::Duration::from_secs(5))).unwrap();
        let mut request = Vec::new();
        let mut buf = [0u8; 1024];
        loop {
            let count = socket.read(&mut buf).unwrap();
            assert!(count > 0, "request ended before headers/body");
            request.extend_from_slice(&buf[..count]);
            if let Some(header_end) = request.windows(4).position(|bytes| bytes == b"\r\n\r\n") {
                let headers = String::from_utf8_lossy(&request[..header_end]);
                let content_length: usize = headers.lines().find_map(|line| {
                    let (name, value) = line.split_once(':')?;
                    name.eq_ignore_ascii_case("content-length").then(|| value.trim().parse().unwrap())
                }).unwrap_or(0);
                if request.len() >= header_end + 4 + content_length { break; }
            }
        }
        socket.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n").unwrap();
        for byte in body.bytes() {
            socket.write_all(&[b'1', b'\r', b'\n', byte, b'\r', b'\n']).unwrap();
            socket.flush().unwrap();
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        socket.write_all(b"0\r\n\r\n").unwrap();
    });
    (base_url, worker)
}

#[tokio::test]
async fn structured_stream_assembles_fragmented_unicode_and_usage_before_validation() {
    let shape = json!({"type":"object","additionalProperties":false,"required":["name"],
        "properties":{"name":{"type":"string"}}});
    let first = json!({"choices":[{"delta":{"content":"{\"name\":\"阿"}}]});
    let last = json!({"choices":[{"delta":{"content":"岚🙂\"}"},"finish_reason":"stop"}]});
    let (url, worker) = fragmented_server(format!(
        "\u{feff}data: {first}\n\ndata: {last}\n\ndata: {{\"choices\":[],\"usage\":{{\"prompt_tokens\":31,\"completion_tokens\":9}}}}\n\ndata: [DONE]\n\n"));
    let result = client(url).structured_with_usage(request(&shape)).await;
    worker.join().unwrap();
    let result = result.unwrap();
    assert_eq!(result.value, json!({"name":"阿岚🙂"}));
    assert_eq!(result.usage, Some((31,9)));
}

#[tokio::test]
async fn structured_json_is_not_published_without_normal_finish() {
    for end in ["", "data: [DONE]\n\n"] {
        let server = MockServer::start().await;
        let event = json!({"choices":[{"delta":{"content":"{\"ok\":true}"}}]});
        mount_stream(&server, &format!("data: {event}\n\n{end}")).await;
        assert!(matches!(client(server.uri()).structured(request(&schema())).await,
            Err(LlmError::Stream(_))));
    }
}

#[tokio::test]
async fn idle_partial_structured_stream_is_discarded_and_retried_without_leaking_socket() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let worker = tokio::spawn(async move {
        for attempt in 0..2 {
            let (mut socket, _) = tokio::time::timeout(std::time::Duration::from_secs(5), listener.accept()).await.unwrap().unwrap();
            let mut request = Vec::new();
            let mut buffer = [0u8; 1024];
            loop {
                let count = socket.read(&mut buffer).await.unwrap();
                assert!(count > 0);
                request.extend_from_slice(&buffer[..count]);
                if let Some(end) = request.windows(4).position(|bytes| bytes == b"\r\n\r\n") {
                    let headers = String::from_utf8_lossy(&request[..end]);
                    let length: usize = headers.lines().find_map(|line| {
                        let (name, value) = line.split_once(':')?;
                        name.eq_ignore_ascii_case("content-length").then(|| value.trim().parse().unwrap())
                    }).unwrap_or(0);
                    if request.len() >= end + 4 + length { break; }
                }
            }
            socket.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n").await.unwrap();
            let body = if attempt == 0 {
                "data: {\"choices\":[{\"delta\":{\"content\":\"{\\\"wrong\\\":\"}}]}\n\n".to_owned()
            } else {
                format!("data: {}\n\ndata: [DONE]\n\n", completion("{\"ok\":true}"))
            };
            socket.write_all(format!("{:x}\r\n{body}\r\n", body.len()).as_bytes()).await.unwrap();
            if attempt == 0 {
                let closed = tokio::time::timeout(std::time::Duration::from_secs(3), socket.read(&mut buffer)).await.unwrap().unwrap();
                assert_eq!(closed, 0, "idle upstream socket must be released before retry");
            } else {
                socket.write_all(b"0\r\n\r\n").await.unwrap();
            }
        }
    });
    let adapter = OpenAiClient { cfg: cfg(url), http: reqwest::Client::builder()
        .retry(reqwest::retry::never()).read_timeout(std::time::Duration::from_millis(150))
        .timeout(std::time::Duration::from_secs(2)).build().unwrap() };
    let client = crate::llm::policy::PolicyClient::new(Box::new(adapter), Default::default());
    let result = client.structured(request(&schema())).await;
    worker.await.unwrap();
    assert_eq!(result.unwrap(), json!({"ok":true}));
}

#[tokio::test]
async fn sdk_handles_unicode_and_multiline_events_fragmented_at_every_byte() {
    let (base_url, worker) = fragmented_server(
        ": heartbeat\r\n\r\ndata: {\"choices\":\r\ndata: [{\"delta\":{\"content\":\"你好🙂\"}}]}\r\n\r\n\
        data: {\"choices\":[{\"delta\":{\"content\":\"世界\"},\"finish_reason\":\"stop\"}]}\r\n\r\n".into());
    let deltas = Arc::new(parking_lot::Mutex::new(Vec::<String>::new()));
    let sink = deltas.clone();
    let full = client(base_url).stream_text(text_request(), Arc::new(move |delta| sink.lock().push(delta.to_owned()))).await.unwrap();
    worker.join().unwrap();
    assert_eq!(full, "你好🙂世界");
    assert_eq!(*deltas.lock(), vec!["你好🙂", "世界"]);
}

#[tokio::test]
async fn sdk_handles_an_initial_bom_fragmented_at_every_byte() {
    let (base_url, worker) = fragmented_server(
        "\u{feff}data: {\"choices\":[{\"delta\":{\"content\":\"你好🙂\"}}]}\r\n\r\n\
        data: {\"choices\":[{\"delta\":{\"content\":\"世界\"},\"finish_reason\":\"stop\"}]}\r\n\r\n".into());
    let deltas = Arc::new(parking_lot::Mutex::new(Vec::<String>::new()));
    let sink = deltas.clone();
    let result = tokio::time::timeout(std::time::Duration::from_secs(5),
        client(base_url).stream_text(text_request(), Arc::new(move |delta| sink.lock().push(delta.to_owned())))).await;
    worker.join().unwrap();
    assert_eq!(result.unwrap().unwrap(), "你好🙂世界");
    assert_eq!(*deltas.lock(), vec!["你好🙂", "世界"]);
}

#[tokio::test]
async fn adjacent_initial_boms_are_safe_in_buffered_and_fragmented_provider_responses() {
    let sse = "\u{feff}\u{feff}data: {\"choices\":[{\"delta\":{\"content\":\"你好🙂\"},\"finish_reason\":\"stop\"}]}\n\n";
    let server = MockServer::start().await;
    mount_stream(&server, sse).await;
    let deltas = Arc::new(parking_lot::Mutex::new(Vec::<String>::new()));
    let sink = deltas.clone();
    let result = tokio::time::timeout(std::time::Duration::from_secs(5),
        client(server.uri()).stream_text(text_request(), Arc::new(move |delta| sink.lock().push(delta.to_owned())))).await;
    assert_eq!(result.unwrap().unwrap(), "你好🙂");
    assert_eq!(*deltas.lock(), vec!["你好🙂"]);
    let (base_url, worker) = fragmented_server(sse.into());
    let deltas = Arc::new(parking_lot::Mutex::new(Vec::<String>::new()));
    let sink = deltas.clone();
    let result = tokio::time::timeout(std::time::Duration::from_secs(5),
        client(base_url).stream_text(text_request(), Arc::new(move |delta| sink.lock().push(delta.to_owned())))).await;
    worker.join().unwrap();
    assert_eq!(result.unwrap().unwrap(), "你好🙂");
    assert_eq!(*deltas.lock(), vec!["你好🙂"]);
}

#[tokio::test]
async fn streaming_bom_is_safe_with_missing_or_incorrect_response_mime() {
    for mime in ["", "application/octet-stream"] {
        let server = MockServer::start().await;
        let sse = "\u{feff}data: {\"choices\":[{\"delta\":{\"content\":\"你好🙂\"}}]}\n\n\
            data: {\"choices\":[{\"delta\":{\"content\":\"世界\"},\"finish_reason\":\"stop\"}]}\n\n";
        Mock::given(method("POST")).and(path("/chat/completions"))
            .respond_with(ResponseTemplate::new(200).set_body_raw(sse.as_bytes().to_vec(), mime))
            .expect(1).mount(&server).await;
        let deltas = Arc::new(parking_lot::Mutex::new(Vec::<String>::new()));
        let sink = deltas.clone();
        let result = tokio::time::timeout(std::time::Duration::from_secs(5),
            client(server.uri()).stream_text(text_request(), Arc::new(move |delta| sink.lock().push(delta.to_owned())))).await;
        assert_eq!(result.unwrap().unwrap(), "你好🙂世界");
        assert_eq!(*deltas.lock(), vec!["你好🙂", "世界"]);
        assert_eq!(server.received_requests().await.unwrap().len(), 1);
    }
}

#[tokio::test]
async fn bom_normalization_preserves_stream_response_metadata() {
    use reqwest::ResponseBuilderExt;
    let url = reqwest::Url::parse("https://example.test/proxy/chat/completions").unwrap();
    let response: reqwest::Response = http::Response::builder()
        .status(201).version(http::Version::HTTP_11).url(url.clone())
        .header("content-type", "Text/Event-Stream; charset=utf-8")
        .header("content-length", "6")
        .header("x-request-id", "bom-request")
        .header("retry-after", "2")
        .extension(42u32)
        .body(reqwest::Body::from("\u{feff}abc")).unwrap().into();
    let response = bom::normalize(response, CancellationToken::new()).unwrap();
    assert_eq!(response.status(), 201);
    assert_eq!(response.version(), http::Version::HTTP_11);
    assert_eq!(response.url(), &url);
    assert_eq!(response.headers()["x-request-id"], "bom-request");
    assert_eq!(response.headers()["retry-after"], "2");
    assert!(!response.headers().contains_key("content-length"));
    assert_eq!(response.extensions().get::<u32>(), Some(&42));
    assert_eq!(response.bytes().await.unwrap().as_ref(), b"abc");
}

#[tokio::test]
async fn bom_prefix_filter_preserves_short_mismatched_and_noninitial_sequences() {
    for (chunks, expected) in [
        (vec![vec![0xef], vec![0xbb], vec![0xbf], b"data".to_vec()], b"data".to_vec()),
        (vec![vec![0xef, 0xbb, 0xbf, 0xef, 0xbb, 0xbf, b'd']], b"d".to_vec()),
        (vec![vec![0xef], vec![0xbb], vec![0xbf], vec![0xef], vec![0xbb], vec![0xbf], b"data".to_vec()], b"data".to_vec()),
        (vec![vec![0xef, 0xbb, 0xbf, 0xef, 0xbb]], vec![0xef, 0xbb]),
        (vec![vec![0xef]], vec![0xef]),
        (vec![vec![0xef], vec![0xbb]], vec![0xef, 0xbb]),
        (vec![vec![0xef], b"x".to_vec()], vec![0xef, b'x']),
        (vec![vec![0xef], vec![0xbb], b"x".to_vec()], vec![0xef, 0xbb, b'x']),
        (vec![b"data".to_vec(), vec![0xef, 0xbb, 0xbf]], vec![b'd', b'a', b't', b'a', 0xef, 0xbb, 0xbf]),
        (vec![vec![], b"data".to_vec()], b"data".to_vec()),
        (vec![vec![0xef, 0xbb, 0xbf]], vec![]),
    ] {
        let input = futures_util::stream::iter(chunks.into_iter().map(|chunk| Ok::<_, std::io::Error>(bytes::Bytes::from(chunk))));
        let output = bom::strip_prefix(input, CancellationToken::new()).collect::<Vec<_>>().await;
        let actual: Vec<u8> = output.into_iter().flat_map(|chunk| chunk.unwrap().to_vec()).collect();
        assert_eq!(actual, expected);
    }
}
