use lily2claude::adapters::http_upstream::HttpUpstreamAdapter;
use lily2claude::config::warp_pool::WarpPool;
use lily2claude::protocol::openai::{ChatCompletionsRequest, OpenAiMessage};
use std::io::{Read, Write};
use std::net::TcpListener;
use std::thread;
use std::time::Duration;

fn spawn_proxy(status: &'static str, body: &'static str) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind fake proxy");
    let address = listener.local_addr().expect("proxy address");

    thread::spawn(move || {
        for stream in listener.incoming().take(2) {
            let Ok(mut stream) = stream else { continue };
            let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));

            let mut buffer = [0u8; 8192];
            let _ = stream.read(&mut buffer);

            let response = format!(
                "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = stream.write_all(response.as_bytes());
            let _ = stream.flush();
        }
    });

    format!("http://127.0.0.1:{}", address.port())
}

fn sample_request() -> ChatCompletionsRequest {
    ChatCompletionsRequest {
        model: "test-model".to_string(),
        messages: vec![OpenAiMessage {
            role: "user".to_string(),
            content: Some("hello".to_string()),
            tool_calls: None,
            tool_call_id: None,
        }],
        tools: None,
        stream: false,
        max_tokens: Some(16),
        temperature: Some(0.0),
    }
}

#[tokio::test]
async fn test_adapter_fails_over_from_429_to_healthy_node() {
    let rate_limited = spawn_proxy("429 Too Many Requests", r#"{"error":"rate"}"#);
    let healthy = spawn_proxy("200 OK", r#"{"ok":true}"#);

    let pool = WarpPool::for_test(vec![rate_limited.clone(), healthy]);
    let adapter = HttpUpstreamAdapter::new("http://example.invalid/v1", None, Some(pool));

    let response = adapter
        .send_request(&sample_request())
        .await
        .expect("second node must succeed");

    assert_eq!(response.status(), reqwest::StatusCode::OK);
}

#[tokio::test]
async fn test_adapter_returns_429_after_all_nodes_are_limited() {
    let first = spawn_proxy("429 Too Many Requests", r#"{"error":"rate"}"#);
    let second = spawn_proxy("429 Too Many Requests", r#"{"error":"rate"}"#);

    let pool = WarpPool::for_test(vec![first, second]);
    let adapter = HttpUpstreamAdapter::new("http://example.invalid/v1", None, Some(pool));

    let response = adapter
        .send_request(&sample_request())
        .await
        .expect("429 response is returned, not an error");

    assert_eq!(response.status(), reqwest::StatusCode::TOO_MANY_REQUESTS);
}
