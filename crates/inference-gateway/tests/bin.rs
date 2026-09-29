//! The `inference-gateway` binary, driven the way Sterna drives it.
//!
//! Every test here spawns the **built binary** and talks to it over the two
//! channels the contract names — one line of stdout, and stdin as the
//! shutdown signal. Nothing calls a library function to stand in for the
//! process, because the three facts under test are all facts about a
//! process: that the ready line is the first and only thing on stdout, that
//! a request carrying the announced token reaches the provider, and that
//! closing stdin ends it with status `0`.
//!
//! The provider is a loopback fixture in this test process that parses HTTP
//! itself. It re-uses no parser from the crate on purpose: a fixture built
//! on the production reader would agree with it about a request it had
//! mis-framed, and "the request arrived" would stop being a claim about the
//! wire.

use std::io::{BufRead, BufReader, Read, Write};
use std::process::Stdio;
use std::time::Duration;

mod common;
use common::{FakeProvider, gateway, post, wait_for_exit};

/// The whole interprocess contract, end to end: one ready line, a request
/// through the announced address with the announced token reaching the
/// provider, and stdin closing ending it with `0`.
#[test]
fn serve_announces_one_line_forwards_with_it_and_exits_when_stdin_closes() {
    let provider = FakeProvider::start();
    let scratch = tempfile::tempdir().expect("a scratch directory");
    let config_path = scratch.path().join("gateway.toml");
    std::fs::write(
        &config_path,
        format!(
            r#"
[providers.fixture]
base_url = "{}"
protocol = "anthropic-messages"
credential_env = ["GATEWAY_BIN_TEST_KEY"]

[accounts.local]
kind = "api-key"
provider = "fixture"
credential = {{ env = "GATEWAY_BIN_TEST_KEY" }}
"#,
            provider.base_url()
        ),
    )
    .expect("the configuration is written");

    let mut child = gateway(&config_path, scratch.path())
        .args(["serve", "--listen", "127.0.0.1:0"])
        .env("GATEWAY_BIN_TEST_KEY", "fixture-provider-key")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("the built binary runs");

    let mut stdout = BufReader::new(child.stdout.take().expect("stdout was piped"));
    let mut ready = String::new();
    stdout.read_line(&mut ready).expect("a ready line arrives");
    let ready: serde_json::Value =
        serde_json::from_str(ready.trim()).expect("the ready line is one JSON object");
    let listening = ready["listening"]
        .as_str()
        .expect("`listening` is a string");
    let token = ready["token"].as_str().expect("`token` is a string");
    assert_eq!(
        ready.as_object().map(|object| object.len()),
        Some(2),
        "the ready line carries exactly `listening` and `token`: {ready}"
    );
    assert!(
        listening.starts_with("http://127.0.0.1:"),
        "the gateway announces a loopback URL: {listening}"
    );
    assert!(!token.is_empty(), "the gateway announces a token");

    let (status, body) = post(
        &format!("{listening}/v1/messages"),
        &format!("Bearer {token}"),
        r#"{"model":"fixture-model","max_tokens":1,"messages":[{"role":"user","content":"ping"}]}"#,
    );
    assert!(status.contains("200"), "the forward succeeded: {status}");
    assert!(
        body.contains("msg_fixture"),
        "the provider's own answer came back: {body}"
    );

    let seen = provider.requests(1);
    assert_eq!(seen.len(), 1, "the fixture saw exactly one request");
    assert!(
        seen[0].request_line.contains("/v1/messages"),
        "the request target was forwarded verbatim: {}",
        seen[0].request_line
    );
    assert_eq!(
        seen[0].header("authorization"),
        Some("Bearer fixture-provider-key"),
        "the gateway swapped its own token for the provider's credential"
    );

    // Closing stdin is the shutdown channel, and it is the whole of it.
    drop(child.stdin.take().expect("stdin was piped"));
    let status = wait_for_exit(&mut child);
    assert!(
        status.success(),
        "the gateway exits 0 when stdin reaches EOF, got {status:?}"
    );

    // ... and nothing followed the ready line on stdout.
    let mut trailing = String::new();
    stdout
        .read_to_string(&mut trailing)
        .expect("stdout can be drained");
    assert!(
        trailing.trim().is_empty(),
        "stdout carried more than the ready line: {trailing:?}"
    );
}

/// Same-model failover with no host anywhere: two accounts, the first at a
/// port nothing listens on. The first request fails there; the gateway moves
/// the session to the second account on that outcome, and the next request
/// is served — with the provider seeing exactly one request.
#[test]
fn serve_fails_over_to_the_next_account_when_the_first_is_unreachable() {
    let provider = FakeProvider::start();
    let scratch = tempfile::tempdir().expect("a scratch directory");
    let config_path = scratch.path().join("gateway.toml");
    std::fs::write(
        &config_path,
        format!(
            r#"
[providers.dead]
base_url = "http://127.0.0.1:1"
protocol = "anthropic-messages"
credential_env = ["GATEWAY_BIN_TEST_KEY"]

[providers.fixture]
base_url = "{}"
protocol = "anthropic-messages"
credential_env = ["GATEWAY_BIN_TEST_KEY"]

[accounts.a-first]
kind = "api-key"
provider = "dead"
credential = {{ env = "GATEWAY_BIN_TEST_KEY" }}

[accounts.b-second]
kind = "api-key"
provider = "fixture"
credential = {{ env = "GATEWAY_BIN_TEST_KEY" }}
"#,
            provider.base_url()
        ),
    )
    .expect("the configuration is written");

    let mut child = gateway(&config_path, scratch.path())
        .args(["serve", "--listen", "127.0.0.1:0"])
        .env("GATEWAY_BIN_TEST_KEY", "fixture-provider-key")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("the built binary runs");
    let mut stdout = BufReader::new(child.stdout.take().expect("stdout was piped"));
    let mut ready = String::new();
    stdout.read_line(&mut ready).expect("a ready line arrives");
    let ready: serde_json::Value =
        serde_json::from_str(ready.trim()).expect("the ready line is one JSON object");
    let listening = ready["listening"].as_str().expect("`listening`").to_owned();
    let token = ready["token"].as_str().expect("`token`").to_owned();
    let request =
        r#"{"model":"fixture-model","max_tokens":1,"messages":[{"role":"user","content":"ping"}]}"#;

    let (first, _) = post(
        &format!("{listening}/v1/messages"),
        &format!("Bearer {token}"),
        request,
    );
    assert!(
        !first.contains("200"),
        "the first account is unreachable, so the first request fails: {first}"
    );
    let (second, body) = post(
        &format!("{listening}/v1/messages"),
        &format!("Bearer {token}"),
        request,
    );
    assert!(
        second.contains("200"),
        "the gateway failed over to the second account on its own: {second}"
    );
    assert!(
        body.contains("msg_fixture"),
        "served by the fixture: {body}"
    );
    assert_eq!(
        provider.requests(1).len(),
        1,
        "one request reached the fixture"
    );

    drop(child.stdin.take().expect("stdin was piped"));
    let status = wait_for_exit(&mut child);
    assert!(status.success(), "clean exit on stdin EOF, got {status:?}");
}

/// `entitlements --json` over a two-account catalogue: the documented keys,
/// sorted by account, and a subscription row that says which flow connects
/// it and that nothing has.
#[test]
fn entitlements_json_reports_the_documented_shape() {
    let scratch = tempfile::tempdir().expect("a scratch directory");
    let config_path = scratch.path().join("gateway.toml");
    std::fs::write(
        &config_path,
        r#"
[providers.fixture]
base_url = "http://127.0.0.1:1"
credential_env = ["GATEWAY_BIN_TEST_KEY"]

[accounts.zeta]
kind = "claude"
subscription_broker = "cliproxyapi"

[accounts.alpha]
kind = "api-key"
provider = "fixture"
credential = { env = "GATEWAY_BIN_TEST_KEY" }

[accounts.beta]
kind = "api-key"
provider = "fixture"
credential = { env = "GATEWAY_BIN_TEST_KEY" }
models = ["fixture-flash"]
"#,
    )
    .expect("the configuration is written");

    let output = gateway(&config_path, scratch.path())
        .args(["entitlements", "--json"])
        .output()
        .expect("the built binary runs");
    assert!(
        output.status.success(),
        "entitlements exits 0: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let document: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("one JSON document on stdout");
    assert_eq!(document["version"], 1);
    let accounts = document["accounts"]
        .as_array()
        .expect("`accounts` is an array");
    assert_eq!(accounts.len(), 3);
    assert_eq!(
        accounts[0]["account"], "alpha",
        "accounts are sorted by name"
    );
    assert_eq!(accounts[2]["account"], "zeta");

    for account in accounts {
        let object = account.as_object().expect("each account is an object");
        for key in [
            "account",
            "provider",
            "models",
            "scope",
            "selectable",
            "unavailable_reason",
            "authenticated",
            "connect_with",
        ] {
            assert!(object.contains_key(key), "`{key}` is present: {account}");
        }
        assert_eq!(account["selectable"], true);
        assert_eq!(account["unavailable_reason"], serde_json::Value::Null);
    }

    // The provider-backed row names its provider and claims no model,
    // because nothing has read a catalogue for it.
    assert_eq!(accounts[0]["provider"], "fixture");
    assert_eq!(accounts[0]["scope"], "unknown");
    assert_eq!(accounts[0]["models"], serde_json::json!([]));
    assert_eq!(accounts[0]["connect_with"], serde_json::Value::Null);
    assert_eq!(accounts[0]["authenticated"], serde_json::Value::Null);

    // An account that names its own models reports them: they are what
    // routing uses.
    assert_eq!(accounts[1]["scope"], "account-declared");
    assert_eq!(accounts[1]["models"], serde_json::json!(["fixture-flash"]));

    // The subscription row names the flow that would connect it, and says
    // out loud that nothing has. Its `provider` is the broker's own slug
    // because this account states no `vendor`: a stated vendor names it
    // instead, and that fallback order is the host's, copied.
    assert_eq!(accounts[2]["provider"], "cliproxyapi");
    assert_eq!(accounts[2]["connect_with"], "anthropic");
    assert_eq!(accounts[2]["authenticated"], false);
}

/// `--refresh` reads an api-key account's list from its provider the known
/// way, says so, and the listing then reports it.
#[test]
fn refresh_reads_an_api_key_accounts_model_list() {
    let provider = FakeProvider::answering(
        "HTTP/1.1 200 OK",
        "content-type: application/json\r\n",
        r#"{"object":"list","data":[{"id":"fixture-flash"},{"id":"fixture-pro"}]}"#,
    );
    // Beta names its own models, so its provider is never asked.
    let declared = FakeProvider::answering("HTTP/1.1 200 OK", "", r#"{"data":[]}"#);
    let scratch = tempfile::tempdir().expect("a scratch directory");
    let config_path = scratch.path().join("gateway.toml");
    std::fs::write(
        &config_path,
        format!(
            r#"
[providers.fixture]
base_url = "{}"
protocol = "openai-chat"
credential_env = ["GATEWAY_BIN_REFRESH_KEY"]

[providers.declared]
base_url = "{}"
protocol = "openai-chat"
credential_env = ["GATEWAY_BIN_REFRESH_KEY"]

[accounts.alpha]
kind = "api-key"
provider = "fixture"
credential = {{ env = "GATEWAY_BIN_REFRESH_KEY" }}

[accounts.beta]
kind = "api-key"
provider = "declared"
credential = {{ env = "GATEWAY_BIN_REFRESH_KEY" }}
models = ["fixture-flash"]
"#,
            provider.base_url(),
            declared.base_url()
        ),
    )
    .expect("the configuration is written");

    let output = gateway(&config_path, scratch.path())
        .args(["entitlements", "--json", "--refresh"])
        .env("GATEWAY_BIN_REFRESH_KEY", "listing-key")
        .output()
        .expect("the built binary runs");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "{stderr}");
    assert!(
        stderr.contains(&format!(
            "account `alpha`: read 2 model(s) from GET {}/models",
            provider.base_url()
        )),
        "{stderr}"
    );
    let document: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("one JSON document on stdout");
    assert_eq!(document["accounts"][0]["scope"], "provider-declared");
    assert_eq!(
        document["accounts"][0]["models"],
        serde_json::json!(["fixture-flash", "fixture-pro"])
    );
    assert!(
        !stderr.contains("account `beta`") && declared.requests(0).is_empty(),
        "an account naming its own models is not read: {stderr}"
    );
    let seen = provider.requests(1);
    assert_eq!(seen.len(), 1, "one list read, for alpha only");
    assert_eq!(seen[0].header("authorization"), Some("Bearer listing-key"));

    // A list read today is not read again: the model picker refreshes on
    // every open.
    let again = gateway(&config_path, scratch.path())
        .args(["entitlements", "--json", "--refresh"])
        .env("GATEWAY_BIN_REFRESH_KEY", "listing-key")
        .output()
        .expect("the built binary runs");
    assert!(again.status.success());
    // The command has exited, so any request it made has already arrived.
    assert_eq!(
        provider.requests(1).len(),
        1,
        "the fresh list was read again"
    );
}

/// A `--config` naming a file that does not exist is an empty catalogue and
/// a note, not a crash.
#[test]
fn a_missing_config_is_an_empty_catalogue() {
    let scratch = tempfile::tempdir().expect("a scratch directory");
    let output = gateway(&scratch.path().join("no-such-file.toml"), scratch.path())
        .args(["entitlements", "--json"])
        .output()
        .expect("the built binary runs");

    assert!(
        output.status.success(),
        "a missing configuration is not a failure"
    );
    let document: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("one JSON document on stdout");
    assert_eq!(document["version"], 1);
    assert_eq!(document["accounts"], serde_json::json!([]));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("does not exist"),
        "the note says which file was missing: {stderr}"
    );
}

/// `--listen` is refused rather than quietly ignored when it names an
/// address the library cannot bind, and the refusal reaches stderr with
/// nothing on stdout — so a caller reading one line of stdout is never left
/// waiting on a process that has already given up.
#[test]
fn a_fixed_listen_port_is_refused_before_anything_is_bound() {
    let scratch = tempfile::tempdir().expect("a scratch directory");
    let output = gateway(&scratch.path().join("no-such-file.toml"), scratch.path())
        .args(["serve", "--listen", "127.0.0.1:8080"])
        .stdin(Stdio::null())
        .output()
        .expect("the built binary runs");

    assert!(
        !output.status.success(),
        "an address that cannot be honoured is a failure, not a warning"
    );
    assert!(
        output.stdout.is_empty(),
        "nothing reaches stdout when there is no ready line: {:?}",
        String::from_utf8_lossy(&output.stdout)
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("127.0.0.1:8080") && stderr.contains("127.0.0.1:0"),
        "the refusal names what was asked for and what is accepted: {stderr}"
    );
}

/// The standalone bootstrap, end to end: nothing resolves at start, the
/// gateway listens anyway and answers `503` naming the fix; a key stored
/// through `credentials set` — on stdin — is picked up by the *running*
/// gateway on a later request, and the provider sees exactly that key.
#[test]
fn serve_listens_before_a_credential_exists_and_picks_one_up_when_stored() {
    let provider = FakeProvider::start();
    let scratch = tempfile::tempdir().expect("a scratch directory");
    let config_path = scratch.path().join("gateway.toml");
    std::fs::write(
        &config_path,
        format!(
            r#"
[providers.fixture]
base_url = "{}"
protocol = "anthropic-messages"
credential_env = ["GATEWAY_BIN_DEFERRED_KEY"]

[accounts.local]
kind = "api-key"
provider = "fixture"
credential = {{ env = "GATEWAY_BIN_DEFERRED_KEY" }}
"#,
            provider.base_url()
        ),
    )
    .expect("the configuration is written");

    let mut child = gateway(&config_path, scratch.path())
        .args(["serve", "--listen", "127.0.0.1:0"])
        .env_remove("GATEWAY_BIN_DEFERRED_KEY")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("the built binary runs");
    let mut stdout = BufReader::new(child.stdout.take().expect("stdout was piped"));
    let mut ready = String::new();
    stdout
        .read_line(&mut ready)
        .expect("a ready line arrives even with nothing to serve");
    let ready: serde_json::Value =
        serde_json::from_str(ready.trim()).expect("the ready line is one JSON object");
    let listening = ready["listening"].as_str().expect("a URL").to_owned();
    let token = format!("Bearer {}", ready["token"].as_str().expect("a token"));
    let url = format!("{listening}/v1/messages");
    let body =
        r#"{"model":"fixture-model","max_tokens":1,"messages":[{"role":"user","content":"ping"}]}"#;

    let (status, answer) = post(&url, &token, body);
    assert!(
        status.contains("503"),
        "nothing to forward to yet: {status}"
    );
    assert!(
        answer.contains("no account in the catalogue can serve a request")
            && answer.contains("credentials set"),
        "the refusal names the cause and the fix: {answer}"
    );
    assert!(answer.contains(r#""type":"api_error""#), "{answer}");
    assert!(
        provider.requests(0).is_empty(),
        "nothing reached the provider"
    );
    let (status, _) = post(&url, "Bearer not-this-gateways-token", body);
    assert!(
        status.contains("401"),
        "the bearer rule holds while nothing is served: {status}"
    );

    let mut set = gateway(&config_path, scratch.path())
        .args(["credentials", "set", "fixture", "--json"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("the built binary runs");
    set.stdin
        .take()
        .expect("stdin was piped")
        .write_all(b"stored-through-stdin\n")
        .expect("the key is written");
    let set = set.wait_with_output().expect("set exits");
    assert!(
        set.status.success(),
        "storing the key: {}",
        String::from_utf8_lossy(&set.stderr)
    );
    let stored: serde_json::Value =
        serde_json::from_slice(&set.stdout).expect("one JSON object on stdout");
    assert_eq!(stored["provider"], "fixture");
    assert_eq!(stored["variable"], "GATEWAY_BIN_DEFERRED_KEY");
    // Storing the key read the account's model list the known way at once;
    // this provider answers every request with a message, so the line says
    // what came back and how to name the models by hand.
    let lists = stored["model_lists"]
        .as_array()
        .expect("model_lists is an array");
    assert_eq!(lists.len(), 1, "{stored}");
    let line = lists[0].as_str().unwrap_or_default();
    assert!(
        line.starts_with("account `local`: could not read a model list (GET ")
            && line.contains("/v1/models: answered 200")
            && line.ends_with("under [accounts.local]: models = [\"<model id>\", …]"),
        "{line}"
    );

    // A refused rebuild stands for a second before a request tries again.
    std::thread::sleep(Duration::from_millis(1100));
    let (status, answer) = post(&url, &token, body);
    assert!(
        status.contains("200"),
        "the running gateway picked the stored key up: {status} {answer}"
    );
    let seen = provider.requests(2);
    assert_eq!(seen.len(), 2);
    assert!(
        seen[0].request_line.starts_with("GET /v1/models "),
        "{}",
        seen[0].request_line
    );
    assert_eq!(seen[0].header("x-api-key"), Some("stored-through-stdin"));
    assert_eq!(
        seen[1].header("authorization"),
        Some("Bearer stored-through-stdin"),
        "the provider was given the key that was stored, and nothing else"
    );

    drop(child.stdin.take().expect("stdin was piped"));
    let status = wait_for_exit(&mut child);
    assert!(
        status.success(),
        "clean exit after a deferred start: {status:?}"
    );
}

/// `credentials list`, `set` and `remove` in the shapes another program
/// reads, with the refusals a person reads: an empty key and an unknown
/// provider store nothing.
#[test]
fn credentials_list_set_and_remove_report_the_documented_shapes() {
    let scratch = tempfile::tempdir().expect("a scratch directory");
    let config_path = scratch.path().join("gateway.toml");
    std::fs::write(
        &config_path,
        "[providers.fixture]\nbase_url = \"http://127.0.0.1:1\"\nprotocol = \
         \"anthropic-messages\"\ncredential_env = [\"GATEWAY_BIN_LIST_KEY\"]\n",
    )
    .expect("the configuration is written");
    let credentials_file = scratch.path().join("credentials.toml");

    let list = || -> serde_json::Value {
        let output = gateway(&config_path, scratch.path())
            .args(["credentials", "list", "--json"])
            .env_remove("GATEWAY_BIN_LIST_KEY")
            .output()
            .expect("the built binary runs");
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        serde_json::from_slice(&output.stdout).expect("one JSON document")
    };
    let fixture_row = |document: &serde_json::Value| -> serde_json::Value {
        document["providers"]
            .as_array()
            .expect("providers is an array")
            .iter()
            .find(|row| row["provider"] == "fixture")
            .cloned()
            .expect("the configured provider is listed")
    };
    let set = |provider: &str, key: &[u8]| -> std::process::Output {
        let mut child = gateway(&config_path, scratch.path())
            .args(["credentials", "set", provider])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("the built binary runs");
        child
            .stdin
            .take()
            .expect("stdin was piped")
            .write_all(key)
            .expect("written");
        child.wait_with_output().expect("set exits")
    };

    let before = list();
    assert_eq!(before["version"], 1);
    let row = fixture_row(&before);
    assert_eq!(row["variable"], "GATEWAY_BIN_LIST_KEY");
    assert!(row["source"].is_null(), "{row}");
    assert!(
        ["present", "absent", "refused", "unavailable"]
            .contains(&row["native_store"].as_str().unwrap_or_default()),
        "{row}"
    );
    assert!(
        before["providers"]
            .as_array()
            .expect("an array")
            .iter()
            .any(|row| row["provider"] == "anthropic" && row["variable"] == "ANTHROPIC_API_KEY"),
        "the built-in templates are listed too: {before}"
    );

    let empty = set("fixture", b"\n");
    assert!(!empty.status.success(), "an empty key is refused");
    assert!(
        String::from_utf8_lossy(&empty.stderr).contains("no key arrived on stdin"),
        "{}",
        String::from_utf8_lossy(&empty.stderr)
    );
    let unknown = set("no-such-provider", b"a-key\n");
    assert!(!unknown.status.success(), "an unknown provider is refused");
    assert!(
        String::from_utf8_lossy(&unknown.stderr).contains("neither a configured provider"),
        "{}",
        String::from_utf8_lossy(&unknown.stderr)
    );
    assert!(!credentials_file.exists(), "a refused set writes nothing");

    let stored = set("fixture", b"the-key\n");
    assert!(
        stored.status.success(),
        "{}",
        String::from_utf8_lossy(&stored.stderr)
    );
    let said = String::from_utf8_lossy(&stored.stdout);
    assert!(
        said.contains("stored GATEWAY_BIN_LIST_KEY for fixture in"),
        "{said}"
    );
    assert!(
        !said.contains("the-key"),
        "the value is never printed: {said}"
    );
    assert_eq!(fixture_row(&list())["source"], "file");
    assert!(
        std::fs::read_to_string(&credentials_file)
            .expect("the file exists")
            .contains("GATEWAY_BIN_LIST_KEY = \"the-key\""),
        "flat TOML, variable to value"
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let mode = std::fs::metadata(&credentials_file)
            .expect("metadata")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600, "owner-only, was {mode:o}");
    }

    let removed = gateway(&config_path, scratch.path())
        .args(["credentials", "remove", "fixture", "--json"])
        .output()
        .expect("the built binary runs");
    assert!(removed.status.success());
    let removed: serde_json::Value =
        serde_json::from_slice(&removed.stdout).expect("one JSON object");
    assert_eq!(removed["removed"], true);
    assert!(fixture_row(&list())["source"].is_null());
    let again = gateway(&config_path, scratch.path())
        .args(["credentials", "remove", "fixture", "--json"])
        .output()
        .expect("the built binary runs");
    let again: serde_json::Value = serde_json::from_slice(&again.stdout).expect("one JSON object");
    assert_eq!(again["removed"], false, "absent is not an error");
}

/// The broker's executable and an account's login are the gateway's to
/// keep: `adopt-binary` files a copy under its digest and points the marker
/// at it — the path `serve` starts brokers from — and `logout` empties an
/// account's auth directory and leaves it private. A logout with the wrong
/// vendor's flow is refused by name, exactly as `connect` refuses it.
#[cfg(unix)]
#[test]
fn subscriptions_adopt_binary_pins_by_digest_and_logout_forgets_a_login() {
    use std::os::unix::fs::PermissionsExt as _;
    let scratch = tempfile::tempdir().expect("a scratch directory");
    let config_path = scratch.path().join("gateway.toml");
    std::fs::write(
        &config_path,
        "[accounts.zeta]\nkind = \"claude\"\nsubscription_broker = \"cliproxyapi\"\n",
    )
    .expect("the configuration is written");

    let source = scratch.path().join("fake-cliproxyapi");
    std::fs::write(&source, "#!/bin/sh\nexit 0\n").expect("written");
    std::fs::set_permissions(&source, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    let adopted = gateway(&config_path, scratch.path())
        .args(["subscriptions", "adopt-binary"])
        .arg(&source)
        .output()
        .expect("the built binary runs");
    assert!(
        adopted.status.success(),
        "{}",
        String::from_utf8_lossy(&adopted.stderr)
    );
    let root = scratch.path().join("tools").join("cliproxyapi");
    let marker = std::fs::read_to_string(root.join("current")).expect("the marker is written");
    let digest = marker
        .strip_prefix("sha256-")
        .expect("the marker is a digest");
    assert!(
        digest.len() == 64 && digest.bytes().all(|byte| byte.is_ascii_hexdigit()),
        "{marker}"
    );
    let destination = root.join(&marker).join("cliproxyapi");
    assert_eq!(
        String::from_utf8_lossy(&adopted.stdout).trim(),
        destination.display().to_string(),
        "adopt-binary prints where the copy landed"
    );
    assert_eq!(
        std::fs::read(&destination).expect("the copy exists"),
        std::fs::read(&source).expect("the source exists"),
        "byte-identical copy"
    );
    assert_ne!(
        std::fs::metadata(&destination)
            .expect("metadata")
            .permissions()
            .mode()
            & 0o111,
        0,
        "the copy stays executable"
    );
    assert_eq!(
        std::fs::metadata(root.join(&marker))
            .expect("metadata")
            .permissions()
            .mode()
            & 0o777,
        0o700,
        "the release directory is private"
    );

    // Logging out: a login present, then absent, then a private empty
    // directory ready for the next connect.
    let auth = scratch
        .path()
        .join("subscription-brokers")
        .join(format!("entitlement-{}", hex_of("zeta")))
        .join("auth");
    std::fs::create_dir_all(&auth).expect("created");
    std::fs::write(auth.join("claude-someone.json"), "{}").expect("a login file");
    let refused = gateway(&config_path, scratch.path())
        .args(["subscriptions", "logout", "openai", "--entitlement", "zeta"])
        .output()
        .expect("the built binary runs");
    assert!(
        !refused.status.success(),
        "the wrong vendor's flow is refused"
    );
    assert!(
        String::from_utf8_lossy(&refused.stderr)
            .contains("connected with `anthropic`, not `openai`"),
        "{}",
        String::from_utf8_lossy(&refused.stderr)
    );
    assert!(
        auth.join("claude-someone.json").exists(),
        "a refusal removes nothing"
    );
    let out = gateway(&config_path, scratch.path())
        .args([
            "subscriptions",
            "logout",
            "anthropic",
            "--entitlement",
            "zeta",
        ])
        .output()
        .expect("the built binary runs");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "anthropic\tzeta\tabsent\n"
    );
    assert!(
        std::fs::read_dir(&auth)
            .expect("the auth dir exists again")
            .next()
            .is_none(),
        "the auth directory is empty"
    );
    assert_eq!(
        std::fs::metadata(&auth)
            .expect("metadata")
            .permissions()
            .mode()
            & 0o777,
        0o700
    );
}

#[cfg(unix)]
fn hex_of(text: &str) -> String {
    text.bytes().map(|byte| format!("{byte:02x}")).collect()
}

/// A stand-in for the broker's login: it records its arguments, prints the
/// real login's output shape (an SSH hint naming an address, the link, the
/// paste prompt), reads one pasted line, saves a credential and says where.
#[cfg(unix)]
fn fake_broker_login(scratch: &std::path::Path) -> std::path::PathBuf {
    use std::os::unix::fs::PermissionsExt as _;
    let script = scratch.join("fake-cliproxyapi-login");
    std::fs::write(
        &script,
        r#"#!/bin/sh
auth=$(/usr/bin/sed -n 's/^auth-dir: "\(.*\)"$/\1/p' "$2")
if [ "$3" = "-local-model" ]; then
# Serving, as `connect` starts it to use the saved login once: the model
# list, and one completion -- refused with 401 when a `refuse` marker sits
# beside the auth directory (the broker runs with a cleared environment).
exec python3 - "$2" "$auth/../served.txt" "$auth/../refuse" <<'PY'
import os, socket, sys
lines = open(sys.argv[1]).read().splitlines()
port = int(next(l.split(":", 1)[1].strip() for l in lines if l.startswith("port:")))
listener = socket.socket()
listener.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
listener.bind(("127.0.0.1", port))
listener.listen()
while True:
    connection, _ = listener.accept()
    request = connection.recv(65536).decode(errors="replace")
    line = request.split("\r\n", 1)[0]
    with open(sys.argv[2], "a") as log:
        log.write(line + "\n")
    if line.startswith("GET /v1/models"):
        status, body = "200 OK", '{"data":[{"id":"claude-image-x"},{"id":"claude-haiku-test"}]}'
    elif os.path.exists(sys.argv[3]):
        status, body = "401 Unauthorized", '{"error":"token revoked"}'
    else:
        status, body = "200 OK", '{"choices":[]}'
    connection.sendall(f"HTTP/1.1 {status}\r\ncontent-length: {len(body)}\r\nconnection: close\r\n\r\n{body}".encode())
    connection.close()
PY
fi
echo "$@" > "$auth/../args.txt"
echo "To authenticate from a remote machine, an SSH tunnel may be required."
echo "  ssh -L 54545:127.0.0.1:54545 root@203.0.113.7 -p 22"
case "$3" in
  -codex-device-login)
    echo "Codex device URL: https://auth.openai.com/codex/device"
    echo "Codex device code: ABCD-EFGH"
    file="$auth/codex-me@example.com.json" ;;
  *)
    echo "Visit the following URL to continue authentication:"
    echo "https://claude.ai/oauth/authorize?client_id=x&scope=user%3Aprofile&state=s"
    printf "Paste the Claude callback URL (or press Enter to keep waiting): "
    read pasted
    echo "$pasted" > "$auth/../pasted.txt"
    file="$auth/claude-me@example.com.json" ;;
esac
echo '{}' > "$file"
echo "authentication successful"
echo "Authentication saved to $file"
"#,
    )
    .expect("the stand-in is written");
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    script
}

/// `subscriptions connect` signs in through the broker's own login: the
/// whole link crosses and nothing else the broker printed does, a pasted
/// callback address reaches the broker, and success names the saved account.
#[cfg(unix)]
#[test]
fn connect_drives_the_broker_login_and_forwards_a_pasted_address() {
    use std::io::Write as _;
    let scratch = tempfile::tempdir().expect("a scratch directory");
    let config_path = scratch.path().join("gateway.toml");
    std::fs::write(
        &config_path,
        "[accounts.zeta]\nkind = \"claude\"\nsubscription_broker = \"cliproxyapi\"\n",
    )
    .expect("the configuration is written");
    // A login the gateway was killed during left its private config.
    let stale = scratch
        .path()
        .join("subscription-brokers")
        .join(format!("entitlement-{}", hex_of("zeta")))
        .join("instances/login-stale");
    std::fs::create_dir_all(&stale).expect("a stale login directory");
    let mut child = gateway(&config_path, scratch.path())
        .args([
            "subscriptions",
            "connect",
            "anthropic",
            "--entitlement",
            "zeta",
            "--json",
            "--no-browser",
        ])
        .env(
            "INFERENCE_GATEWAY_CLIPROXYAPI_BIN",
            fake_broker_login(scratch.path()),
        )
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("the built binary runs");
    child
        .stdin
        .take()
        .expect("stdin is piped")
        .write_all(b"http://localhost:54545/callback?code=c&state=s\n")
        .expect("the paste is written");
    let status = wait_for_exit(&mut child);
    let mut stdout = String::new();
    std::io::Read::read_to_string(&mut child.stdout.take().unwrap(), &mut stdout).unwrap();
    assert!(status.success(), "{stdout}");
    let lines: Vec<serde_json::Value> = stdout
        .lines()
        .map(|line| serde_json::from_str(line).expect("one JSON object per line"))
        .collect();
    assert_eq!(
        lines,
        vec![
            serde_json::json!({"state": "opened", "authorize_url": "https://claude.ai/oauth/authorize?client_id=x&scope=user%3Aprofile&state=s", "browser_opened": false}),
            serde_json::json!({"state": "connected", "account": "me@example.com"}),
        ]
    );
    assert!(!stdout.contains("203.0.113.7"), "{stdout}");
    let entitlement = scratch
        .path()
        .join("subscription-brokers")
        .join(format!("entitlement-{}", hex_of("zeta")));
    assert_eq!(
        std::fs::read_to_string(entitlement.join("pasted.txt"))
            .unwrap()
            .trim(),
        "http://localhost:54545/callback?code=c&state=s"
    );
    let args = std::fs::read_to_string(entitlement.join("args.txt")).unwrap();
    assert!(
        args.contains("-claude-login") && args.contains("-no-browser"),
        "{args}"
    );
    assert!(
        entitlement
            .join("auth/claude-me@example.com.json")
            .is_file()
    );
    let leftovers: Vec<_> = std::fs::read_dir(entitlement.join("instances"))
        .unwrap()
        .filter_map(Result::ok)
        .collect();
    assert!(
        leftovers.is_empty(),
        "the login's private config is removed: {leftovers:?}"
    );
}

/// A saved credential the provider refuses is a failed sign-in, never
/// "connected": `connect` uses the login once before it says so, and the
/// served broker was asked for its models and one completion.
#[cfg(unix)]
#[test]
fn connect_fails_when_the_saved_credential_is_refused() {
    use std::io::Write as _;
    let scratch = tempfile::tempdir().expect("a scratch directory");
    let config_path = scratch.path().join("gateway.toml");
    std::fs::write(
        &config_path,
        "[accounts.zeta]\nkind = \"claude\"\nsubscription_broker = \"cliproxyapi\"\n",
    )
    .expect("the configuration is written");
    let entitlement_dir = scratch
        .path()
        .join("subscription-brokers")
        .join(format!("entitlement-{}", hex_of("zeta")));
    std::fs::create_dir_all(&entitlement_dir).expect("the entitlement directory");
    std::fs::write(entitlement_dir.join("refuse"), "").expect("the refusal marker");
    let mut child = gateway(&config_path, scratch.path())
        .args([
            "subscriptions",
            "connect",
            "anthropic",
            "--entitlement",
            "zeta",
            "--json",
            "--no-browser",
        ])
        .env(
            "INFERENCE_GATEWAY_CLIPROXYAPI_BIN",
            fake_broker_login(scratch.path()),
        )
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("the built binary runs");
    child
        .stdin
        .take()
        .expect("stdin is piped")
        .write_all(b"http://localhost:54545/callback?code=c&state=s\n")
        .expect("the paste is written");
    let status = wait_for_exit(&mut child);
    let mut stdout = String::new();
    std::io::Read::read_to_string(&mut child.stdout.take().unwrap(), &mut stdout).unwrap();
    assert!(!status.success(), "{stdout}");
    assert!(!stdout.contains("\"connected\""), "{stdout}");
    assert!(
        stdout.contains("using it failed")
            && stdout.contains("claude-haiku-test was refused with HTTP 401"),
        "{stdout}"
    );
    let served = std::fs::read_to_string(
        scratch
            .path()
            .join("subscription-brokers")
            .join(format!("entitlement-{}", hex_of("zeta")))
            .join("served.txt"),
    )
    .unwrap();
    assert!(
        served.contains("GET /v1/models") && served.contains("POST /v1/chat/completions"),
        "{served}"
    );
}

/// Over SSH there is no browser to open, so an OpenAI account signs in with
/// a device code without being asked, and the code crosses with its link.
#[cfg(unix)]
#[test]
fn connect_over_ssh_uses_a_device_code_for_openai() {
    let scratch = tempfile::tempdir().expect("a scratch directory");
    let config_path = scratch.path().join("gateway.toml");
    std::fs::write(
        &config_path,
        "[accounts.omega]\nkind = \"chatgpt\"\nvendor = \"openai\"\nsubscription_broker = \"cliproxyapi\"\n",
    )
    .expect("the configuration is written");
    let output = gateway(&config_path, scratch.path())
        .args([
            "subscriptions",
            "connect",
            "openai",
            "--entitlement",
            "omega",
            "--json",
        ])
        .env(
            "INFERENCE_GATEWAY_CLIPROXYAPI_BIN",
            fake_broker_login(scratch.path()),
        )
        .env("SSH_CONNECTION", "203.0.113.9 50000 203.0.113.7 22")
        .stdin(Stdio::null())
        .output()
        .expect("the built binary runs");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success(),
        "{stdout}{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let first: serde_json::Value =
        serde_json::from_str(stdout.lines().next().expect("a first line")).unwrap();
    assert_eq!(
        first,
        serde_json::json!({"state": "device_code", "verification_url": "https://auth.openai.com/codex/device", "user_code": "ABCD-EFGH"})
    );
    let entitlement = scratch
        .path()
        .join("subscription-brokers")
        .join(format!("entitlement-{}", hex_of("omega")));
    let args = std::fs::read_to_string(entitlement.join("args.txt")).unwrap();
    assert!(args.contains("-codex-device-login"), "{args}");
}
