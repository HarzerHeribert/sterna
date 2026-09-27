use super::*;

use std::io::{Read, Write};
use std::net::TcpStream;
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use super::fixture::FixtureUpstream;
use crate::secret::Secret;

/// A source file's production code: everything before the first
/// `#[cfg(test)]`, with `//` comments stripped — the idiom
/// `harness/mod.rs` introduced and that `main.rs`, `shim.rs`,
/// `secret/mod.rs` and `session/lifecycle.rs` each keep their own copy
/// of.
///
/// Dropping comment lines is not a convenience here, it is the point:
/// this module's doc comments *name* every path it must not import,
/// while explaining why it does not import them.
fn production_code(source: &str) -> String {
    source
        .split("#[cfg(test)]")
        .next()
        .expect("split always yields at least one part")
        .lines()
        .filter(|line| !line.trim_start().starts_with("//"))
        .collect::<Vec<_>>()
        .join("\n")
}

/// The same thing, cut at the file's inline test **module** rather than at
/// the first `#[cfg(test)]` attribute anywhere in it.
///
/// [`production_code`] stops at the first `#[cfg(test)]` it finds, which is
/// right for a file whose only one introduces `mod tests` and silently
/// wrong for a file carrying a test-only `use` or helper higher up.
/// `gateway/session.rs` has `#[cfg(test)] use super::ingress::Tokens;` on
/// line 45 of 1050, so the import scan below was reading **4%** of the file
/// it most needed to read — and reading it clean. Measured 2026-09-10;
/// `gateway/upstream.rs` is the other one, cut at 42% by a
/// `#[cfg(test)] pub(super) fn for_test`.
///
/// Only the import scan uses this. The relay's no-deserialization scan and
/// the token scan still read [`production_code`] and still carry the same
/// blind spot: widening the rule being extended here is in scope, and
/// changing what every scan in this file sees is not.
fn production_code_to_test_module(source: &str) -> String {
    let lines: Vec<&str> = source.lines().collect();
    let mut cut = lines.len();
    for (index, line) in lines.iter().enumerate() {
        if !line.contains("#[cfg(test)]") {
            continue;
        }
        let follows = lines.get(index + 1).map(|next| next.trim_start());
        if follows.is_some_and(|next| next.starts_with("mod tests") || next.contains(" mod tests"))
        {
            cut = index;
            break;
        }
    }
    lines[..cut]
        .iter()
        .filter(|line| !line.trim_start().starts_with("//"))
        .copied()
        .collect::<Vec<_>>()
        .join("\n")
}

/// Every production source file in this directory, for the scans below.
///
/// Listed rather than walked: `include_str!` needs a literal, and a list
/// that has to be added to when a file is added is a list a reviewer can
/// see is complete.
///
/// `fixture.rs` and `conformance.rs` are absent because both are
/// `#[cfg(test)]` in their entirety: they are not production code, and
/// scanning them would be scanning the tests for the rules the tests
/// exist to check.
fn gateway_sources() -> Vec<(&'static str, &'static str)> {
    let mut sources = relay_sources();
    sources.extend(translate_sources());
    sources.extend(extracted_sources());
    sources
}

/// Files outside this directory that are extracted with it.
///
/// `routing/interactive` is the ranking policy the gateway calls on a
/// provider failure, and it leaves with the gateway rather than staying
/// behind — so the import rule below is its rule too, and by the user ruling
/// of 2026-09-10 it may no more name a harness than `session/mod.rs` may.
/// Held here rather than in [`relay_sources`] because the relay's own
/// no-deserialization rule is about files that move bytes, and this one moves
/// none.
fn extracted_sources() -> Vec<(&'static str, &'static str)> {
    vec![(
        "routing/interactive/mod.rs",
        include_str!("../routing/interactive/mod.rs"),
    )]
}

/// The relay: the files that move bytes and may not parse them.
///
/// "May never read them" is what this said until the user's ruling of
/// 2026-09-03 let `usage.rs` read a provider's own usage figures out of a
/// supported body on the way past. The rule the scans below hold is the one
/// that did not change: nothing in this list turns a response into a
/// document, and `usage.rs` is in the list rather than exempted from it.
fn relay_sources() -> Vec<(&'static str, &'static str)> {
    vec![
        ("gateway/mod.rs", include_str!("mod.rs")),
        ("gateway/http.rs", include_str!("http.rs")),
        ("gateway/ingress.rs", include_str!("ingress.rs")),
        ("gateway/request_model.rs", include_str!("request_model.rs")),
        ("gateway/session.rs", include_str!("session/mod.rs")),
        (
            "gateway/subscription_broker.rs",
            include_str!("subscription_broker.rs"),
        ),
        ("gateway/upstream.rs", include_str!("upstream.rs")),
        // The 2026-09-03 ruling's reader. It is here rather than beside the
        // codecs on purpose: it holds no parser, so the scan below is a real
        // constraint on it and not an exception carved out for it.
        ("gateway/usage.rs", include_str!("usage.rs")),
    ]
}

/// The codecs: the one part of this directory that parses a body, by
/// the Phase 56 ruling — and only for a target the provider does not
/// serve. Held to the harness-import rule like every other file here,
/// and deliberately **not** to the no-deserialization rule, which is the
/// relay's.
fn translate_sources() -> Vec<(&'static str, &'static str)> {
    vec![
        ("gateway/translate/mod.rs", include_str!("translate/mod.rs")),
        (
            "gateway/translate/canonical.rs",
            include_str!("translate/canonical.rs"),
        ),
        (
            "gateway/translate/anthropic.rs",
            include_str!("translate/anthropic.rs"),
        ),
        (
            "gateway/translate/openai_chat.rs",
            include_str!("translate/openai_chat.rs"),
        ),
        (
            "gateway/translate/openai_responses.rs",
            include_str!("translate/openai_responses/mod.rs"),
        ),
        (
            "gateway/translate/gemini.rs",
            include_str!("translate/gemini/mod.rs"),
        ),
        (
            "gateway/translate/stream.rs",
            include_str!("translate/stream.rs"),
        ),
    ]
}

/// The credential a fixture upstream expects to see attached. Planted,
/// so that `!contains` on it is a real assertion rather than a shape
/// check.
const PROVIDER_CREDENTIAL: &str = "sk-planted-provider-key-qqqqwwwweeeerrrr";

/// A gateway pointed at `fixture`, holding [`PROVIDER_CREDENTIAL`].
fn gateway_to(fixture: &FixtureUpstream) -> Gateway {
    Gateway::start(anthropic_upstream_to(&fixture.base_url())).expect("loopback is bindable")
}

/// An upstream serving Anthropic Messages at `base_url` and nothing
/// else — the shape every test in this module written before the
/// ingress served more than one protocol assumes.
fn anthropic_upstream_to(base_url: &str) -> Upstream {
    Upstream::new(
        "fixture".to_owned(),
        vec![Route::new(
            "anthropic-messages".to_owned(),
            &["/messages"],
            base_url,
        )],
        Secret::mint_for_test(PROVIDER_CREDENTIAL),
        crate::routing::CredentialId::new(
            "fixture",
            crate::secret::SecretRef::Environment {
                var: "FIXTURE_API_KEY".to_owned(),
            },
        ),
    )
    .expect("the fixture's base URL is absolute")
}

/// An upstream serving only `openai-chat` — for testing the translated
/// path, which an anthropic-messages-shaped request reaches through
/// `unrouted`/`translate::place` rather than `route_for`. `targets` is
/// empty on purpose: nothing here is ever reached by relay matching, only
/// by `route_named`, which looks the route up by protocol slug alone.
fn openai_chat_upstream_to(base_url: &str) -> Upstream {
    Upstream::new(
        "fixture".to_owned(),
        vec![Route::new("openai-chat".to_owned(), &[], base_url)],
        Secret::mint_for_test(PROVIDER_CREDENTIAL),
        crate::routing::CredentialId::new(
            "fixture",
            crate::secret::SecretRef::Environment {
                var: "FIXTURE_API_KEY".to_owned(),
            },
        ),
    )
    .expect("the fixture's base URL is absolute")
}

/// The smallest OpenAI Chat Completions response `openai_chat::decode_response`
/// accepts: one choice, a `stop` finish reason, and a plain-text message.
const OPENAI_CHAT_COMPLETION: &str = "{\"id\":\"chatcmpl-1\",\"choices\":[{\"index\":0,\"finish_reason\":\"stop\",\"message\":{\"role\":\"assistant\",\"content\":\"ok\"}}]}";

/// The bytes a Claude Code child sends: a bearer token, a JSON body, and
/// a length.
fn messages_request(token: &str, body: &str) -> Vec<u8> {
    format!(
        "POST /v1/messages?beta=true HTTP/1.1\r\n\
         Host: 127.0.0.1\r\n\
         Authorization: Bearer {token}\r\n\
         Content-Type: application/json\r\n\
         Anthropic-Version: 2023-06-01\r\n\
         Content-Length: {}\r\n\
         \r\n\
         {body}",
        body.len()
    )
    .into_bytes()
}

/// Send `raw` to `address` and hand back the still-open connection.
fn send(address: SocketAddr, raw: &[u8]) -> TcpStream {
    let mut client = TcpStream::connect(address).expect("the gateway accepts connections");
    // Generous on purpose, and it costs a correct implementation
    // nothing: every exchange here completes in microseconds. The
    // margin exists so that a loaded machine cannot turn a passing test
    // into a failing one, and it has to stay larger than the fixture's
    // own wait in `a_streamed_response_...` so that a *buffering*
    // implementation is still observed failing rather than timing out
    // here first.
    client
        .set_read_timeout(Some(Duration::from_secs(60)))
        .expect("a non-zero read timeout is valid");
    client
        .write_all(raw)
        .expect("the gateway reads the request");
    client.flush().expect("the gateway reads the request");
    client
}

/// Everything the gateway wrote back, to the close.
fn read_all(mut client: TcpStream) -> String {
    let mut out = Vec::new();
    client
        .read_to_end(&mut out)
        .expect("the gateway answers and then closes");
    String::from_utf8_lossy(&out).into_owned()
}

// --- the token is a credential, and is shaped like one ----------------

/// A length is a real leak: it narrows a key space. So the rendering is
/// identical for every token, and no prefix or suffix of one — however
/// short — survives into it. Lose this and the first `tracing` field
/// that takes a `Gateway` publishes the instance's authentication token
/// to a log file.
#[test]
fn debug_on_a_gateway_token_prints_a_fixed_marker_and_never_the_token() {
    // A stand-in value rather than a generated one, and built through the
    // private field the way `secret`'s twin of this test builds a
    // `Secret`. A real token is 64 hex characters, and `[redacted]`
    // itself contains `a`, `c`, `d` and `e` — so a prefix scan over a
    // *generated* token reports a one-character "leak" roughly a quarter
    // of the time. That is the scan colliding with the marker, not a
    // leak, and a test that fails at random is worth less than no test.
    const VALUE: &str = "ghp_qqqqwwwweeeerrrrttttyyyyuuuu9999";

    let rendered = format!("{:?}", GatewayToken(VALUE.to_owned()));
    assert_eq!(rendered, REDACTED, "the marker must be fixed");
    for n in 1..=VALUE.len() {
        assert!(
            !rendered.contains(&VALUE[..n]),
            "the first {n} characters of the token survived into {rendered:?}"
        );
        assert!(
            !rendered.contains(&VALUE[VALUE.len() - n..]),
            "the last {n} characters of the token survived into {rendered:?}"
        );
    }
    assert!(
        !rendered.contains(&VALUE.len().to_string()),
        "the token's length appeared in {rendered:?}"
    );
    assert_eq!(
        format!("{:?}", GatewayToken(String::new())),
        format!("{:?}", GatewayToken("x".repeat(4096))),
        "an empty token and a 4096-character one must be indistinguishable in Debug output"
    );

    // ... and the same holds for a token that really came from the
    // generator. `expose` is used to *check for* the value, never to
    // print it: the message renders only the marker.
    let minted = GatewayToken::generate().expect("the OS has entropy");
    let rendered = format!("{minted:?}");
    assert_eq!(rendered, REDACTED);
    assert!(
        !rendered.contains(minted.expose()),
        "a minted token survived into {rendered:?}"
    );
}

/// The token is reachable through the whole gateway, so the whole
/// gateway has to be safe to render — a `Debug` on the owner is exactly
/// how a redacted field gets printed anyway. Since this slice the
/// gateway also *holds a provider credential*, so the same rendering has
/// to withhold two different secrets at once.
#[test]
fn debug_on_a_gateway_never_reaches_its_token_or_its_credential() {
    let fixture = FixtureUpstream::answering("HTTP/1.1 200 OK", "", "{}");
    let gateway = gateway_to(&fixture);
    let rendered = format!("{gateway:?}");
    assert!(
        !rendered.contains(gateway.token().expose()),
        "the gateway's own Debug leaked its token"
    );
    assert!(
        !rendered.contains(PROVIDER_CREDENTIAL),
        "the gateway's own Debug leaked the provider credential it holds"
    );
    assert!(
        rendered.contains(REDACTED),
        "the gateway's Debug must show the token's redaction marker, not omit the field"
    );
}

/// The compile-fail guard this codebase can express: a source scan of
/// production code, the same idiom as
/// `secret::a_secret_has_no_display_no_deref_and_no_asref`, which this
/// deliberately mirrors — the packet's rule is that the gateway token is
/// treated *exactly* as a credential, and "exactly" is only checkable if
/// the same check exists.
#[test]
fn a_gateway_token_has_no_display_no_deref_and_no_asref() {
    let code = production_code(include_str!("mod.rs"));
    for forbidden in [
        "Display",
        "Deref",
        "AsRef",
        "Borrow",
        "ToString",
        "Serialize",
        "Deserialize",
        "serde",
    ] {
        assert!(
            !code.contains(forbidden),
            "gateway/mod.rs names `{forbidden}` in production code: the gateway token must \
             not be printable, dereferenceable, borrowable as a str or serializable, \
             because every one of those is a way for a credential to reach output by \
             accident. `expose` is the only door."
        );
    }
}

// --- what the clients are backed by decides, not a flag ---------------

/// The predicate is the whole of "only when at least one active client
/// requires it", so it has to read what the clients are backed by rather
/// than anything that merely travels alongside that. A client that reaches
/// its backend directly must never cause a socket to exist.
///
/// Which *configuration* produces which demand is the caller's translation
/// and is tested where it lives — `profile::tests::\
/// a_profile_demands_the_gateway_only_when_the_gateway_backs_it`. Nothing
/// in this directory can see a launch profile, which is the point.
#[test]
fn only_a_local_gateway_demand_requires_a_gateway() {
    assert!(!gateway_is_required(&[]));
    assert!(!gateway_is_required(&[BackendDemand::Direct]));

    assert!(gateway_is_required(&[BackendDemand::LocalGateway]));
    // One among several is enough: "at least one" is the rule.
    assert!(gateway_is_required(&[
        BackendDemand::Direct,
        BackendDemand::LocalGateway,
    ]));
}

/// Asserted on the *absence* of a gateway rather than on a boolean: the
/// promise is that no listener is bound at all, and a predicate that
/// answered `false` while something still bound a socket would satisfy a
/// boolean assertion and break the promise.
///
/// It also asserts that the upstream was never built. Resolving a
/// credential for a launch that needs no gateway would read a secret
/// nothing was going to use, which is the kind of thing that is only
/// ever noticed after it has been logged somewhere.
#[test]
fn no_client_needing_a_gateway_binds_no_listener_and_resolves_no_credential() {
    let demands = [BackendDemand::Direct, BackendDemand::Direct];
    let mut built = false;
    let started = start_if_required(&demands, || {
        built = true;
        unreachable!("the upstream must not be built for clients that need no gateway")
    })
    .expect("deciding not to start cannot fail");
    assert!(
        started.is_none(),
        "a gateway was bound for clients that never asked for one"
    );
    assert!(!built);
}

/// The other half of the same rule, and the one that keeps it from being
/// satisfied by a function that simply never starts anything.
#[test]
fn a_client_served_by_the_gateway_binds_a_listener() {
    let fixture = FixtureUpstream::answering("HTTP/1.1 200 OK", "", "{}");
    let demands = [BackendDemand::LocalGateway];
    let started = start_if_required(&demands, || Ok(anthropic_upstream_to(&fixture.base_url())))
        .expect("loopback is bindable");
    assert!(
        started.is_some(),
        "a gateway-serving demand did not produce a gateway"
    );
}

// --- the ingress: what the upstream sees ------------------------------

/// The heart of lines 2 and 3. The upstream must see the *provider's*
/// credential, attached by the gateway; the child's own token must not
/// reach it in any header at all.
///
/// Both halves are asserted, and the second is the one that matters: a
/// gateway that attached the provider key while *also* forwarding the
/// child's `authorization` would pass a test that only checked the
/// first, and would be handing an upstream a Glasshouse instance's
/// authentication token.
#[test]
fn a_request_carrying_the_gateway_token_reaches_the_upstream_with_the_provider_credential() {
    let fixture = FixtureUpstream::answering("HTTP/1.1 200 OK", "", "{\"ok\":true}");
    let gateway = gateway_to(&fixture);

    let response = read_all(send(
        gateway.address(),
        &messages_request(gateway.token().expose(), "{\"model\":\"probe\"}"),
    ));
    assert!(response.starts_with("HTTP/1.1 200 OK"), "{response}");
    assert!(response.contains("{\"ok\":true}"), "{response}");

    let request = fixture.only_request();
    assert_eq!(
        request.header("authorization"),
        Some(format!("Bearer {PROVIDER_CREDENTIAL}").as_str()),
        "the gateway did not attach the provider's own credential"
    );
    let rendered = format!("{request:?}");
    assert!(
        !rendered.contains(gateway.token().expose()),
        "the child's gateway token reached the upstream"
    );

    // The request target was appended to the provider's base URL with
    // its query intact, and the method and end-to-end headers survived.
    assert_eq!(request.method, "POST");
    assert_eq!(request.target, "/v1/messages?beta=true");
    assert_eq!(
        request.body, b"{\"model\":\"probe\"}",
        "the request body did not arrive byte-for-byte"
    );
    assert_eq!(request.header("anthropic-version"), Some("2023-06-01"));
    // ... and `host` names the upstream rather than the loopback address
    // the child was pointed at.
    assert_eq!(
        request.header("host"),
        Some(fixture.base_url().trim_start_matches("http://")),
        "the host header was not corrected to the upstream's"
    );
}

/// Pass-through means the provider sees the harness's own headers and
/// **nothing the gateway or its HTTP client decided to add**.
///
/// This is a real hazard rather than a hypothetical one: `ureq` adds a
/// `user-agent`, an `accept` and an `accept-encoding` of its own unless
/// told not to, and the `gzip` feature would additionally advertise an
/// encoding and then transparently decode the response — leaving a
/// `content-encoding` header describing something the client is no
/// longer being sent. `upstream::agent` turns all four off. Lose any of
/// them and the provider sees a client the harness is not, which is
/// exactly what "keep the first gateway implementation protocol
/// pass-through" forbids.
#[test]
fn the_gateway_adds_no_headers_of_its_own_to_a_forwarded_request() {
    let fixture = FixtureUpstream::answering("HTTP/1.1 200 OK", "", "{}");
    let gateway = gateway_to(&fixture);

    read_all(send(
        gateway.address(),
        &messages_request(gateway.token().expose(), "{}"),
    ));

    let request = fixture.only_request();
    let names: Vec<&str> = request
        .headers
        .iter()
        .map(|(name, _)| name.as_str())
        .collect();

    for invented in ["user-agent", "accept", "accept-encoding"] {
        assert!(
            !names.contains(&invented),
            "the gateway's HTTP client added `{invented}` to a request the harness did not \
             send it on: {names:?}"
        );
    }
    // Exactly the harness's own end-to-end headers, plus the framing and
    // routing the next hop requires. Asserted as a set so that an added
    // header fails here rather than being noticed years later in a
    // provider's logs.
    let mut sorted = names.clone();
    sorted.sort_unstable();
    assert_eq!(
        sorted,
        vec![
            "anthropic-version",
            "authorization",
            "content-length",
            "content-type",
            "host",
        ],
        "the forwarded header set changed"
    );
}

/// Capability map line 2451: a relayed exchange a backend actually served
/// answers with a head naming that backend's provider and entitlement —
/// never the credential's own secret bytes, on the same response the
/// existing secret-hygiene tests above assert it on requests.
#[test]
fn a_relayed_response_names_the_serving_backend_and_never_its_secret() {
    let fixture = FixtureUpstream::answering("HTTP/1.1 200 OK", "", "{\"ok\":true}");
    let gateway = gateway_to(&fixture);

    let response = read_all(send(
        gateway.address(),
        &messages_request(gateway.token().expose(), "{\"model\":\"probe\"}"),
    ));

    assert!(response.starts_with("HTTP/1.1 200 OK"), "{response}");
    assert!(
        response.contains("x-glasshouse-provider: fixture\r\n"),
        "a relayed response the backend served must name the serving provider: {response}"
    );
    assert!(
        response.contains("x-glasshouse-entitlement: fixture/FIXTURE_API_KEY\r\n"),
        "a relayed response the backend served must name the serving entitlement: {response}"
    );
    assert!(
        !response.contains(PROVIDER_CREDENTIAL),
        "no response head the gateway writes may ever contain the credential's own secret \
         bytes: {response}"
    );
}

/// Requirement 2: a refusal the gateway writes itself — here, a target
/// belonging to no served protocol — carries neither header, because
/// nothing served it.
#[test]
fn a_refusal_the_gateway_writes_itself_carries_neither_served_by_header() {
    let fixture = FixtureUpstream::answering("HTTP/1.1 200 OK", "", "{}");
    let gateway = gateway_to(&fixture);

    let raw = format!(
        "POST /this-target-belongs-to-no-served-protocol HTTP/1.1\r\n\
         Host: 127.0.0.1\r\n\
         Authorization: Bearer {}\r\n\
         Content-Length: 2\r\n\r\n{{}}",
        gateway.token().expose()
    )
    .into_bytes();
    let response = read_all(send(gateway.address(), &raw));

    assert!(response.starts_with("HTTP/1.1 404"), "{response}");
    assert!(
        !response.contains("x-glasshouse-provider"),
        "a refusal the gateway wrote itself must carry no served-by header: {response}"
    );
    assert!(
        !response.contains("x-glasshouse-entitlement"),
        "a refusal the gateway wrote itself must carry no served-by header: {response}"
    );
    assert_eq!(
        fixture.connections(),
        0,
        "an unrouted refusal must open nothing upstream"
    );
}

/// Requirement 1's translated half: an exchange the ingress places through
/// `translate::serve` names the serving backend on both of the translated
/// path's writers — the document `deliver_document` reaches, and the
/// stream head a `stream: true` request reaches through the
/// document-to-stream conversion.
#[test]
fn a_translated_response_names_the_serving_backend_on_its_document_and_its_stream_head() {
    let fixture = FixtureUpstream::answering("HTTP/1.1 200 OK", "", OPENAI_CHAT_COMPLETION);
    let gateway =
        Gateway::start(openai_chat_upstream_to(&fixture.base_url())).expect("loopback is bindable");

    let document = read_all(send(
        gateway.address(),
        &messages_request(gateway.token().expose(), "{\"model\":\"probe\"}"),
    ));
    assert!(document.starts_with("HTTP/1.1 200 OK"), "{document}");
    assert!(
        document.contains("x-glasshouse-provider: fixture\r\n"),
        "a translated document response must name the serving provider: {document}"
    );
    assert!(
        document.contains("x-glasshouse-entitlement: fixture/FIXTURE_API_KEY\r\n"),
        "a translated document response must name the serving entitlement: {document}"
    );

    let stream = read_all(send(
        gateway.address(),
        &messages_request(
            gateway.token().expose(),
            "{\"model\":\"probe\",\"stream\":true}",
        ),
    ));
    assert!(stream.starts_with("HTTP/1.1 200 OK"), "{stream}");
    assert!(
        stream.contains("x-glasshouse-provider: fixture\r\n"),
        "a translated stream head must name the serving provider: {stream}"
    );
    assert!(
        stream.contains("x-glasshouse-entitlement: fixture/FIXTURE_API_KEY\r\n"),
        "a translated stream head must name the serving entitlement: {stream}"
    );
}

/// A request without this instance's token is refused **before an
/// upstream connection exists**, which is asserted on the fixture's own
/// connection count rather than on the order of two statements.
///
/// The connection count and not the request count: a gateway that
/// opened a socket and then thought better of it would leave no request
/// behind and would still have told the provider that someone was here.
#[test]
fn a_request_without_this_instances_token_is_refused_and_opens_nothing_upstream() {
    let fixture = FixtureUpstream::answering("HTTP/1.1 200 OK", "", "{}");
    let gateway = gateway_to(&fixture);
    let other = GatewayToken::generate().expect("the OS has entropy");

    for wrong in [
        format!("Bearer {}", other.expose()),
        format!("Bearer {}", &gateway.token().expose()[..32]),
        "Bearer".to_owned(),
        String::new(),
    ] {
        let raw = if wrong.is_empty() {
            // No `authorization` header at all.
            b"POST /v1/messages HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Length: 2\r\n\r\n{}".to_vec()
        } else {
            format!(
                "POST /v1/messages HTTP/1.1\r\nHost: 127.0.0.1\r\nAuthorization: {wrong}\r\n\
                 Content-Length: 2\r\n\r\n{{}}"
            )
            .into_bytes()
        };
        let response = read_all(send(gateway.address(), &raw));
        assert!(
            response.starts_with("HTTP/1.1 401 Unauthorized"),
            "a request presenting {wrong:?} was not refused: {response}"
        );
        assert!(
            response.contains("authentication_error"),
            "the refusal must be in the shape the harness's own protocol uses: {response}"
        );
    }

    assert_eq!(
        fixture.connections(),
        0,
        "a refused request opened a connection to the provider"
    );
}

/// A real harness connects first and writes afterwards, so the gateway
/// routinely accepts a connection *before* its request exists. That is
/// the case where an accepted socket which inherited its listener's
/// non-blocking flag — as it does on macOS, the BSDs and Windows, and
/// does not on Linux — answers the first read with `WouldBlock`, and the
/// connection is dropped without a reply.
///
/// Every other test here writes before the gateway can accept, so the
/// bytes are already in the receive buffer and a non-blocking read
/// succeeds anyway. **Removing `set_nonblocking(false)` from the ingress
/// broke nothing until this test existed** — which is exactly the shape
/// of a platform defect that ships.
///
/// The pause is a bound, not a synchronisation: it only has to exceed
/// one `ACCEPT_POLL`, and a pause that turned out to be too short would
/// make this test *weaker* rather than flaky, because both a correct and
/// a broken gateway pass when the write wins the race.
#[test]
fn a_client_that_connects_before_it_writes_is_still_served() {
    let fixture = FixtureUpstream::answering("HTTP/1.1 200 OK", "", "{\"ok\":true}");
    let gateway = gateway_to(&fixture);

    let mut client =
        TcpStream::connect(gateway.address()).expect("the gateway accepts connections");
    client
        .set_read_timeout(Some(Duration::from_secs(60)))
        .expect("a non-zero read timeout is valid");
    std::thread::sleep(ACCEPT_POLL * 20);

    let raw = messages_request(gateway.token().expose(), "{\"model\":\"probe\"}");
    client
        .write_all(&raw)
        .expect("the gateway is still reading");
    client.flush().expect("the gateway is still reading");

    let response = read_all(client);
    assert!(
        response.starts_with("HTTP/1.1 200 OK"),
        "the gateway dropped a connection it had accepted before the request arrived: \
         {response:?}"
    );
    assert_eq!(fixture.only_request().target, "/v1/messages?beta=true");
}

/// Line 4, and the test is built so a buffered implementation cannot
/// pass it rather than so that a streaming one happens to.
///
/// The fixture writes its first event, then **blocks until the client
/// says it has received that event**, and only then writes the second.
/// So the second event exists only if the first reached the client while
/// the response was still open. A gateway that read the upstream body to
/// the end before writing anything would deadlock: the client would
/// never acknowledge, the fixture's wait would time out, and the marker
/// it writes instead is asserted on below.
#[test]
fn a_streamed_response_reaches_the_client_before_the_upstream_has_finished() {
    let (saw_first, first_seen) = mpsc::channel::<()>();
    let first_seen = Mutex::new(first_seen);

    let fixture = FixtureUpstream::start(move |_request, out| {
        let _ = out.write_all(
            b"HTTP/1.1 200 OK\r\n\
              content-type: text/event-stream\r\n\
              transfer-encoding: chunked\r\n\r\n",
        );
        let first = "event: one\ndata: {\"n\":1}\n\n";
        let _ = out.write_all(format!("{:x}\r\n{first}\r\n", first.len()).as_bytes());
        let _ = out.flush();

        let streamed = first_seen
            .lock()
            .expect("no test panics while holding this")
            .recv_timeout(Duration::from_secs(20))
            .is_ok();
        let second = if streamed {
            "event: two\ndata: {\"n\":2}\n\n"
        } else {
            "event: BUFFERED-NOT-STREAMED\n\n"
        };
        let _ = out.write_all(format!("{:x}\r\n{second}\r\n0\r\n\r\n", second.len()).as_bytes());
        let _ = out.flush();
    });

    let gateway = gateway_to(&fixture);
    let mut client = send(
        gateway.address(),
        &messages_request(gateway.token().expose(), "{\"stream\":true}"),
    );

    let mut seen = Vec::new();
    let mut buffer = [0u8; 1024];
    loop {
        let read = client.read(&mut buffer).unwrap_or_else(|err| {
            panic!(
                "the gateway did not deliver the first event before the upstream finished \
                 ({err}); {} bytes had arrived: {:?}",
                seen.len(),
                String::from_utf8_lossy(&seen)
            )
        });
        assert!(
            read > 0,
            "the gateway closed the response before the first event arrived; {} bytes had \
             arrived: {:?}",
            seen.len(),
            String::from_utf8_lossy(&seen)
        );
        seen.extend_from_slice(&buffer[..read]);
        if String::from_utf8_lossy(&seen).contains("event: one") {
            break;
        }
    }
    saw_first.send(()).expect("the fixture is still writing");

    let mut rest = Vec::new();
    client.read_to_end(&mut rest).expect("the stream completes");
    seen.extend_from_slice(&rest);
    let text = String::from_utf8_lossy(&seen);

    assert!(text.contains("event: one"), "{text}");
    assert!(text.contains("event: two"), "{text}");
    assert!(
        !text.contains("BUFFERED-NOT-STREAMED"),
        "the upstream's wait for the first event to reach the client timed out, so the \
         gateway is buffering the response rather than streaming it: {text}"
    );
}

// --- the listener's address, and its lifetime -------------------------

/// Two facts, each of which fails differently. An interface other than v4
/// loopback would put a Glasshouse instance's gateway on the network,
/// which is the outcome this module has no configuration to cause and
/// therefore no way to notice. A port still equal to the one that was
/// *asked for* would mean `local_addr` was never consulted, and the
/// address handed to a child harness would name a port nothing is
/// listening on.
///
/// `is_loopback()` is deliberately not what is asserted: it also accepts
/// `127.0.0.2` and `::1`, and neither of those is an address this module
/// is allowed to bind.
#[test]
fn the_gateway_binds_v4_loopback_on_a_port_the_operating_system_chose() {
    let fixture = FixtureUpstream::answering("HTTP/1.1 200 OK", "", "{}");
    let gateway = gateway_to(&fixture);
    let address = gateway.address();

    assert_eq!(
        address.ip(),
        Ipv4Addr::LOCALHOST,
        "the gateway bound an interface other than v4 loopback"
    );
    assert_ne!(
        address.port(),
        EPHEMERAL_PORT,
        "the address still carries the port that was requested, so the port the operating \
         system actually chose was never read back"
    );
    assert_eq!(gateway.base_url(), format!("http://{address}"));
}

/// "Multiple Glasshouse instances can coexist" is a claim about two
/// listeners being alive *at the same time*, so both are held across the
/// comparison. Drop the first before asking and the operating system is
/// entitled to reissue its port to the second: the assertion would still
/// pass and would have proved nothing.
#[test]
fn two_gateways_in_one_process_bind_different_ports() {
    let fixture = FixtureUpstream::answering("HTTP/1.1 200 OK", "", "{}");
    let first = gateway_to(&fixture);
    let second = gateway_to(&fixture);

    assert_ne!(
        first.address().port(),
        second.address().port(),
        "two gateways bound at the same time claimed the same port"
    );
}

/// A token that repeated across instances would let one Glasshouse
/// authenticate against another's gateway, and would mean the value is
/// not coming from the operating system's generator at all.
///
/// Compared with a bare `assert!` rather than `assert_ne!`, and through
/// the private field that `mod tests` can see: `assert_ne!` renders both
/// operands when it fails, so the single run that ever failed would be
/// the run that published two live credentials into CI output — undoing
/// the hand-written [`Debug`](fmt::Debug) above. The message below names
/// no value and no part of one.
#[test]
fn two_gateways_mint_different_tokens() {
    let fixture = FixtureUpstream::answering("HTTP/1.1 200 OK", "", "{}");
    let first = gateway_to(&fixture);
    let second = gateway_to(&fixture);

    assert!(
        first.token().0 != second.token().0,
        "two gateways minted the same token"
    );
}

/// Nothing here calls a `close` or a `stop`: the port is released only
/// because dropping the [`Gateway`] stops its accept loop and joins it,
/// which drops the listener the loop owns. Lose that and a process which
/// started and finished with several gateways would hold every port it
/// had ever bound until it exited.
///
/// **Now with a live accept loop**, which is what makes this the
/// shutdown test rather than a statement about `Drop` on a struct: the
/// gateway has served a real exchange before it is dropped, so the loop
/// is running and blocked on nothing but its own poll.
///
/// Asserted as "the same address binds again", which is a direct
/// statement that the descriptor is gone. The alternative — "connecting
/// now fails" — depends on when the kernel gets around to refusing, and
/// that is a wait this test would have to encode as a timeout.
#[test]
fn dropping_the_gateway_releases_its_port() {
    let fixture = FixtureUpstream::answering("HTTP/1.1 200 OK", "", "{}");
    let gateway = gateway_to(&fixture);
    let address = gateway.address();

    let response = read_all(send(
        gateway.address(),
        &messages_request(gateway.token().expose(), "{}"),
    ));
    assert!(response.starts_with("HTTP/1.1 200 OK"), "{response}");

    let started = Instant::now();
    drop(gateway);
    let elapsed = started.elapsed();

    // Generous by two orders of magnitude over `ACCEPT_POLL`, because
    // this is a bound on "does not hang" and not a benchmark. A blocking
    // accept with no stop flag would sit here until the next connection,
    // which in a test is forever.
    assert!(
        elapsed < Duration::from_secs(2),
        "dropping a gateway with a running accept loop took {elapsed:?}"
    );

    // Bounded retry, and it does not weaken the assertion. The gateway
    // binds an *ephemeral* port, so between the drop above and this bind
    // the kernel is free to hand that same port to any other test thread
    // calling `bind(0)` — and this suite has many. That transient loss
    // races as `AddrInUse` and is not this gateway holding anything: two
    // workers hit it independently on 2026-08-26, once captured by name.
    //
    // If the gateway really had failed to release the descriptor, no
    // number of retries would ever succeed, so the loop still fails for
    // the reason the test exists. It only tolerates an unrelated binder
    // holding the port briefly.
    let deadline = Instant::now() + Duration::from_secs(2);
    let mut rebound = TcpListener::bind(address);
    while rebound.is_err() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(20));
        rebound = TcpListener::bind(address);
    }
    assert!(
        rebound.is_ok(),
        "the gateway's port was still held after the gateway was dropped: {:?}",
        rebound.as_ref().err()
    );
}

// --- the rule the module is built to be unable to break ---------------

/// "The gateway is never a coding harness and never owns an interactive
/// session" is a promise until something makes it impossible to break by
/// accident, and this is that something. A module that cannot see the
/// session model cannot own a session, cannot drive a terminal and cannot
/// reach a harness adapter — so the rule survives a contributor who never
/// read the header, which is the only kind of rule worth having here.
///
/// Every file in this directory, not just this one: the ingress is where
/// a "just look up which session this belongs to" would be written.
///
/// `crate::events` is on the list and carries **no exception any more**:
/// `session::gateway_failure` answers in [`super::DegradeReason`], this
/// module's own word, so no file here names a host event vocabulary. The
/// ratchet test that used to guard the exception is deleted with it, which
/// is exactly what its own failure message asked for.
///
/// `glasshouse::` is the one that survives the extraction. The others are
/// `crate::`-rooted and so name modules of *this* crate: they stay because
/// the day somebody adds a `session` or `harness` module here is the day
/// this directory could reach one.
///
/// **Every production file in the crate, not only this directory** — the
/// list is [`crate_sources`], the same one
/// [`the_gateway_names_no_glasshouse_path`] uses. It was `gateway_sources`
/// until `provider/registry.rs` stopped naming `crate::integrations`, which
/// was the last production file outside this directory that a `crate::`
/// rooted rule would have failed. Nothing about the seven `crate::`-rooted
/// needles was ever particular to `gateway/`: a `crate::harness` in
/// `routing/pairing.rs` or `secret/native.rs` re-introduces exactly the
/// coupling the extraction removed, and one list held to one rule is what a
/// reviewer can check.
///
/// Reads [`production_code_to_test_module`] and not [`production_code`],
/// because the latter was reading 4% of `gateway/session.rs`. See that
/// function.
#[test]
fn the_gateway_imports_none_of_the_modules_that_would_make_it_a_harness() {
    for (name, source) in crate_sources() {
        let code = production_code_to_test_module(source);
        for forbidden in [
            "crate::session",
            "crate::shell",
            "crate::tui",
            "crate::harness",
            "crate::profile",
            // User ruling 2026-09-10: harness and integration identity leave
            // the gateway entirely. A candidate is ranked on what the caller
            // states about it (`crate::routing::pairing::RouteAffinity`) and
            // on what this side can measure, never on which client is asking.
            "crate::integrations",
            "crate::events",
            "glasshouse::",
        ] {
            assert!(
                !code.contains(forbidden),
                "{name} names `{forbidden}` in production code: this crate has become \
                 able to see the session model it must never own, and \"the harness stays \
                 the harness\" is back to being a promise rather than something these \
                 files are structurally unable to break"
            );
        }
    }
}

/// Every production source file in this crate, for the crate-wide scan
/// below.
///
/// Listed rather than walked, for [`gateway_sources`]'s reason: `include_str!`
/// needs a literal, and a list somebody has to add to when a file is added is
/// a list a reviewer can see is complete. The only files left out are the
/// ones that are test-only in their entirety — `conformance.rs`,
/// `gateway/fixture.rs`, and `provider/fixture.rs` behind the `fixtures`
/// feature — and the inline test modules, which every entry is cut at by
/// [`production_code_to_test_module`]; a file's test module therefore sits
/// at its end, which clippy's `items_after_test_module` also insists on.
///
/// `provider/fixture.rs` **is** here even though it exists for tests: it is a
/// plain `pub mod`, so it ships in the library, and a rule about what this
/// crate names has to cover what this crate compiles.
fn crate_sources() -> Vec<(&'static str, &'static str)> {
    let mut sources = gateway_sources();
    sources.extend([
        ("lib.rs", include_str!("../lib.rs")),
        ("main.rs", include_str!("../main.rs")),
        ("config.rs", include_str!("../config.rs")),
        ("pool.rs", include_str!("../pool.rs")),
        ("entitlement.rs", include_str!("../entitlement.rs")),
        ("provider/mod.rs", include_str!("../provider/mod.rs")),
        ("provider/budget.rs", include_str!("../provider/budget.rs")),
        ("provider/cache.rs", include_str!("../provider/cache.rs")),
        (
            "provider/discovery.rs",
            include_str!("../provider/discovery/mod.rs"),
        ),
        (
            "provider/fixture.rs",
            include_str!("../provider/fixture.rs"),
        ),
        (
            "provider/quota.rs",
            include_str!("../provider/quota/mod.rs"),
        ),
        (
            "provider/registry.rs",
            include_str!("../provider/registry.rs"),
        ),
        (
            "provider/telemetry.rs",
            include_str!("../provider/telemetry/mod.rs"),
        ),
        ("routing/mod.rs", include_str!("../routing/mod.rs")),
        ("routing/domain.rs", include_str!("../routing/domain.rs")),
        (
            "routing/evidence/mod.rs",
            include_str!("../routing/evidence/mod.rs"),
        ),
        (
            "routing/evidence/vocabulary.rs",
            include_str!("../routing/evidence/vocabulary.rs"),
        ),
        ("routing/free.rs", include_str!("../routing/free.rs")),
        ("routing/pairing.rs", include_str!("../routing/pairing.rs")),
        ("routing/request.rs", include_str!("../routing/request.rs")),
        ("routing/tier.rs", include_str!("../routing/tier.rs")),
        ("routing/wire.rs", include_str!("../routing/wire.rs")),
        ("secret/mod.rs", include_str!("../secret/mod.rs")),
        ("secret/native.rs", include_str!("../secret/native.rs")),
        ("secret/file.rs", include_str!("../secret/file.rs")),
        (
            "subscription/mod.rs",
            include_str!("../subscription/mod.rs"),
        ),
        (
            "subscription/connect.rs",
            include_str!("../subscription/connect.rs"),
        ),
    ]);
    sources
}

/// The rule `lib.rs`'s own header states — **nothing in this crate may name
/// Glasshouse** — made enforceable rather than remembered.
///
/// Two needles, and each is the realistic way the rule would be broken:
///
/// - `glasshouse::` is what a re-introduced dependency reads like at a use
///   site. It needs no module of this crate to exist first, which is what
///   separates it from
///   [`the_gateway_imports_none_of_the_modules_that_would_make_it_a_harness`]'s
///   `crate::`-rooted list — that one fires only once somebody has added a
///   `session` or `harness` module here, this one fires on the import alone.
///   **The obvious "fix" for a boundary a Rust trait cannot cross is to embed
///   the gateway back inside its host**, and that compiles; this is what
///   refuses it.
/// - `rusqlite` is the host's ledger reaching back the other way. The
///   gateway produces routing observations and reports them outward as
///   [`super::Observation::Routed`]; a host stores them. `routing/evidence`
///   is the vocabulary that crossing is written in, and it names no database
///   — its own header says so, and this is what holds it to it. The
///   manifest is scanned beside the sources because a dependency is where
///   this would actually start.
#[test]
fn the_gateway_names_no_glasshouse_path() {
    const FORBIDDEN: [&str; 2] = ["glasshouse::", "rusqlite"];
    for (name, source) in crate_sources() {
        let code = production_code_to_test_module(source);
        for forbidden in FORBIDDEN {
            assert!(
                !code.contains(forbidden),
                "{name} names `{forbidden}` in production code: this crate has stopped \
                 being the bottom of the split. It is the layer a host sits on, not one \
                 that can see a host or a host's store"
            );
        }
    }

    // The dependency itself, which is where a `rusqlite` would arrive before
    // any source file could name it.
    let manifest = include_str!("../../Cargo.toml");
    assert!(
        !manifest.contains("rusqlite"),
        "this crate has taken a dependency on `rusqlite`: the gateway reports observations \
         outward and stores none, and a database here is the ledger the extraction removed"
    );

    // ... and the scan is not vacuous: it runs over the whole crate, and it
    // fires on the change it exists to catch rather than passing because a
    // needle was misspelled.
    assert_eq!(crate_sources().len(), 43);
    let violating =
        production_code_to_test_module("use glasshouse::session::SessionId;\nfn f() {}");
    assert!(FORBIDDEN.iter().any(|needle| violating.contains(needle)));
    let ledger = production_code_to_test_module("use rusqlite::Connection;\nfn f() {}");
    assert!(FORBIDDEN.iter().any(|needle| ledger.contains(needle)));
}

/// An upstream whose only backend is a port nothing listens on, so a real
/// exchange through it ends in `Outcome::Unreachable` — the one outcome
/// `session::gateway_failure` turns into an observation.
fn unreachable_upstream() -> Upstream {
    anthropic_upstream_to("http://127.0.0.1:1")
}

/// What the gateway hands its sink is its **own** vocabulary: a resource
/// slug it minted and one of its own [`super::DegradeReason`]s. Nothing in
/// the payload is a host type, which is the whole reason the extraction can
/// take this directory and leave Glasshouse behind.
///
/// In this crate rather than only in `tests/gateway_degrade.rs` because the
/// production step it watches — `accept_loop` building the `Observation` —
/// is private to this module, and a mutation to it has to be killable from
/// here.
#[test]
fn the_sink_is_handed_the_gateways_own_words_for_what_it_saw() {
    let observed: Arc<Mutex<Vec<Observation>>> = Arc::new(Mutex::new(Vec::new()));
    let sink: ObservationSink = {
        let observed = Arc::clone(&observed);
        Arc::new(move |observation| observed.lock().unwrap().push(observation))
    };
    let gateway =
        Gateway::start_with_degrade_sink(unreachable_upstream(), None, None, Some(sink), None)
            .expect("loopback is bindable");

    let response = read_all(send(
        gateway.address(),
        &messages_request(gateway.token().expose(), "{\"model\":\"probe\"}"),
    ));
    assert!(
        response.starts_with("HTTP/1.1 502"),
        "an unreachable upstream is reported to the harness as a gateway error: {response}"
    );

    // The sink runs on the connection thread *after* the response socket has
    // been closed, so the read above is not proof it has been called yet.
    let deadline = Instant::now() + Duration::from_secs(5);
    while observed.lock().unwrap().is_empty() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(10));
    }

    assert_eq!(
        *observed.lock().unwrap(),
        vec![Observation::Degraded {
            resource: LOCAL_GATEWAY_RESOURCE.to_owned(),
            reason: DegradeReason::Unreachable,
        }],
        "the gateway must report exactly one observation, naming the resource it minted \
         and its own reason for the failure"
    );
}

/// [`super::null_sink`] is a choice, and this is what makes it one: a
/// gateway with no host anywhere serves the harness exactly as a hosted one
/// does, and everything it observes goes nowhere on purpose.
///
/// The standalone case is the extracted crate's normal case, not an edge:
/// there is no Glasshouse to report to, and "nobody is listening" has to be
/// a sink somebody named rather than a `None` nobody noticed.
#[test]
fn a_gateway_with_no_host_serves_the_harness_and_drops_what_it_observes() {
    let gateway = Gateway::start_with_degrade_sink(
        unreachable_upstream(),
        None,
        None,
        Some(null_sink()),
        None,
    )
    .expect("loopback is bindable");

    let response = read_all(send(
        gateway.address(),
        &messages_request(gateway.token().expose(), "{\"model\":\"probe\"}"),
    ));
    assert!(
        response.starts_with("HTTP/1.1 502"),
        "a gateway with no host must answer the harness exactly as a hosted one does: \
         {response}"
    );

    // Nothing to assert about where the observation went, because there is
    // nowhere for it to go — the assertion is that dropping it neither
    // panics the connection thread nor changes what the harness saw. A
    // second exchange proves the first one's dropped observation left the
    // accept loop able to serve.
    let again = read_all(send(
        gateway.address(),
        &messages_request(gateway.token().expose(), "{\"model\":\"probe\"}"),
    ));
    assert!(
        again.starts_with("HTTP/1.1 502"),
        "the gateway must keep serving after dropping an observation: {again}"
    );
}

/// The scan above is only worth having if it can fail — and here, more
/// than anywhere else in this crate, if it does not fire on the prose
/// that explains it: this file's own header names all four forbidden
/// paths in the course of saying it imports none of them. A scan that
/// could not tell those apart would have to be deleted the first time
/// someone wrote the rule down.
#[test]
fn the_gateway_dependency_scan_would_catch_a_violation() {
    let violating = "use crate::session::SessionLifecycle;\nfn start() {}";
    assert!(production_code(violating).contains("crate::session"));
    // ... and does not fire on a doc comment that merely mentions the
    // module, the way this file's own header legitimately does for all
    // four paths.
    let documented = "//! Imports none of `crate::session`.\nfn start() {}";
    assert!(!production_code(documented).contains("crate::session"));
    // ... nor on a mention inside a test.
    let tested = "fn start() {}\n#[cfg(test)]\nmod tests { use crate::session::SessionLifecycle; }";
    assert!(!production_code(tested).contains("crate::session"));
    // ... and the same three readings hold for the host event module, the
    // path added when the gateway got its own `Observation`.
    let host_event = "fn sink() -> crate::events::GatewayFailure { todo!() }";
    assert!(production_code_to_test_module(host_event).contains("crate::events"));
    let host_event_documented = "/// Never names `crate::events`.\nfn sink() {}";
    assert!(!production_code_to_test_module(host_event_documented).contains("crate::events"));
    // ... and the widened slice reads past a test-only `use` that
    // `production_code` stops dead at — the defect that made this scan read
    // 4% of `gateway/session.rs` while reporting it clean.
    let early_test_use = "#[cfg(test)]\nuse super::Tokens;\nfn sink() -> crate::events::GatewayFailure { todo!() }\n#[cfg(test)]\nmod tests;";
    assert!(!production_code(early_test_use).contains("crate::events"));
    assert!(production_code_to_test_module(early_test_use).contains("crate::events"));
    // ... and still stops at the inline test module itself, so a forbidden
    // path a test legitimately names is not a violation.
    let inline_tests =
        "fn sink() {}\n#[cfg(test)]\nmod tests {\n    use crate::events::GatewayFailure;\n}";
    assert!(!production_code_to_test_module(inline_tests).contains("crate::events"));
    // ... and the file list it runs over is not empty, which would make
    // every assertion in it vacuous.
    assert_eq!(gateway_sources().len(), 16);
}

/// No file of the **relay** may deserialize anything. The whole of
/// "preserve tool-call payloads without lossy rewriting" and "keep the
/// first gateway implementation protocol pass-through" rests on nothing
/// here constructing a document from a body. The relay may run bounded
/// streaming observers for approved evidence fields, but a serialization
/// crate reaching these files would quietly turn that into body ownership.
///
/// Phase 56 narrowed this rule and did not repeal it: `translate/` is
/// the one place a body is parsed, entered only from the branch that
/// answered `404`, and it is held apart here on purpose. The second half
/// of this test is what keeps that split honest — the codecs *do*
/// deserialize, so a relay file that started to would be caught by the
/// first half and not excused by the second.
///
/// A scan cannot prove the absence of a hand-rolled parser, and this one
/// does not claim to. What it does catch is the realistic version: the
/// `use serde_json` that a body inspection would be written on top of.
///
/// `usage.rs`, added by the 2026-09-03 ruling, is held to this list like
/// every other relay file and passes it: it scans a sliding window for a
/// table of literal key spellings, which is the shape "bounded streaming or
/// incremental parsing" permits and the shape a deserializer is not. The
/// day someone reaches for `serde_json` to make that reading easier is the
/// day the window becomes a whole response, and this is what fires then.
#[test]
fn no_part_of_the_relay_deserializes_anything() {
    const FORBIDDEN: [&str; 5] = [
        "serde_json",
        "serde::",
        "Deserialize",
        "from_str::<",
        "toml::",
    ];
    for (name, source) in relay_sources() {
        let code = production_code(source);
        for forbidden in FORBIDDEN {
            assert!(
                !code.contains(forbidden),
                "{name} names `{forbidden}` in production code: the relay has started \
                 deserializing a body instead of performing a bounded streaming observation"
            );
        }
    }
    // The exception is real and confined: the codecs deserialize, and
    // nothing outside `translate/` does.
    let codecs_parse = translate_sources()
        .iter()
        .any(|(_, source)| production_code(source).contains("serde_json"));
    assert!(
        codecs_parse,
        "translate/ no longer deserializes anything, so the split above proves nothing"
    );
    assert_eq!(relay_sources().len(), 8);

    // ... and the scan fires on the change it exists to catch, rather
    // than passing because the needle was misspelled.
    let violating = production_code("use serde_json::Value;\nfn peek() {}");
    assert!(FORBIDDEN.iter().any(|needle| violating.contains(needle)));
}

// --- GH-GATEWAY-PURPOSE-HEADER ----------------------------------------------

/// The observations a gateway reported, and the sink it reported them
/// through — everything a purpose-header test needs, because the purpose is
/// something the gateway *says* rather than something it stores.
fn capturing_sink() -> (ObservationSink, Arc<Mutex<Vec<Observation>>>) {
    let seen: Arc<Mutex<Vec<Observation>>> = Arc::new(Mutex::new(Vec::new()));
    let sink: ObservationSink = {
        let seen = Arc::clone(&seen);
        Arc::new(move |observation| seen.lock().unwrap().push(observation))
    };
    (sink, seen)
}

/// A gateway reporting every observation to `sink`, over a fixture upstream
/// — [`gateway_to`] plus the sink wiring `Gateway::start_with_degrade_sink`
/// takes.
fn gateway_to_with_observation_sink(fixture: &FixtureUpstream, sink: ObservationSink) -> Gateway {
    Gateway::start_with_degrade_sink(
        anthropic_upstream_to(&fixture.base_url()),
        None,
        None,
        Some(sink),
        None,
    )
    .expect("loopback is bindable")
}

/// [`messages_request`], with one extra header line before the empty line
/// that ends the head.
fn messages_request_with_header(token: &str, body: &str, header_line: &str) -> Vec<u8> {
    format!(
        "POST /v1/messages?beta=true HTTP/1.1\r\n\
         Host: 127.0.0.1\r\n\
         Authorization: Bearer {token}\r\n\
         Content-Type: application/json\r\n\
         Anthropic-Version: 2023-06-01\r\n\
         {header_line}\r\n\
         Content-Length: {}\r\n\
         \r\n\
         {body}",
        body.len()
    )
    .into_bytes()
}

/// A bound, real, end-to-end exchange's reported routing observation,
/// polled the way `conformance::a_real_forwarded_exchange_is_reported_to_the_sink`
/// does: the report happens on the connection thread after the response is
/// already on the wire.
fn reported_purpose_observation(
    seen: &Arc<Mutex<Vec<Observation>>>,
) -> crate::routing::evidence::NewObservation {
    reported_purpose_observation_for_model(seen, "fixture-model")
}

fn reported_purpose_observation_for_model(
    seen: &Arc<Mutex<Vec<Observation>>>,
    model: &str,
) -> crate::routing::evidence::NewObservation {
    let mut attempts = 0;
    loop {
        let found = seen
            .lock()
            .expect("no test panics while holding this")
            .iter()
            .rev()
            .find_map(|reported| match reported {
                Observation::Routed { observation, .. }
                    if observation.provider == "fixture"
                        && observation.model == model
                        && observation.route.as_deref() == Some("anthropic-messages")
                        && observation.harness.as_deref() == Some("fixture-harness") =>
                {
                    Some(observation.as_ref().clone())
                }
                _ => None,
            });
        if let Some(observation) = found {
            return observation;
        }
        attempts += 1;
        assert!(
            attempts < 200,
            "no routing observation was reported within 2s of a completed, bound exchange"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// A bound gateway reporting to `sink`, so a purpose-header test only has to
/// send the request and read the newest observation back.
fn bound_gateway_with_observation_sink(
    fixture: &FixtureUpstream,
    sink: ObservationSink,
) -> Gateway {
    bound_gateway_with_observation_sink_for_model(fixture, sink, "fixture-model")
}

fn bound_gateway_with_observation_sink_for_model(
    fixture: &FixtureUpstream,
    sink: ObservationSink,
    assigned_model: &str,
) -> Gateway {
    use crate::routing::AssignedModel;

    let gateway = gateway_to_with_observation_sink(fixture, sink);
    gateway.routing().bind(
        "fixture-harness",
        "anthropic-messages",
        AssignedModel::named(assigned_model),
        &gateway
            .upstream()
            .expect("a started gateway has its upstream"),
    );
    gateway
}

/// The helper's model is a fact of the request, independent of the task
/// model that selected the provider. The relay must observe that one bounded
/// field without changing the body or leaking the private purpose header.
#[test]
fn sol_task_and_luna_helper_requests_keep_their_wire_models_and_distinct_purposes() {
    use crate::routing::evidence::{HARNESS_TURN_PURPOSE, HELPER_PURPOSE};

    let (sink, seen) = capturing_sink();
    let fixture = FixtureUpstream::answering("HTTP/1.1 200 OK", "", "{\"ok\":true}");
    let gateway = bound_gateway_with_observation_sink_for_model(&fixture, sink, "gpt-5.6-sol");
    let task_body =
        r#"{"model":"gpt-5.6-sol","max_tokens":64,"messages":[{"role":"user","content":"work"}]}"#;
    let helper_body = r#"{"model":"gpt-5.6-luna","max_tokens":64,"messages":[{"role":"user","content":"inspect"}]}"#;

    let task_response = read_all(send(
        gateway.address(),
        &messages_request(gateway.token().expose(), task_body),
    ));
    assert!(
        task_response.starts_with("HTTP/1.1 200 OK"),
        "{task_response}"
    );

    let helper_response = read_all(send(
        gateway.address(),
        &messages_request_with_header(
            gateway.token().expose(),
            helper_body,
            "X-Glasshouse-Purpose: helper",
        ),
    ));
    assert!(
        helper_response.starts_with("HTTP/1.1 200 OK"),
        "{helper_response}"
    );

    let requests = fixture.requests();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[0].body, task_body.as_bytes(), "task bytes changed");
    assert_eq!(
        requests[1].body,
        helper_body.as_bytes(),
        "helper bytes changed"
    );
    assert!(
        requests
            .iter()
            .all(|request| request.header("x-glasshouse-purpose").is_none()),
        "private purpose header reached the upstream"
    );

    let task_row = reported_purpose_observation_for_model(&seen, "gpt-5.6-sol");
    assert_eq!(task_row.purpose.as_deref(), Some(HARNESS_TURN_PURPOSE));
    let helper_row = reported_purpose_observation_for_model(&seen, "gpt-5.6-luna");
    assert_eq!(helper_row.purpose.as_deref(), Some(HELPER_PURPOSE));
}

/// A `supervisor` purpose header stamps the reported observation and never
/// reaches the upstream — `ask-primary-supervisor-purpose-header.md`, ruled
/// yes.
#[test]
fn a_supervisor_purpose_header_stamps_the_row_and_never_reaches_the_upstream() {
    use crate::routing::evidence::SUPERVISOR_PURPOSE;

    let (sink, seen) = capturing_sink();
    let fixture = FixtureUpstream::answering("HTTP/1.1 200 OK", "", "{\"ok\":true}");
    let gateway = bound_gateway_with_observation_sink(&fixture, sink);

    let response = read_all(send(
        gateway.address(),
        &messages_request_with_header(
            gateway.token().expose(),
            "{}",
            "X-Glasshouse-Purpose: supervisor",
        ),
    ));
    assert!(response.starts_with("HTTP/1.1 200 OK"), "{response}");

    let request = fixture.only_request();
    assert_eq!(
        request.header("x-glasshouse-purpose"),
        None,
        "the purpose header must never reach the upstream"
    );

    let row = reported_purpose_observation(&seen);
    assert_eq!(row.purpose.as_deref(), Some(SUPERVISOR_PURPOSE));
}

/// An unknown purpose value is stripped like a known one, and the reported
/// observation stays `harness-turn`.
#[test]
fn an_unknown_purpose_header_is_stripped_and_the_row_stays_harness_turn() {
    use crate::routing::evidence::HARNESS_TURN_PURPOSE;

    let (sink, seen) = capturing_sink();
    let fixture = FixtureUpstream::answering("HTTP/1.1 200 OK", "", "{\"ok\":true}");
    let gateway = bound_gateway_with_observation_sink(&fixture, sink);

    let response = read_all(send(
        gateway.address(),
        &messages_request_with_header(
            gateway.token().expose(),
            "{}",
            "X-Glasshouse-Purpose: not-a-purpose",
        ),
    ));
    assert!(response.starts_with("HTTP/1.1 200 OK"), "{response}");

    let request = fixture.only_request();
    assert_eq!(
        request.header("x-glasshouse-purpose"),
        None,
        "an unrecognised purpose header must still never reach the upstream"
    );

    let row = reported_purpose_observation(&seen);
    assert_eq!(row.purpose.as_deref(), Some(HARNESS_TURN_PURPOSE));
}

/// A `decision` purpose header stamps the reported observation and never
/// reaches the upstream — the same shape `supervisor` and `helper` already
/// have, for a Sterna request naming what it asked a decision model about the
/// task rather than the task itself.
#[test]
fn a_decision_purpose_header_stamps_the_row_and_never_reaches_the_upstream() {
    use crate::routing::evidence::DECISION_PURPOSE;

    let (sink, seen) = capturing_sink();
    let fixture = FixtureUpstream::answering("HTTP/1.1 200 OK", "", "{\"ok\":true}");
    let gateway = bound_gateway_with_observation_sink(&fixture, sink);

    let response = read_all(send(
        gateway.address(),
        &messages_request_with_header(
            gateway.token().expose(),
            "{}",
            "X-Glasshouse-Purpose: decision",
        ),
    ));
    assert!(response.starts_with("HTTP/1.1 200 OK"), "{response}");

    let request = fixture.only_request();
    assert_eq!(
        request.header("x-glasshouse-purpose"),
        None,
        "the purpose header must never reach the upstream"
    );

    let row = reported_purpose_observation(&seen);
    assert_eq!(row.purpose.as_deref(), Some(DECISION_PURPOSE));
}

/// No header at all is recorded exactly as before this package.
#[test]
fn a_request_without_a_purpose_header_is_recorded_as_a_harness_turn() {
    use crate::routing::evidence::HARNESS_TURN_PURPOSE;

    let (sink, seen) = capturing_sink();
    let fixture = FixtureUpstream::answering("HTTP/1.1 200 OK", "", "{\"ok\":true}");
    let gateway = bound_gateway_with_observation_sink(&fixture, sink);

    let response = read_all(send(
        gateway.address(),
        &messages_request(gateway.token().expose(), "{}"),
    ));
    assert!(response.starts_with("HTTP/1.1 200 OK"), "{response}");

    let row = reported_purpose_observation(&seen);
    assert_eq!(row.purpose.as_deref(), Some(HARNESS_TURN_PURPOSE));
}

/// A deferred slot: the first request rebuilds at once, a refused rebuild
/// stands for the interval, and once the supplier answers the slot is filled
/// for good.
#[test]
fn a_deferred_slot_paces_rebuilds_and_fills_once_its_supplier_answers() {
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    let attempts = Arc::new(AtomicUsize::new(0));
    let ready = Arc::new(AtomicBool::new(false));
    let supplier = {
        let attempts = Arc::clone(&attempts);
        let ready = Arc::clone(&ready);
        move || {
            attempts.fetch_add(1, Ordering::SeqCst);
            if !ready.load(Ordering::SeqCst) {
                return Err("still nothing".to_owned());
            }
            Upstream::new(
                "fixture".to_owned(),
                vec![Route::new(
                    "anthropic-messages".to_owned(),
                    &["/messages"],
                    "http://127.0.0.1:1",
                )],
                Secret::mint_for_test("k"),
                crate::routing::CredentialId::new(
                    "fixture".to_owned(),
                    crate::secret::SecretRef::Environment {
                        var: "K".to_owned(),
                    },
                ),
            )
            .map_err(|error| error.to_string())
        }
    };
    let slot = UpstreamSlot::deferred("nothing at start".to_owned(), Box::new(supplier))
        .with_rebuild_interval(Duration::from_millis(50));

    assert_eq!(slot.current_or_build().unwrap_err(), "still nothing");
    assert_eq!(
        attempts.load(Ordering::SeqCst),
        1,
        "the first request rebuilds at once"
    );
    assert_eq!(slot.current_or_build().unwrap_err(), "still nothing");
    assert_eq!(
        attempts.load(Ordering::SeqCst),
        1,
        "a refusal stands for the interval"
    );
    ready.store(true, Ordering::SeqCst);
    std::thread::sleep(Duration::from_millis(60));
    assert!(slot.current_or_build().is_ok());
    assert!(slot.current().is_some());
    assert!(slot.current_or_build().is_ok());
    assert_eq!(
        attempts.load(Ordering::SeqCst),
        2,
        "filled once, never rebuilt"
    );
}

/// A serving slot told how to reload rebuilds once per change and keeps the
/// running pool when the rebuild fails -- a bad configuration edit never
/// takes a working gateway down.
#[test]
fn a_serving_slot_reloads_once_per_change_and_keeps_its_pool_on_a_failed_rebuild() {
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    let upstream = |name: &str| {
        Upstream::new(
            name.to_owned(),
            vec![Route::new(
                "anthropic-messages".to_owned(),
                &["/messages"],
                "http://127.0.0.1:1",
            )],
            Secret::mint_for_test("k"),
            crate::routing::CredentialId::new(
                name.to_owned(),
                crate::secret::SecretRef::Environment {
                    var: "K".to_owned(),
                },
            ),
        )
        .unwrap()
    };
    let slot = UpstreamSlot::ready(upstream("before"));
    let first = slot.current().unwrap();
    let changed = Arc::new(AtomicBool::new(false));
    let broken = Arc::new(AtomicBool::new(false));
    let builds = Arc::new(AtomicUsize::new(0));
    let _ = slot.reload.set(Reload {
        supplier: Box::new({
            let broken = Arc::clone(&broken);
            let builds = Arc::clone(&builds);
            move || {
                builds.fetch_add(1, Ordering::SeqCst);
                if broken.load(Ordering::SeqCst) {
                    return Err("does not parse".to_owned());
                }
                Ok(upstream("after"))
            }
        }),
        changed: Box::new({
            let changed = Arc::clone(&changed);
            move || changed.swap(false, Ordering::SeqCst)
        }),
    });

    assert!(
        Arc::ptr_eq(&slot.current_or_build().unwrap(), &first),
        "nothing changed"
    );
    assert_eq!(builds.load(Ordering::SeqCst), 0);

    changed.store(true, Ordering::SeqCst);
    let reloaded = slot.current_or_build().unwrap();
    assert!(!Arc::ptr_eq(&reloaded, &first), "a change rebuilds");
    assert!(
        Arc::ptr_eq(&slot.current_or_build().unwrap(), &reloaded),
        "once per change"
    );
    assert_eq!(builds.load(Ordering::SeqCst), 1);

    broken.store(true, Ordering::SeqCst);
    changed.store(true, Ordering::SeqCst);
    assert!(
        Arc::ptr_eq(&slot.current_or_build().unwrap(), &reloaded),
        "a failed rebuild keeps the pool"
    );
}
