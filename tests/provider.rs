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
    let body = json!({"choices": [{"message": {"content": content}}]}).to_string();
    serve_response(200, &body)
}

fn serve_response(
    status: u16,
    body: &str,
) -> (String, mpsc::Receiver<Value>, std::thread::JoinHandle<()>) {
    serve_responses(status, "", body, 1)
}

/// Answer `connections` requests with the same response; `headers` are extra
/// `Name: value\r\n` lines. A retried request needs more than one connection.
fn serve_responses(
    status: u16,
    headers: &str,
    body: &str,
    connections: usize,
) -> (String, mpsc::Receiver<Value>, std::thread::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}/v1", listener.local_addr().unwrap());
    let body = body.to_string();
    let extra = headers.to_string();
    let (sender, receiver) = mpsc::channel();
    let thread = std::thread::spawn(move || {
        for _ in 0..connections {
            respond(&listener, status, &extra, &body, &sender);
        }
    });
    (url, receiver, thread)
}

fn respond(
    listener: &TcpListener,
    status: u16,
    extra: &str,
    body: &str,
    sender: &mpsc::Sender<Value>,
) {
    {
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
        write!(stream, "HTTP/1.1 {status} Test\r\nContent-Type: application/json\r\n{extra}Content-Length: {}\r\nConnection: close\r\n\r\n{}", body.len(), body).unwrap();
    }
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

fn intent_session() -> Session {
    let mut session = session();
    session.intents.insert(
        "intent-test".into(),
        Intent {
            id: "intent-test".into(),
            purpose: "Explain setup for new colleagues".into(),
            paths: ["guide.md".into()].into(),
            created_at: now(),
        },
    );
    session.changes.get_mut("guide.md").unwrap().intent = Some("intent-test".into());
    session
}

#[test]
fn local_fallback_preserves_only_purposes_associated_with_actual_changes() {
    let mut session = intent_session();
    session.changes.insert(
        "unrelated.md".into(),
        Change {
            base: None,
            content: Some(b"unrelated".to_vec()),
            intent: None,
            last_edit: now(),
        },
    );
    session.intents.insert(
        "unused".into(),
        Intent {
            id: "unused".into(),
            purpose: "Unrelated old purpose".into(),
            paths: ["unused.md".into()].into(),
            created_at: now(),
        },
    );
    let message = ai::fallback(&session);
    assert!(
        message
            .description
            .contains("Recorded user intent (requested purpose)")
    );
    assert!(
        message
            .description
            .contains("Files: guide.md\nPurpose: Explain setup for new colleagues")
    );
    assert!(!message.description.contains("Files: unrelated.md"));
    assert!(!message.description.contains("Unrelated old purpose"));
    let change = session.changes.get_mut("guide.md").unwrap();
    change.base = change.content.clone();
    assert!(
        !ai::fallback(&session)
            .description
            .contains("Recorded user intent")
    );
}

#[tokio::test]
async fn metadata_only_mode_uses_factual_local_operations_and_keeps_purpose() {
    let temp = tempfile::tempdir().unwrap();
    let workspace = Workspace::init(
        temp.path(),
        Identity {
            name: "Alice".into(),
            email: "alice@example.test".into(),
        },
        None,
        "main".into(),
    )
    .unwrap();
    let mut config = workspace.config.clone();
    config.provider = Some(ProviderConfig {
        base_url: "http://127.0.0.1:1/v1".into(),
        model: "test".into(),
        api_key_env: None,
        json_mode: false,
        send_text: false,
    });
    let message = ai::generate(&config, &intent_session()).await;
    assert_eq!(message.title, "Add guide.md");
    assert!(message.description.contains("Text sharing is disabled"));
    assert!(
        message
            .description
            .contains("Purpose: Explain setup for new colleagues")
    );
    assert_eq!(
        message.description.matches("Recorded user intent").count(),
        1
    );
}

#[test]
fn factual_messages_distinguish_creation_modification_and_deletion() {
    let mut session = session();
    for (before, after, title, label) in [
        (
            None,
            Some(b"New milestones".to_vec()),
            "Add guide.md",
            "Added",
        ),
        (
            Some(b"Existing milestones".to_vec()),
            Some(b"Existing milestones\n0.7 macOS beta".to_vec()),
            "Update guide.md",
            "Modified",
        ),
        (
            Some(b"Existing milestones".to_vec()),
            None,
            "Delete guide.md",
            "Deleted",
        ),
    ] {
        let change = session.changes.get_mut("guide.md").unwrap();
        change.base = before;
        change.content = after;
        let message = ai::fallback(&session);
        assert_eq!(message.title, title);
        assert!(message.description.contains(&format!("{label}: guide.md")));
    }
}

#[tokio::test]
async fn text_enabled_summaries_receive_the_existing_file_and_its_actual_edits() {
    let (url, receiver, thread) = serve(
        r#"{"title":"Add macOS beta milestone","description":"Insert a macOS beta milestone into the existing plan."}"#,
    );
    let temp = tempfile::tempdir().unwrap();
    let workspace = Workspace::init(
        temp.path(),
        Identity {
            name: "Alice".into(),
            email: "alice@example.test".into(),
        },
        None,
        "main".into(),
    )
    .unwrap();
    let mut config = workspace.config.clone();
    config.provider = Some(ProviderConfig {
        base_url: url,
        model: "test".into(),
        api_key_env: None,
        json_mode: true,
        send_text: true,
    });
    let mut session = intent_session();
    let change = session.changes.get_mut("guide.md").unwrap();
    change.base = Some(b"Existing milestones\n".to_vec());
    change.content = Some(b"Existing milestones\n0.7 macOS beta\n".to_vec());
    let message = ai::generate(&config, &session).await;
    assert_eq!(message.title, "Add macOS beta milestone");
    assert!(
        message
            .description
            .contains("Purpose: Explain setup for new colleagues")
    );
    let request = receiver.recv().unwrap();
    let payload: Value =
        serde_json::from_str(request["messages"][1]["content"].as_str().unwrap()).unwrap();
    let change = &payload["changes"][0];
    assert_eq!(change["operation"], "modified");
    assert_eq!(change["before_exists"], true);
    assert_eq!(change["after_exists"], true);
    assert_eq!(change["content_available"], true);
    assert_eq!(change["before"], "Existing milestones\n");
    assert_eq!(change["after"], "Existing milestones\n0.7 macOS beta\n");
    thread.join().unwrap();
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
    let payload: Value =
        serde_json::from_str(request["messages"][1]["content"].as_str().unwrap()).unwrap();
    assert_eq!(payload["changes"][0]["operation"], "added");
    assert_eq!(payload["changes"][0]["content_available"], false);
    assert!(payload["changes"][0].get("before").is_none());
    assert!(payload["changes"][0].get("after").is_none());
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
    let message = ai::generate(&workspace.config, &intent_session()).await;
    assert_eq!(message.title, "Add guide.md");
    assert!(
        message
            .description
            .contains("Purpose: Explain setup for new colleagues")
    );
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

#[test]
fn endpoint_url_accepts_api_bases_and_full_routes_without_duplication() {
    for base in [
        "https://api.openai.com/v1",
        "https://api.openai.com/v1/",
        "https://api.openai.com/v1/chat/completions",
        "https://api.openai.com/v1/chat/completions/",
    ] {
        let config = ProviderConfig {
            base_url: base.into(),
            model: "test".into(),
            api_key_env: None,
            json_mode: false,
            send_text: false,
        };
        assert_eq!(
            ai::chat_completions_url(&config).unwrap().as_str(),
            "https://api.openai.com/v1/chat/completions"
        );
    }
}

#[tokio::test]
async fn a_full_endpoint_setting_sends_to_the_route_exactly_once() {
    let (url, receiver, thread) =
        serve(r#"{"title":"Update guide","description":"Update guide.md."}"#);
    let provider = ChatCompletions {
        config: ProviderConfig {
            base_url: format!("{url}/chat/completions"),
            model: "test".into(),
            api_key_env: None,
            json_mode: true,
            send_text: false,
        },
    };
    assert_eq!(
        provider.summarize(&session()).await.unwrap().title,
        "Update guide"
    );
    assert_eq!(
        receiver.recv().unwrap()["response_format"]["type"],
        "json_object"
    );
    thread.join().unwrap();
}

#[tokio::test]
async fn missing_credentials_name_the_required_variable_without_attempting_a_request() {
    let name = format!("MESH_MISSING_TEST_{}", uuid::Uuid::new_v4().simple());
    assert!(std::env::var_os(&name).is_none());
    let provider = ChatCompletions {
        config: ProviderConfig {
            base_url: "http://127.0.0.1:1/v1".into(),
            model: "test".into(),
            api_key_env: Some(name.clone()),
            json_mode: false,
            send_text: false,
        },
    };
    let error = provider
        .summarize(&session())
        .await
        .unwrap_err()
        .to_string();
    assert!(error.contains(&name));
    assert!(error.contains("before starting Mesh"));
}

#[tokio::test]
async fn authentication_failures_report_http_status_without_echoing_response_contents() {
    let secret = "sk-test-sensitive-error-body";
    let (url, receiver, thread) =
        serve_response(401, &json!({"error": {"message": secret}}).to_string());
    let provider = ChatCompletions {
        config: ProviderConfig {
            base_url: url,
            model: "test".into(),
            api_key_env: None,
            json_mode: false,
            send_text: false,
        },
    };
    let error = provider.summarize(&session()).await.unwrap_err();
    assert!(error.to_string().contains("HTTP 401"));
    assert!(!format!("{error:#}").contains(secret));
    let _ = receiver.recv().unwrap();
    thread.join().unwrap();
}

#[tokio::test]
async fn invalid_message_errors_do_not_echo_model_output() {
    let secret = "sk-test-sensitive-model-output";
    let (url, receiver, thread) = serve(&serde_json::to_string(secret).unwrap());
    let provider = ChatCompletions {
        config: ProviderConfig {
            base_url: url,
            model: "test".into(),
            api_key_env: None,
            json_mode: false,
            send_text: false,
        },
    };
    let error = provider.summarize(&session()).await.unwrap_err();
    assert!(error.to_string().contains("enable --json-mode"));
    assert!(!format!("{error:#}").contains(secret));
    let _ = receiver.recv().unwrap();
    thread.join().unwrap();
}

async fn too_many_requests_error(headers: &str, body: &str) -> String {
    // The provider retries once, so the mock answers both attempts identically.
    let (url, receiver, thread) = serve_responses(429, headers, body, 2);
    let provider = ChatCompletions {
        config: ProviderConfig {
            base_url: url,
            model: "test".into(),
            api_key_env: None,
            json_mode: false,
            send_text: false,
        },
    };
    let error = provider.summarize(&session()).await.unwrap_err();
    let _ = receiver.recv().unwrap();
    thread.join().unwrap();
    format!("{error:#}")
}

#[tokio::test]
async fn exhausted_quota_is_distinguished_from_a_rate_limit_without_echoing_the_body() {
    let secret = "sk-test-sensitive-quota-body";
    let error = too_many_requests_error(
        "",
        &json!({"error": {"message": secret, "type": "insufficient_quota", "code": "insufficient_quota"}})
            .to_string(),
    )
    .await;
    assert!(error.contains("quota or billing exhausted"), "{error}");
    assert!(!error.contains("rate limit"), "{error}");
    assert!(!error.contains(secret), "{error}");
}

#[tokio::test]
async fn a_rate_limit_reports_the_retry_delay_without_echoing_the_body() {
    let secret = "sk-test-sensitive-rate-body";
    let error = too_many_requests_error(
        "Retry-After: 7\r\n",
        &json!({"error": {"message": secret, "type": "requests", "code": "rate_limit_exceeded"}})
            .to_string(),
    )
    .await;
    assert!(
        error.contains("temporary rate limit; retry after 7 s"),
        "{error}"
    );
    assert!(!error.contains(secret), "{error}");
}

#[tokio::test]
async fn an_unexplained_429_keeps_the_generic_hint() {
    let secret = "sk-test-sensitive-plain-body";
    let error = too_many_requests_error("", secret).await;
    assert!(
        error.contains("HTTP 429: check rate limits and available API quota"),
        "{error}"
    );
    assert!(
        !error.contains("quota or billing exhausted") && !error.contains("temporary rate limit"),
        "{error}"
    );
    assert!(!error.contains(secret), "{error}");
}

#[test]
fn retry_after_accepts_delta_seconds_and_http_dates() {
    // Reference times computed independently (Python calendar.timegm).
    let rfc_example = 784_111_777; // Sun, 06 Nov 1994 08:49:37 GMT
    assert_eq!(ai::retry_after_seconds("120", 0), Some(120));
    assert_eq!(
        ai::retry_after_seconds("Sun, 06 Nov 1994 08:49:37 GMT", rfc_example - 60),
        Some(60)
    );
    assert_eq!(
        ai::retry_after_seconds("Thu, 05 Nov 2026 13:00:00 GMT", 1_793_883_600 - 90),
        Some(90)
    );
    assert_eq!(
        ai::retry_after_seconds("Tue, 29 Feb 2028 00:00:00 GMT", 1_835_395_200 - 1),
        Some(1),
        "leap day"
    );
    assert_eq!(
        ai::retry_after_seconds("Sun, 06 Nov 1994 08:49:37 GMT", rfc_example + 3_600),
        Some(0),
        "an elapsed date means retry now, never a negative delay"
    );

    // RFC 9110 §5.6.7: recipients accept all three HTTP-date forms. The
    // obsolete forms denote the same instant as the preferred one.
    for same_instant in [
        "Sun, 06 Nov 1994 08:49:37 GMT",  // IMF-fixdate
        "Sunday, 06-Nov-94 08:49:37 GMT", // RFC 850
        "Sun Nov  6 08:49:37 1994",       // asctime
    ] {
        assert_eq!(
            ai::retry_after_seconds(same_instant, rfc_example - 60),
            Some(60),
            "future: {same_instant}"
        );
        assert_eq!(
            ai::retry_after_seconds(same_instant, rfc_example + 60),
            Some(0),
            "elapsed: {same_instant}"
        );
    }
    // RFC 850's two-digit year: more than 50 years ahead means the past century.
    let now_2026 = 1_793_883_600; // Thu, 05 Nov 2026 13:00:00 GMT
    assert_eq!(
        ai::retry_after_seconds("Thursday, 05-Nov-26 13:01:30 GMT", now_2026),
        Some(90)
    );
    assert_eq!(
        ai::retry_after_seconds("Wednesday, 01-Jan-76 00:00:00 GMT", now_2026),
        Some(3_345_062_400 - now_2026),
        "2076 is exactly 50 years ahead, so it stays in this century"
    );
    assert_eq!(
        ai::retry_after_seconds("Tuesday, 01-Jan-80 00:00:00 GMT", now_2026),
        Some(0),
        "2080 is more than 50 years ahead, so it means 1980 (elapsed)"
    );
    // The cutoff is the complete timestamp 50 calendar years after now
    // (2076-11-05T13:00:00Z = 3_371_806_800), not merely the year 2076.
    assert_eq!(
        ai::retry_after_seconds("Thursday, 05-Nov-76 12:59:59 GMT", now_2026),
        Some(3_371_806_799 - now_2026),
        "one second before the cutoff is still in the future"
    );
    assert_eq!(
        ai::retry_after_seconds("Thursday, 05-Nov-76 13:00:00 GMT", now_2026),
        Some(3_371_806_800 - now_2026),
        "exactly 50 years ahead is not more than 50 years ahead"
    );
    assert_eq!(
        ai::retry_after_seconds("Friday, 05-Nov-76 13:00:01 GMT", now_2026),
        Some(0),
        "one second past the cutoff means 1976, already elapsed"
    );
    assert_eq!(
        ai::retry_after_seconds("Friday, 31-Dec-76 23:59:59 GMT", now_2026),
        Some(0),
        "a later date in the boundary year also means 1976"
    );
    // On a leap day, the 50th anniversary (2078-02-29) does not exist; the
    // cutoff falls on the following instant, 2078-03-01T00:00:00Z.
    let leap_day_2028 = 1_835_395_200;
    assert_eq!(
        ai::retry_after_seconds("Tuesday, 01-Mar-78 00:00:00 GMT", leap_day_2028),
        Some(3_413_318_400 - leap_day_2028)
    );
    assert_eq!(
        ai::retry_after_seconds("Wednesday, 01-Mar-78 00:00:01 GMT", leap_day_2028),
        Some(0)
    );
    assert_eq!(
        ai::retry_after_seconds("Tue Feb 29 00:00:00 2028", 1_835_395_200 - 1),
        Some(1),
        "asctime leap day"
    );

    for malformed in [
        "Sun, 06 Nov 1994 25:00:00 GMT",
        "Sun, 06 Foo 1994 08:49:37 GMT",
        "Sun, 06 Nov 1994 08:49:37 UTC",
        "Sun, 6 Nov 1994 08:49:37 GMT", // IMF-fixdate needs two-digit days
        "Sunday, 06-Nov-1994 08:49:37 GMT", // RFC 850 needs a two-digit year
        "Sun Nov  6 08:49:37 94",       // asctime needs a four-digit year
        "Sun Nov  6 08:49:37 1994 GMT",
        "Funday, 06-Nov-94 08:49:37 GMT",
        // Impossible calendar dates are rejected, never normalized.
        "Thu, 31 Feb 2028 00:00:00 GMT",
        "Wed, 30 Feb 2028 00:00:00 GMT",
        "Sun, 29 Feb 2027 00:00:00 GMT",
        "Thu, 31 Apr 2026 00:00:00 GMT",
        "Monday, 31-Apr-26 00:00:00 GMT",
        "Sat Feb 30 00:00:00 2028",
        "Sun, 00 Nov 1994 08:49:37 GMT",
        "-5",
        "soon",
    ] {
        assert_eq!(ai::retry_after_seconds(malformed, 0), None, "{malformed}");
    }
}

#[tokio::test]
async fn an_obsolete_http_date_reports_the_remaining_delay() {
    let secret = "sk-test-sensitive-obsolete-date-body";
    for header in [
        "Retry-After: Friday, 31-Dec-99 23:59:59 GMT\r\n",
        "Retry-After: Fri Dec 31 23:59:59 2100\r\n",
    ] {
        let error = too_many_requests_error(
            header,
            &json!({"error": {"message": secret, "code": "rate_limit_exceeded"}}).to_string(),
        )
        .await;
        let seconds: u64 = error
            .split("retry after ")
            .nth(1)
            .and_then(|rest| rest.split(" s").next())
            .and_then(|number| number.parse().ok())
            .unwrap_or_else(|| panic!("no retry delay for {header:?} in: {error}"));
        if header.contains("-99") {
            // Two-digit 99 is more than 50 years ahead of today, so it means 1999: elapsed.
            assert_eq!(seconds, 0, "{error}");
        } else {
            assert!(seconds > 365 * 24 * 3_600, "{error}");
        }
        assert!(!error.contains(secret), "{error}");
    }
}

#[tokio::test]
async fn a_future_http_date_reports_the_remaining_delay() {
    let secret = "sk-test-sensitive-date-body";
    let error = too_many_requests_error(
        "Retry-After: Fri, 31 Dec 2100 23:59:59 GMT\r\n",
        &json!({"error": {"message": secret, "code": "rate_limit_exceeded"}}).to_string(),
    )
    .await;
    let seconds: u64 = error
        .split("retry after ")
        .nth(1)
        .and_then(|rest| rest.split(" s").next())
        .and_then(|number| number.parse().ok())
        .unwrap_or_else(|| panic!("no retry delay in: {error}"));
    assert!(seconds > 365 * 24 * 3_600, "{error}");
    assert!(!error.contains(secret), "{error}");
}

#[tokio::test]
async fn an_elapsed_http_date_reports_no_remaining_delay() {
    let secret = "sk-test-sensitive-elapsed-body";
    let error = too_many_requests_error(
        "Retry-After: Thu, 01 Jan 1970 00:00:10 GMT\r\n",
        &json!({"error": {"message": secret, "code": "rate_limit_exceeded"}}).to_string(),
    )
    .await;
    assert!(
        error.contains("temporary rate limit; retry after 0 s"),
        "{error}"
    );
    assert!(!error.contains(secret), "{error}");
}

#[test]
fn a_failed_summary_is_persisted_for_status_and_cleared_by_the_next_success() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().to_path_buf();
    let mesh = |args: &[&str]| {
        let output = std::process::Command::new(env!("CARGO_BIN_EXE_dispatch-mesh"))
            .arg("--workspace")
            .arg(&root)
            .args(args)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        output.stdout
    };
    let use_provider = |base_url: String| {
        let mut workspace = Workspace::open(&root).unwrap();
        workspace.config.provider = Some(ProviderConfig {
            base_url,
            model: "test".into(),
            api_key_env: None,
            json_mode: false,
            send_text: true,
        });
        workspace.save_config().unwrap();
    };
    drop(
        Workspace::init(
            &root,
            Identity {
                name: "Alice".into(),
                email: "alice@example.test".into(),
            },
            None,
            "main".into(),
        )
        .unwrap(),
    );

    // 1. The provider refuses the request: publication continues with the
    //    factual fallback, and the sanitized failure survives into `status`.
    let secret = "sk-test-sensitive-summary-body";
    let (url, receiver, thread) =
        serve_response(401, &json!({"error": {"message": secret}}).to_string());
    use_provider(url);
    std::fs::write(root.join("first.md"), "first").unwrap();
    mesh(&["session", "finish"]);
    mesh(&["sync"]);
    let _ = receiver.recv().unwrap();
    thread.join().unwrap();
    let status: Value = serde_json::from_slice(&mesh(&["status"])).unwrap();
    assert_eq!(status["status"], "needs_attention", "{status:#}");
    let reason = status["reason"].as_str().unwrap();
    assert!(
        reason.starts_with("AI summary failed:") && reason.contains("HTTP 401"),
        "{reason}"
    );
    assert!(!status.to_string().contains(secret), "{status:#}");
    assert_eq!(status["summary_failure"], status["reason"]);
    assert_eq!(status["sessions"][0]["stage"], "published", "{status:#}");

    // 2. A later successful summary clears it.
    let (url, receiver, thread) =
        serve(r#"{"title":"Add second note","description":"Adds second.md."}"#);
    use_provider(url);
    std::fs::write(root.join("second.md"), "second").unwrap();
    mesh(&["session", "finish"]);
    mesh(&["sync"]);
    let _ = receiver.recv().unwrap();
    thread.join().unwrap();
    let status: Value = serde_json::from_slice(&mesh(&["status"])).unwrap();
    assert_eq!(status["status"], "synced", "{status:#}");
    assert!(status["summary_failure"].is_null() && status["reason"].is_null());
}

#[tokio::test]
async fn excluded_files_are_described_without_their_text_even_when_text_sharing_is_on() {
    let (url, receiver, thread) = serve(
        r#"{"title":"Stop synchronizing the layout","description":"Exclude the per-device layout."}"#,
    );
    let temp = tempfile::tempdir().unwrap();
    let workspace = Workspace::init(
        temp.path(),
        Identity {
            name: "Alice".into(),
            email: "alice@example.test".into(),
        },
        None,
        "main".into(),
    )
    .unwrap();
    let mut config = workspace.config.clone();
    let mut session = session();
    config.provider = Some(ProviderConfig {
        base_url: url,
        model: "test".into(),
        api_key_env: None,
        json_mode: true,
        send_text: true,
    });
    session.changes.insert(
        ".obsidian/workspace.json".into(),
        Change {
            base: Some(b"{\"private-layout\":1}".to_vec()),
            content: None,
            intent: None,
            last_edit: now(),
        },
    );
    session.excluded.insert(".obsidian/workspace.json".into());
    ai::generate(&config, &session).await;
    let request = receiver.recv().unwrap();
    let text = request["messages"][1]["content"].as_str().unwrap();
    assert!(!text.contains("private-layout"), "{text}");
    let payload: Value = serde_json::from_str(text).unwrap();
    let changes = payload["changes"].as_array().unwrap();
    let excluded = changes
        .iter()
        .find(|c| c["path"] == ".obsidian/workspace.json")
        .unwrap();
    assert_eq!(excluded["operation"], "excluded");
    assert_eq!(excluded["content_available"], false);
    assert!(excluded.get("before").is_none() && excluded.get("after").is_none());
    let edited = changes.iter().find(|c| c["path"] == "guide.md").unwrap();
    assert_eq!(edited["operation"], "added");
    assert_eq!(edited["content_available"], true);
    assert_eq!(edited["after"], "confidential sample text");
    thread.join().unwrap();
}
