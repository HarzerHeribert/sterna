use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use sterna::web::{WebBroker, WebConfig, WebResponse, WebTransport, public_ip};

struct Fake {
    replies: Mutex<VecDeque<WebResponse>>,
    urls: Arc<Mutex<Vec<String>>>,
}
impl WebTransport for Fake {
    fn get(&self, url: &str, _: usize, timeout: Duration) -> Result<WebResponse, String> {
        assert!(timeout <= Duration::from_secs(20));
        self.urls.lock().unwrap().push(url.into());
        self.replies
            .lock()
            .unwrap()
            .pop_front()
            .ok_or("unexpected request".into())
    }
}
fn response(status: u16, content_type: &str, body: &str, location: Option<&str>) -> WebResponse {
    WebResponse {
        status,
        content_type: content_type.into(),
        body: body.as_bytes().to_vec(),
        location: location.map(str::to_owned),
    }
}
fn broker(config: WebConfig, replies: Vec<WebResponse>) -> (WebBroker, Arc<Mutex<Vec<String>>>) {
    let urls = Arc::new(Mutex::new(Vec::new()));
    let fake = Fake {
        replies: Mutex::new(replies.into()),
        urls: urls.clone(),
    };
    (
        WebBroker::with_transport(config, Box::new(fake)).unwrap(),
        urls,
    )
}
/// Enabled with `example.com` allowed: since map 2656 an empty allow list
/// refuses every fetch, so a fixture that fetches names its domain.
fn enabled() -> WebConfig {
    WebConfig {
        enabled: true,
        allow_domains: vec!["example.com".into(), "*.example.com".into()],
        ..WebConfig::default()
    }
}

/// **Map 2656: refused until a domain is allowed.** Enabled with nothing
/// allowed, a fetch is refused by a sentence naming the setting, and the
/// transport is never asked — an empty list is not "everything".
#[test]
fn an_empty_allow_list_refuses_every_fetch_and_names_the_setting() {
    let config = WebConfig {
        enabled: true,
        ..WebConfig::default()
    };
    assert!(!config.fetch_configured());
    assert!(
        !config.configured(),
        "nothing is configured, so `web` does not exist"
    );
    let (web, urls) = broker(config, vec![response(200, "text/plain", "never", None)]);
    let refusal = web.fetch("https://example.com").unwrap_err();
    assert!(
        refusal.contains("no domain is allowed") && refusal.contains("allow_domains"),
        "{refusal}"
    );
    assert!(urls.lock().unwrap().is_empty(), "the transport was asked");
    // And the same list with one domain reaches it.
    let (web, urls) = broker(enabled(), vec![response(200, "text/plain", "ok", None)]);
    assert_eq!(web.fetch("https://example.com").unwrap().content, "ok");
    assert_eq!(urls.lock().unwrap().len(), 1);
}

#[test]
fn disabled_and_unconfigured_search_do_not_touch_transport() {
    let (web, urls) = broker(WebConfig::default(), vec![]);
    assert!(
        web.fetch("https://example.com")
            .unwrap_err()
            .contains("disabled")
    );
    assert!(urls.lock().unwrap().is_empty());
    let (web, urls) = broker(enabled(), vec![]);
    let refusal = web.search("rust").unwrap_err();
    assert!(
        refusal.contains("no search provider is configured"),
        "{refusal}"
    );
    assert!(urls.lock().unwrap().is_empty());
}

#[test]
fn domain_deny_wins_and_wildcards_require_boundary() {
    let config = WebConfig {
        allow_domains: vec!["*.example.com".into()],
        deny_domains: vec!["secret.example.com".into()],
        ..enabled()
    };
    let (web, _) = broker(config, vec![]);
    assert!(web.validate_url("https://docs.example.com/a").is_ok());
    for url in [
        "https://example.com",
        "https://fakeexample.com",
        "https://secret.example.com",
        "https://docs.example.com.evil.com",
    ] {
        assert!(web.validate_url(url).is_err(), "{url}");
    }
}

#[test]
fn private_literals_credentials_and_non_http_schemes_are_rejected() {
    let (web, urls) = broker(enabled(), vec![]);
    for url in [
        "file:///etc/passwd",
        "http://example.com",
        "https://user:pass@example.com",
        "https://localhost/a",
        "https://a.local",
        "https://127.0.0.1",
        "https://10.0.0.1",
        "https://169.254.169.254",
        "https://[::1]",
        "https://[::ffff:127.0.0.1]",
        "https://example.com./",
    ] {
        assert!(web.fetch(url).is_err(), "{url}");
    }
    assert!(urls.lock().unwrap().is_empty());
}

#[test]
fn resolver_address_filter_excludes_private_and_transition_ranges() {
    for address in [
        "0.0.0.0",
        "127.0.0.1",
        "10.2.3.4",
        "172.16.0.1",
        "192.168.1.1",
        "100.64.0.1",
        "198.18.0.1",
        "169.254.169.254",
        "224.0.0.1",
        "240.0.0.1",
        "::1",
        "fe80::1",
        "fc00::1",
        "::ffff:10.0.0.1",
        "64:ff9b::a00:1",
        "2002:7f00:1::",
        "2001:db8::1",
    ] {
        assert!(!public_ip(address.parse().unwrap()), "{address}");
    }
    for address in ["1.1.1.1", "8.8.8.8", "2606:4700:4700::1111"] {
        assert!(public_ip(address.parse().unwrap()), "{address}");
    }
}

#[test]
fn redirects_recheck_policy_before_second_request() {
    for destination in [
        "https://127.0.0.1/admin",
        "https://blocked.example.com/a",
        "//localhost/a",
    ] {
        let config = WebConfig {
            deny_domains: vec!["blocked.example.com".into()],
            ..enabled()
        };
        let (web, urls) = broker(config, vec![response(302, "", "", Some(destination))]);
        assert!(web.fetch("https://example.com").is_err());
        assert_eq!(urls.lock().unwrap().len(), 1);
    }
}

#[test]
fn relative_redirect_returns_final_citation_and_untrusted_text() {
    let (web, urls) = broker(
        enabled(),
        vec![
            response(302, "", "", Some("next")),
            response(200, "text/html; charset=utf-8", "<h1>Hello</h1>", None),
        ],
    );
    let result = web.fetch("https://example.com/docs/start").unwrap();
    assert_eq!(result.citation, "https://example.com/docs/next");
    assert!(result.untrusted_content);
    assert_eq!(result.content, "<h1>Hello</h1>");
    assert_eq!(urls.lock().unwrap().len(), 2);
}

#[test]
fn excessive_redirects_large_responses_nontext_and_http_errors_fail() {
    let (web, urls) = broker(
        enabled(),
        (0..6)
            .map(|_| response(302, "", "", Some("/again")))
            .collect(),
    );
    assert!(
        web.fetch("https://example.com")
            .unwrap_err()
            .contains("redirect limit")
    );
    assert_eq!(urls.lock().unwrap().len(), 6);
    let (web, _) = broker(
        WebConfig {
            max_response_bytes: 3,
            ..enabled()
        },
        vec![response(200, "text/plain", "large", None)],
    );
    assert!(
        web.fetch("https://example.com")
            .unwrap_err()
            .contains("byte limit")
    );
    let (web, _) = broker(enabled(), vec![response(200, "image/png", "png", None)]);
    assert!(
        web.fetch("https://example.com")
            .unwrap_err()
            .contains("content type")
    );
    let (web, _) = broker(enabled(), vec![response(403, "text/plain", "denied", None)]);
    assert!(
        web.fetch("https://example.com")
            .unwrap_err()
            .contains("HTTP 403")
    );
}

#[test]
fn configured_search_encodes_query_and_returns_filtered_citations() {
    // No allow list: the endpoint is reached by being configured, and the
    // hits answer to the deny list and the private-address rule only.
    let config = WebConfig {
        enabled: true,
        search_endpoint: Some("https://search.example.com/search".into()),
        deny_domains: vec!["blocked.example.com".into()],
        ..WebConfig::default()
    };
    assert!(config.search_configured() && config.configured());
    let payload = r#"{"results":[{"title":"Rust","url":"https://rust-lang.org/","content":"A language"},{"title":"Bad","url":"https://blocked.example.com/"},{"title":"Local","url":"http://localhost/"}]}"#;
    let (web, urls) = broker(
        config,
        vec![response(200, "application/json", payload, None)],
    );
    let result = web.search("rust & tools").unwrap();
    assert_eq!(
        urls.lock().unwrap()[0],
        "https://search.example.com/search?q=rust%20%26%20tools&format=json"
    );
    assert_eq!(result.results.len(), 1);
    assert_eq!(result.results[0].snippet, "A language");
    assert_eq!(result.citations, ["https://rust-lang.org/"]);
    assert!(result.untrusted_content);
}

#[test]
fn config_rejects_misspellings_and_unbounded_values() {
    assert!(serde_json::from_str::<WebConfig>(r#"{"enabeld":true}"#).is_err());
    assert!(
        WebBroker::new(WebConfig {
            max_response_bytes: 0,
            ..enabled()
        })
        .is_err()
    );
    assert!(
        WebBroker::new(WebConfig {
            timeout_seconds: 0,
            ..enabled()
        })
        .is_err()
    );
    assert!(
        WebBroker::new(WebConfig {
            allow_domains: vec!["https://example.com".into()],
            ..enabled()
        })
        .is_err()
    );
}

#[test]
fn cancellable_fetch_never_dispatches_after_pre_cancel_or_redirect_cancel() {
    use sterna::tools::invoke::CancellationToken;
    struct CancelRedirect {
        token: CancellationToken,
        calls: Arc<Mutex<usize>>,
    }
    impl WebTransport for CancelRedirect {
        fn get(&self, _: &str, _: usize, timeout: Duration) -> Result<WebResponse, String> {
            assert!(timeout <= Duration::from_secs(10));
            *self.calls.lock().unwrap() += 1;
            self.token.cancel();
            Ok(response(302, "", "", Some("https://example.com/next")))
        }
    }
    let token = CancellationToken::new();
    let (web, urls) = broker(enabled(), vec![]);
    token.cancel();
    assert!(
        web.fetch_cancellable("https://example.com", &token)
            .is_err()
    );
    assert!(urls.lock().unwrap().is_empty());
    let token = CancellationToken::new();
    let calls = Arc::new(Mutex::new(0));
    let web = WebBroker::with_transport(
        enabled(),
        Box::new(CancelRedirect {
            token: token.clone(),
            calls: calls.clone(),
        }),
    )
    .unwrap();
    assert!(
        web.fetch_cancellable("https://example.com", &token)
            .is_err()
    );
    drop(web); // joins the bounded worker, so a late second request is observable
    assert_eq!(*calls.lock().unwrap(), 1);
}

#[test]
fn cancellation_returns_promptly_and_drop_reaps_inflight_worker() {
    use std::sync::atomic::{AtomicBool, Ordering};
    use sterna::tools::invoke::CancellationToken;
    struct Slow {
        token: CancellationToken,
        finished: Arc<AtomicBool>,
        release: Mutex<std::sync::mpsc::Receiver<()>>,
    }
    impl WebTransport for Slow {
        fn get(&self, _: &str, _: usize, _: Duration) -> Result<WebResponse, String> {
            self.token.cancel();
            self.release
                .lock()
                .unwrap()
                .recv_timeout(Duration::from_secs(10))
                .unwrap();
            self.finished.store(true, Ordering::SeqCst);
            Ok(response(200, "text/plain", "complete", None))
        }
    }
    let token = CancellationToken::new();
    let finished = Arc::new(AtomicBool::new(false));
    let (release, receiver) = std::sync::mpsc::channel();
    let web = WebBroker::with_transport(
        enabled(),
        Box::new(Slow {
            token: token.clone(),
            finished: finished.clone(),
            release: Mutex::new(receiver),
        }),
    )
    .unwrap();
    assert!(
        web.fetch_cancellable("https://example.com", &token)
            .is_err()
    );
    assert!(
        !finished.load(Ordering::SeqCst),
        "callback waited for the in-flight request"
    );
    release.send(()).unwrap();
    drop(web);
    assert!(
        finished.load(Ordering::SeqCst),
        "worker was detached on broker drop"
    );
}
