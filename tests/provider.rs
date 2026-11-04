use dispatch_mesh::{
    ai::{self, ChatCompletions, Summarizer},
    engine::Workspace,
    model::*,
};
use serde_json::{Value, json};
use std::{
    io::{Read, Write},
    net::TcpListener,
    sync::mpsc,
};

fn serve(content: &str) -> (String, mpsc::Receiver<Value>, std::thread::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}/v1", listener.local_addr().unwrap());
    let body = json!({"choices": [{"message": {"content": content}}]}).to_string();
    let (sender, receiver) = mpsc::channel();
    let thread = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        stream
            .set_read_timeout(Some(std::time::Duration::from_secs(5)))
            .unwrap();
        let mut request = Vec::new();
        let (header_end, length) = loop {
            let mut chunk = [0_u8; 4096];
            let count = stream.read(&mut chunk).unwrap();
            assert!(count > 0);
            request.extend_from_slice(&chunk[..count]);
            if let Some(index) = request.windows(4).position(|b| b == b"\r\n\r\n") {
                let headers = std::str::from_utf8(&request[..index]).unwrap();
                assert!(headers.starts_with("POST /v1/chat/completions "));
                let length: usize = headers
                    .lines()
                    .find_map(|line| {
                        line.to_lowercase()
                            .strip_prefix("content-length:")
                            .map(|v| v.trim().parse().unwrap())
                    })
                    .unwrap();
                break (index + 4, length);
            }
        };
        while request.len() < header_end + length {
            let mut chunk = [0_u8; 4096];
            let count = stream.read(&mut chunk).unwrap();
            assert!(count > 0);
            request.extend_from_slice(&chunk[..count]);
        }
        sender
            .send(serde_json::from_slice(&request[header_end..header_end + length]).unwrap())
            .unwrap();
        write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}", body.len(), body).unwrap();
    });
    (url, receiver, thread)
}

fn session() -> Session {
    let temp = tempfile::tempdir().unwrap();
    let mut workspace = Workspace::init(
        temp.path(),
        Identity {
            name: "Alice".into(),
            email: "alice@example.test".into(),
        },
        None,
        "main".into(),
    )
    .unwrap();
    std::fs::write(temp.path().join("guide.md"), "confidential sample text").unwrap();
    workspace.capture_at(now()).unwrap();
    workspace.active_session().unwrap().clone()
}

#[tokio::test]
async fn compatible_provider_returns_validated_message_and_withholds_text_by_default() {
    let (url, receiver, thread) =
        serve(r#"{"title":"Update guide","description":"Update guide.md."}"#);
    let provider = ChatCompletions {
        config: ProviderConfig {
            base_url: url,
            model: "local-test".into(),
            api_key_env: None,
            json_mode: false,
            send_text: false,
        },
    };
    let message = provider.summarize(&session()).await.unwrap();
    assert_eq!(message.title, "Update guide");
    let request = receiver.recv().unwrap();
    assert!(!request.to_string().contains("confidential sample text"));
    assert!(request.get("response_format").is_none());
    thread.join().unwrap();
}

#[tokio::test]
async fn malformed_summary_falls_back_without_changing_providers() {
    let (url, receiver, thread) = serve("Not valid JSON");
    let temp = tempfile::tempdir().unwrap();
    let mut workspace = Workspace::init(
        temp.path(),
        Identity {
            name: "Alice".into(),
            email: "alice@example.test".into(),
        },
        None,
        "main".into(),
    )
    .unwrap();
    workspace.config.provider = Some(ProviderConfig {
        base_url: url,
        model: "test".into(),
        api_key_env: None,
        json_mode: true,
        send_text: true,
    });
    let message = ai::generate(&workspace.config, &session()).await;
    assert_eq!(message.title, "Update guide.md");
    assert_eq!(
        receiver.recv().unwrap()["response_format"]["type"],
        "json_object"
    );
    thread.join().unwrap();
}

#[test]
fn provider_configuration_accepts_local_models_but_rejects_embedded_secrets() {
    for (url, allowed) in [
        ("http://localhost:11434/v1", true),
        ("https://example.test/v1", true),
        ("http://example.test/v1", false),
        ("https://token@example.test/v1", false),
        ("https://example.test/v1?key=secret", false),
    ] {
        let config = ProviderConfig {
            base_url: url.into(),
            model: "test".into(),
            api_key_env: None,
            json_mode: false,
            send_text: false,
        };
        assert_eq!(ai::validate_config(&config).is_ok(), allowed, "{url}");
    }
}
