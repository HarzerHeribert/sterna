//! `web.search` (map 2657): a configured provider answers with bounded
//! excerpts that carry their source URLs, the key reaches only the request
//! header, and nothing configured means no search.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use sterna::web::search::{
    BRAVE_DEFAULT_KEY_VAR, BRAVE_ENDPOINT, BRAVE_KEY_HEADER, MAX_SNIPPET_BYTES, SearchProvider,
    credentials_file_value, parse_brave, resolve_key_from,
};
use sterna::web::{WebBroker, WebConfig, WebResponse, WebTransport};

/// A fixture value, never a real credential. glasshouse:not-a-secret
const FIXTURE_KEY: &str = "fixture-key-not-real-0000";

/// Every request's URL and headers, in order.
type Seen = Arc<Mutex<Vec<(String, BTreeMap<String, String>)>>>;

/// A transport that records every request's URL and headers and answers
/// from a queue.
struct Recording {
    replies: Mutex<Vec<WebResponse>>,
    seen: Seen,
}

impl WebTransport for Recording {
    fn get(&self, url: &str, max_bytes: usize, timeout: Duration) -> Result<WebResponse, String> {
        self.get_with_headers(url, &BTreeMap::new(), max_bytes, timeout)
    }
    fn get_with_headers(
        &self,
        url: &str,
        headers: &BTreeMap<String, String>,
        _max_bytes: usize,
        _timeout: Duration,
    ) -> Result<WebResponse, String> {
        self.seen
            .lock()
            .unwrap()
            .push((url.to_string(), headers.clone()));
        let mut replies = self.replies.lock().unwrap();
        if replies.is_empty() {
            return Err("unexpected request".into());
        }
        Ok(replies.remove(0))
    }
}

fn broker(config: WebConfig, replies: Vec<WebResponse>) -> (WebBroker, Seen) {
    let seen: Seen = Arc::new(Mutex::new(Vec::new()));
    let transport = Recording {
        replies: Mutex::new(replies),
        seen: seen.clone(),
    };
    (
        WebBroker::with_transport(config, Box::new(transport)).unwrap(),
        seen,
    )
}

fn json(body: &str) -> WebResponse {
    WebResponse {
        status: 200,
        location: None,
        content_type: "application/json".into(),
        body: body.as_bytes().to_vec(),
    }
}

/// Brave's documented response shape, with the fields a real answer carries
/// beside the three sterna reads. One real response must confirm these names
/// (the design's residual risk); this is the shape from the public API
/// documentation.
fn brave_body(long_description: &str) -> String {
    format!(
        r#"{{"type":"search","query":{{"original":"rust tools","show_strict_warning":false}},
"web":{{"type":"search","results":[
{{"title":"Rust tools","url":"https://example.com/tools","is_source_local":false,"description":"Tooling for Rust.","page_age":"2026-01-01","profile":{{"name":"example"}},"language":"en","family_friendly":true,"type":"search_result","subtype":"generic","meta_url":{{"scheme":"https","netloc":"example.com"}}}},
{{"title":"Elsewhere","url":"https://other.org/rust","description":"A denied domain."}},
{{"title":"No source","description":"a hit with no url"}},
{{"title":"Long","url":"https://example.com/long","description":"{long_description}"}}
],"family_friendly":true}}}}"#
    )
}

fn brave_config(key_var: &str) -> WebConfig {
    WebConfig {
        deny_domains: vec!["other.org".into()],
        search_provider: Some("brave".into()),
        search_key_var: Some(key_var.into()),
        ..WebConfig::default()
    }
}

/// A variable name unique to this test process, so parallel tests never
/// share one.
fn var(label: &str) -> String {
    format!("STERNA_TEST_{label}_{}", std::process::id())
}

/// **The contract.** The query goes in the URL and the key in the
/// `X-Subscription-Token` header; the answer names its provider; hits
/// on a denied host and hits with no URL are dropped; a snippet is
/// bounded; the citations are the surviving URLs.
#[test]
fn brave_sends_the_key_in_the_header_and_answers_with_sourced_bounded_excerpts() {
    let key_var = var("BRAVE_KEY");
    // SAFETY: a name unique to this test in a test process.
    unsafe { std::env::set_var(&key_var, FIXTURE_KEY) };
    let long = "x".repeat(MAX_SNIPPET_BYTES + 500);
    let (web, seen) = broker(brave_config(&key_var), vec![json(&brave_body(&long))]);

    let result = web.search("rust tools").unwrap();

    let seen = seen.lock().unwrap();
    assert_eq!(seen.len(), 1, "one request per query");
    let (url, headers) = &seen[0];
    assert!(
        url.starts_with(BRAVE_ENDPOINT)
            && url.contains("q=rust%20tools")
            && url.contains("count=20"),
        "{url}"
    );
    assert_eq!(
        headers.get(BRAVE_KEY_HEADER).map(String::as_str),
        Some(FIXTURE_KEY)
    );
    assert!(
        !url.contains(FIXTURE_KEY),
        "the key must never be in the URL"
    );

    assert_eq!(result.provider, "brave");
    assert_eq!(result.query, "rust tools");
    let urls: Vec<&str> = result.results.iter().map(|hit| hit.url.as_str()).collect();
    assert_eq!(
        urls,
        ["https://example.com/tools", "https://example.com/long"],
        "other.org is denied and the url-less hit is not a source"
    );
    assert_eq!(result.citations, urls);
    assert_eq!(result.results[0].snippet, "Tooling for Rust.");
    let bounded = &result.results[1].snippet;
    assert!(bounded.ends_with('…') && bounded.len() <= MAX_SNIPPET_BYTES + '…'.len_utf8());
    assert!(result.untrusted_content);

    // Nothing the model or the rollout sees carries the key: the serialized
    // result is what the binding records from.
    let serialized = serde_json::to_string(&result).unwrap();
    assert!(!serialized.contains(FIXTURE_KEY));
}

/// A key that resolves to nothing refuses before any request, by the
/// variable's name and the two places it may be set — never a value.
#[test]
fn a_missing_key_refuses_by_the_variables_name_and_asks_no_transport() {
    let key_var = var("BRAVE_MISSING");
    let config = brave_config(&key_var);
    assert!(
        config.search_configured(),
        "a named key variable is configuration"
    );
    let (web, seen) = broker(config, vec![json(&brave_body("d"))]);
    let refusal = web.search("rust").unwrap_err();
    assert!(
        refusal.contains(&key_var)
            && refusal.contains("credentials set --variable")
            && refusal.starts_with("web.search refused"),
        "{refusal}"
    );
    assert!(seen.lock().unwrap().is_empty(), "the transport was asked");
}

/// The environment first, the gateway's credential file second, and a file
/// value that is absent, empty or not a string is no value.
#[test]
fn the_key_comes_from_the_environment_first_and_the_credentials_file_second() {
    let dir = std::env::temp_dir().join(format!("sterna-web-search-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("credentials.toml");
    let key_var = var("ORDER");
    std::fs::write(
        &file,
        format!("{key_var} = \"from-file\"\nOTHER = 3\nEMPTY = \"\"\n"),
    )
    .unwrap();

    assert_eq!(
        resolve_key_from(&key_var, Some(&file)).as_deref(),
        Some("from-file"),
        "unset in the environment, the file answers"
    );
    // SAFETY: a name unique to this test in a test process.
    unsafe { std::env::set_var(&key_var, "from-env") };
    assert_eq!(
        resolve_key_from(&key_var, Some(&file)).as_deref(),
        Some("from-env"),
        "the environment wins over the file"
    );
    assert_eq!(
        resolve_key_from(&key_var, None).as_deref(),
        Some("from-env")
    );
    assert_eq!(credentials_file_value(&file, "OTHER"), None, "not a string");
    assert_eq!(
        credentials_file_value(&file, "EMPTY"),
        None,
        "empty is absent"
    );
    assert_eq!(credentials_file_value(&file, "NOBODY"), None);
    assert_eq!(
        credentials_file_value(&dir.join("missing.toml"), &key_var),
        None
    );
    std::fs::write(&file, "not = = toml").unwrap();
    assert_eq!(
        credentials_file_value(&file, &key_var),
        None,
        "unparseable is absent"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// The keyless provider stays: an endpoint with no key, no header sent, and
/// the answer names it.
#[test]
fn searxng_answers_without_a_key_and_names_its_provider() {
    let config = WebConfig {
        deny_domains: vec!["denied.org".into()],
        search_endpoint: Some("https://search.example.com/search".into()),
        ..WebConfig::default()
    };
    assert_eq!(
        SearchProvider::from_config(&config).unwrap(),
        Some(SearchProvider::Searxng {
            endpoint: "https://search.example.com/search".into()
        }),
        "an endpoint with no provider named is searxng"
    );
    let body = r#"{"results":[{"title":"A","url":"https://example.com/a","content":"a"},{"title":"B","url":"https://denied.org/b","content":"b"}]}"#;
    let (web, seen) = broker(config, vec![json(body)]);
    let result = web.search("a").unwrap();
    assert_eq!(result.provider, "searxng");
    assert_eq!(result.citations, ["https://example.com/a"]);
    let seen = seen.lock().unwrap();
    assert_eq!(
        seen[0].0,
        "https://search.example.com/search?q=a&format=json"
    );
    assert!(seen[0].1.is_empty(), "no header for a keyless provider");
}

/// A configuration whose search could never be asked refuses at
/// construction, so a session does not start with a search that refuses
/// every query; and nothing configured is no search at all.
#[test]
fn an_unaskable_provider_refuses_at_construction_and_none_means_no_search() {
    let searxng_without_endpoint = WebConfig {
        enabled: true,
        search_provider: Some("searxng".into()),
        ..WebConfig::default()
    };
    let refusal = WebBroker::new(searxng_without_endpoint.clone())
        .err()
        .expect("refused");
    assert!(refusal.contains("search_endpoint"), "{refusal}");
    assert!(SearchProvider::from_config(&searxng_without_endpoint).is_err());

    let unknown = WebConfig {
        enabled: true,
        search_provider: Some("bing".into()),
        ..WebConfig::default()
    };
    assert!(
        WebBroker::new(unknown)
            .err()
            .expect("refused")
            .contains("`bing`")
    );

    let not_a_name = WebConfig {
        enabled: true,
        search_provider: Some("brave".into()),
        search_key_var: Some("not a name".into()),
        ..WebConfig::default()
    };
    assert!(
        WebBroker::new(not_a_name)
            .err()
            .expect("refused")
            .contains("variable name")
    );

    // No provider is no search -- while fetch, on by default, still works.
    let nothing = WebConfig::default();
    assert_eq!(SearchProvider::from_config(&nothing).unwrap(), None);
    assert!(!nothing.search_configured() && nothing.configured());
    assert_eq!(nothing.posture(), "web");

    let brave_default_var = WebConfig {
        enabled: true,
        search_provider: Some("brave".into()),
        ..WebConfig::default()
    };
    assert_eq!(
        SearchProvider::from_config(&brave_default_var).unwrap(),
        Some(SearchProvider::Brave {
            key_var: BRAVE_DEFAULT_KEY_VAR.into()
        })
    );
    assert!(brave_default_var.search_configured() && brave_default_var.configured());
    assert_eq!(brave_default_var.posture(), "web");
    assert_eq!(
        brave_default_var.describe(),
        "fetch reaches the allowed hosts and asks for others · search via brave"
    );
}

/// The documented Brave shape parses, extra fields ignored, a hit with no
/// URL dropped; an answer of another shape is one sentence.
#[test]
fn the_documented_brave_shape_parses_and_another_shape_is_refused() {
    let hits = parse_brave(brave_body("d").as_bytes()).unwrap();
    assert_eq!(hits.len(), 3, "three hits carry a url");
    assert_eq!(hits[0].title, "Rust tools");
    assert_eq!(hits[0].snippet, "Tooling for Rust.");
    assert!(parse_brave(br#"{"web":{}}"#).unwrap().is_empty());
    assert!(parse_brave(br#"{}"#).unwrap().is_empty());
    assert!(
        parse_brave(b"<html>")
            .unwrap_err()
            .contains("brave search must return JSON")
    );
}

/// A search endpoint that redirects or fails is refused, never followed with
/// a key in the headers.
#[test]
fn a_redirecting_or_failing_search_endpoint_is_refused_not_followed() {
    let key_var = var("BRAVE_REDIRECT");
    // SAFETY: a name unique to this test in a test process.
    unsafe { std::env::set_var(&key_var, FIXTURE_KEY) };
    let redirect = WebResponse {
        status: 302,
        location: Some("https://evil.example.org/".into()),
        content_type: String::new(),
        body: Vec::new(),
    };
    let (web, seen) = broker(brave_config(&key_var), vec![redirect]);
    let refusal = web.search("rust").unwrap_err();
    assert!(
        refusal.contains("HTTP 302") && refusal.contains("not followed"),
        "{refusal}"
    );
    assert_eq!(seen.lock().unwrap().len(), 1, "no second request");
    assert!(!refusal.contains(FIXTURE_KEY));
}
