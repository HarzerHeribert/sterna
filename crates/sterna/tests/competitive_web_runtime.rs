//! The shipped V8 capability surface binds the broker only once `[web]`
//! reaches something (map 2658): before configuration `web` is not a name
//! the cell holds and the model is never told of it, and a helper runtime
//! never holds it at all.
use sterna::contract::SessionId;
use sterna::runtime::isolate::Runtime;
use sterna::runtime::outcome::CellOutcome;
use sterna::runtime::preview::Value;
use sterna::sandbox::profile::Profile;

/// **Map 2658: an unconfigured session holds no `web` at all.** The runtime
/// refuses a cell that reads an unbound name before anything runs, so a
/// `web.fetch` here is a `ReferenceError` naming `web` — not a broker that
/// exists and refuses — and the Runtime block the model reads declares no
/// `web`, so the two agree: the name is neither bound nor promised.
#[test]
fn an_unconfigured_session_binds_no_web_and_declares_none() {
    let root = std::env::current_dir().unwrap();
    let mut runtime = Runtime::new(
        &Profile::compile(&root, None),
        &SessionId::new("web-unconfigured"),
    );
    let outcome = runtime.run_cell("return web.fetch('https://example.com').content;");
    match &outcome {
        CellOutcome::Threw { error, .. } => {
            assert_eq!(error.class, "ReferenceError", "{error:?}");
            assert!(
                error.message.contains("`web` is not defined"),
                "an unconfigured cell must find no `web` to call: {}",
                error.message
            );
        }
        other => panic!("expected a ReferenceError naming `web`, got {other:?}"),
    }
    let block = sterna::prompt::render_runtime_reaching(
        sterna::runtime::bindings::HostGlobals::Every,
        sterna::prompt::Reach::default(),
    );
    assert!(
        !block.contains("declare const web") && !block.contains("web.fetch"),
        "the model was told about a `web` the cell does not hold:\n{block}"
    );
}

#[test]
fn web_is_declared_to_the_parent() {
    assert!(
        sterna::prompt::declarations::RUNTIME
            .iter()
            .any(|entry| entry.global == "web" && entry.declaration.contains("search(query"))
    );
}

#[test]
fn web_configuration_is_explicit_and_rejects_unknown_fields() {
    let config = sterna::config::SternaConfig::parse(
        "[web]\nenabled = true\nallow_domains = ['example.com']\n",
    )
    .unwrap();
    assert!(config.web.enabled);
    assert!(!sterna::config::SternaConfig::default().web.enabled);
    assert!(sterna::config::SternaConfig::parse("[web]\nallow_everything = true\n").is_err());
}

struct FixtureTransport;
impl sterna::web::WebTransport for FixtureTransport {
    fn get(
        &self,
        url: &str,
        _max_bytes: usize,
        _timeout: std::time::Duration,
    ) -> Result<sterna::web::WebResponse, String> {
        Ok(sterna::web::WebResponse {
            status: 200,
            location: None,
            content_type: "application/json".into(),
            body: if url.contains("/search?") {
                br#"{"results":[{"title":"Fixture","url":"https://example.com/page","content":"source excerpt"}]}"#.to_vec()
            } else {
                b"source content".to_vec()
            },
        })
    }
}

#[test]
fn native_cells_can_search_then_fetch_and_retain_source_provenance() {
    let root = std::env::current_dir().unwrap();
    let broker = sterna::web::WebBroker::with_transport(
        sterna::web::WebConfig {
            enabled: true,
            // The fixture's domain, so the fetch after the search is allowed:
            // this test is about provenance, not the allow list.
            allow_domains: vec!["example.com".into()],
            search_endpoint: Some("https://example.com/search".into()),
            ..Default::default()
        },
        Box::new(FixtureTransport),
    )
    .unwrap();
    let mut runtime = Runtime::new(
        &Profile::compile(&root, None),
        &SessionId::new("web-integrated"),
    )
    .with_web_broker(broker);
    let first = runtime.run_cell("const found = web.search('fixture'); const source = web.fetch(found.results[0].url); return source.content + ' ' + source.citation;");
    match first {
        CellOutcome::Returned { value, turn, .. } => {
            assert_eq!(
                value,
                Value::string("source content https://example.com/page")
            );
            let record = serde_json::to_string(&turn.record).unwrap();
            assert!(record.contains("web.search"));
            assert!(record.contains("web.fetch"));
        }
        other => panic!("expected web result, got {other:?}"),
    }
}
