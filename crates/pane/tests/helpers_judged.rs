//! Acceptance for map 2644 (rank a Scout's candidates before it reads) and
//! 2645 (judge a helper's own result once it returns) --
//! `docs/product/pane/decision-model.md`, `docs/product/pane/little-helpers.md`.
//!
//! **No test here reaches a real provider.** Every test points
//! `ANTHROPIC_BASE_URL` at a loopback fake this file owns, dispatching on
//! the request path exactly as `tests/decisions.rs` does for the same
//! reason -- `/v1/messages` answers one scripted helper reply,
//! `/v1/systemone` answers every question in one request by its own key, a
//! numeric index for a ranking request or `"judge"` for a check.

use pane::contract::SessionId;
use pane::helper_context::{HelperRole, prepare};
use pane::helpers::{
    HelperCall, HelperContext, HelperJudge, HelperRoute, REDUCER, ScoutCandidate, ScoutRankRoute,
    rank_scout_candidates, run_judged, scout_candidates_from_evidence,
};
use pane::sandbox::profile::Profile;
use pane::tools::invoke::CancellationToken;
use pane::wire::Effort;
use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

#[path = "support/sse.rs"]
mod sse;

/// `ANTHROPIC_BASE_URL` is process-global, so every test that sets it is
/// serialised against the others in this file, exactly as `tests/helpers.rs`
/// serialises its own.
static ENV_LOCK: Mutex<()> = Mutex::new(());
static COUNTER: AtomicU64 = AtomicU64::new(0);

struct Fixture {
    root: PathBuf,
}

impl Fixture {
    fn new(label: &str) -> Self {
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!(
            "pane-helpers-judged-{label}-{}-{n}",
            std::process::id()
        ));
        std::fs::create_dir_all(root.join(".claude")).unwrap();
        Self { root }
    }

    fn profile(&self) -> Profile {
        Profile::compile(&self.root, Some(r#"{"permissions":{"allow":[]}}"#))
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

/// One `/v1/systemone` request's every question, answered as a `noul` from
/// `answers` by the question's own key (a missing key gets `0.5`), or --
/// when `delay` is set -- held for that long first, to exercise the 2 s
/// decision timeout.
struct SystemOne {
    answers: BTreeMap<String, f64>,
    delay: Option<Duration>,
}

/// A loopback fake dispatching on the request path: `/v1/messages` answers
/// every request with one assistant text message (`message_reply`), and
/// `/v1/systemone` answers via `script`. Counts both kinds of request so a
/// test can assert how many of each were made.
fn fake(message_reply: &str, script: SystemOne) -> (String, Arc<AtomicUsize>, Arc<AtomicUsize>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let message_hits = Arc::new(AtomicUsize::new(0));
    let systemone_hits = Arc::new(AtomicUsize::new(0));
    let seen_messages = message_hits.clone();
    let seen_systemone = systemone_hits.clone();
    let reply = message_reply.to_string();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { return };
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut request_line = String::new();
            if reader.read_line(&mut request_line).unwrap_or(0) == 0 {
                return;
            }
            let path = request_line
                .split_whitespace()
                .nth(1)
                .unwrap_or_default()
                .to_string();
            let mut length = 0usize;
            loop {
                let mut line = String::new();
                if reader.read_line(&mut line).unwrap_or(0) == 0 {
                    return;
                }
                if line == "\r\n" || line == "\n" {
                    break;
                }
                if let Some(value) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                    length = value.trim().parse().unwrap_or(0);
                }
            }
            let mut body = vec![0; length];
            if reader.read_exact(&mut body).is_err() {
                return;
            }

            if path == "/v1/systemone" {
                seen_systemone.fetch_add(1, Ordering::SeqCst);
                if let Some(delay) = script.delay {
                    std::thread::sleep(delay);
                }
                let request: serde_json::Value = serde_json::from_slice(&body).unwrap();
                let keys: Vec<String> = request["questions"]
                    .as_object()
                    .map(|map| map.keys().cloned().collect())
                    .unwrap_or_default();
                let answers: serde_json::Map<String, serde_json::Value> = keys
                    .into_iter()
                    .map(|key| {
                        let noul = script.answers.get(&key).copied().unwrap_or(0.5);
                        (key, serde_json::json!({"type": "noul", "noul": noul}))
                    })
                    .collect();
                let payload = serde_json::json!({
                    "model": "jev-latest",
                    "answers": answers,
                    "usage": {"input_tokens": 20, "output_tokens": 5},
                })
                .to_string();
                let _ = write!(
                    stream,
                    "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{payload}",
                    payload.len()
                );
            } else {
                seen_messages.fetch_add(1, Ordering::SeqCst);
                let request: serde_json::Value = serde_json::from_slice(&body).unwrap();
                let whole = serde_json::json!({
                    "role": "assistant",
                    "content": [{"type": "text", "text": reply}],
                    "usage": {"input_tokens": 10, "output_tokens": 5},
                })
                .to_string();
                let (content_type, payload) = sse::response_for(&request, &whole);
                let _ = write!(
                    stream,
                    "HTTP/1.1 200 OK\r\ncontent-type: {content_type}\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{payload}",
                    payload.len()
                );
            }
        }
    });
    (url, message_hits, systemone_hits)
}

fn candidate(path: &str, head: &str) -> ScoutCandidate {
    ScoutCandidate {
        path: path.to_string(),
        head: head.to_string(),
    }
}

// ---------------------------------------------------------------------
// 2644 -- ranking a Scout's candidates
// ---------------------------------------------------------------------

/// The higher-scored candidate is read first, and the one below the floor
/// is skipped. Also kills the ranking-sort-direction mutation: flipping the
/// sort to ascending puts `b.rs` first instead of `a.rs`.
#[test]
fn the_scout_reads_the_higher_ranked_file_first_and_skips_the_one_below_the_floor() {
    let _guard = ENV_LOCK.lock().unwrap();
    let (url, _messages, systemone) = fake(
        "unused",
        SystemOne {
            answers: BTreeMap::from([
                ("0".to_string(), 0.20),
                ("1".to_string(), 0.94),
                ("2".to_string(), 0.05),
            ]),
            delay: None,
        },
    );
    unsafe {
        std::env::set_var("ANTHROPIC_BASE_URL", &url);
    }
    let candidates = vec![
        candidate("a.rs", "fn a() {}"),
        candidate("b.rs", "fn b() {}"),
        candidate("below_floor.rs", "fn c() {}"),
    ];
    let ranking = rank_scout_candidates(
        "find the timeout",
        &candidates,
        ScoutRankRoute {
            model: "jev-latest",
            floor: 0.10,
            apply: true,
        },
    )
    .expect("a scripted answer for every candidate ranks");
    unsafe {
        std::env::remove_var("ANTHROPIC_BASE_URL");
    }

    assert_eq!(ranking.ranked, 3, "{ranking:?}");
    assert_eq!(ranking.skipped, 1, "{ranking:?}");
    assert_eq!(
        ranking
            .kept
            .iter()
            .map(|(path, _)| path.as_str())
            .collect::<Vec<_>>(),
        vec!["b.rs", "a.rs"],
        "the higher-scored file (b.rs, 0.94) is read before the lower one (a.rs, 0.20), \
         and below_floor.rs (0.05) never appears: {ranking:?}"
    );
    assert_eq!(
        systemone.load(Ordering::SeqCst),
        1,
        "one request per ranking, however many files"
    );
}

/// Twenty-five files clear the floor and the brief names twenty-four, so the
/// twenty-fifth is counted rather than vanishing. A Scout shown a list with
/// no remainder reads it as the whole field and stops looking.
#[test]
fn files_past_the_briefs_limit_are_counted_not_dropped() {
    let _guard = ENV_LOCK.lock().unwrap();
    let answers: BTreeMap<String, f64> = (0..25).map(|i| (i.to_string(), 0.90)).collect();
    let (url, _messages, _systemone) = fake(
        "unused",
        SystemOne {
            answers,
            delay: None,
        },
    );
    unsafe {
        std::env::set_var("ANTHROPIC_BASE_URL", &url);
    }
    let candidates: Vec<ScoutCandidate> = (0..25)
        .map(|i| candidate(&format!("file{i}.rs"), "fn f() {}"))
        .collect();
    let ranking = rank_scout_candidates(
        "find the timeout",
        &candidates,
        ScoutRankRoute {
            model: "jev-latest",
            floor: 0.10,
            apply: true,
        },
    )
    .expect("a scripted answer for every candidate ranks");
    unsafe {
        std::env::remove_var("ANTHROPIC_BASE_URL");
    }

    assert_eq!(ranking.ranked, 25, "{ranking:?}");
    assert_eq!(ranking.kept.len(), 24, "the brief names at most 24");
    assert_eq!(
        ranking.past_cap, 1,
        "the file the cap cut is counted, not silently dropped: {ranking:?}"
    );
    assert!(
        ranking.note().contains("1 past the brief's limit"),
        "the record says what the bound cut: {}",
        ranking.note()
    );
}

/// A timed-out decision leaves today's order: `rank_scout_candidates`
/// answers `None` rather than a partial or default ranking.
#[test]
fn a_timeout_leaves_todays_order() {
    let _guard = ENV_LOCK.lock().unwrap();
    let (url, _messages, _systemone) = fake(
        "unused",
        SystemOne {
            answers: BTreeMap::new(),
            delay: Some(pane::decide::DECISION_TIMEOUT + Duration::from_secs(1)),
        },
    );
    unsafe {
        std::env::set_var("ANTHROPIC_BASE_URL", &url);
    }
    let candidates = vec![candidate("a.rs", "fn a() {}")];
    let ranking = rank_scout_candidates(
        "find the timeout",
        &candidates,
        ScoutRankRoute {
            model: "jev-latest",
            floor: 0.10,
            apply: true,
        },
    );
    unsafe {
        std::env::remove_var("ANTHROPIC_BASE_URL");
    }
    assert_eq!(ranking, None, "a timed-out decision ranks nothing");
}

/// The pool `rank_scout_candidates` ranks is `helper_context::prepare`'s own
/// term-matched walk, never a separate directory walk: the file that never
/// matched a task term is never offered to the ranking question at all,
/// whatever its place in the walk's own directory order (`aaa_*` sorts
/// first; it is the one left out).
#[test]
fn the_ranked_pool_is_prepare_scouts_term_matched_walk() {
    let _guard = ENV_LOCK.lock().unwrap();
    let fixture = Fixture::new("term-matched-pool");
    std::fs::write(
        fixture.root.join("aaa_unrelated.rs"),
        "fn totally_unrelated() {}\n",
    )
    .unwrap();
    std::fs::write(
        fixture.root.join("zzz_timeout_handler.rs"),
        "fn handle_timeout() { /* the timeout retry lives here */ }\n",
    )
    .unwrap();
    let profile = fixture.profile();
    let token = CancellationToken::new();
    let prepared = prepare(
        HelperRole::Scout,
        "find the timeout retry logic",
        &profile,
        &token,
    );
    let candidates = scout_candidates_from_evidence(&prepared.evidence, &profile, 40);
    assert_eq!(
        candidates
            .iter()
            .map(|c| c.path.as_str())
            .collect::<Vec<_>>(),
        vec!["zzz_timeout_handler.rs"],
        "only the file the walk actually matched a task term in is a candidate, \
         and the untouched file never appears despite sorting first: {candidates:?}"
    );

    let (url, _messages, systemone) = fake(
        "unused",
        SystemOne {
            answers: BTreeMap::from([("0".to_string(), 0.94)]),
            delay: None,
        },
    );
    unsafe {
        std::env::set_var("ANTHROPIC_BASE_URL", &url);
    }
    let ranking = rank_scout_candidates(
        "find the timeout retry logic",
        &candidates,
        ScoutRankRoute {
            model: "jev-latest",
            floor: 0.10,
            apply: true,
        },
    )
    .expect("a scripted answer for the one candidate ranks");
    unsafe {
        std::env::remove_var("ANTHROPIC_BASE_URL");
    }
    assert_eq!(ranking.ranked, 1, "{ranking:?}");
    assert_eq!(
        ranking.kept,
        vec![("zzz_timeout_handler.rs".to_string(), 0.94)],
        "{ranking:?}"
    );
    assert_eq!(systemone.load(Ordering::SeqCst), 1);
}

// ---------------------------------------------------------------------
// 2645 -- judging a helper's own result
// ---------------------------------------------------------------------

/// [`REDUCER`] is toolless and one-shot, so `run_judged` reaches it through
/// exactly one `/v1/messages` call -- the simplest vehicle for testing the
/// judge question without a live agent loop.
fn call_reducer(fixture: &Fixture, judge: Option<HelperJudge<'_>>) -> HelperCall {
    let profile = fixture.profile();
    let session_id = SessionId::new("helpers-judged");
    let token = CancellationToken::new();
    let context = HelperContext {
        profile: &profile,
        session: &session_id,
        token: &token,
    };
    run_judged(
        &REDUCER,
        HelperRoute {
            model: "helper-model",
            effort: Effort::Low,
            cap: None,
        },
        "a build log",
        context,
        judge,
    )
    .0
}

/// A confident no (0.05, at or below the 0.10 floor) carries the line and
/// the helper's own answer is still delivered in full.
#[test]
fn a_confident_no_carries_the_line_and_still_delivers_the_result() {
    let _guard = ENV_LOCK.lock().unwrap();
    let fixture = Fixture::new("confident-no");
    let (url, _messages, systemone) = fake(
        "three failures found",
        SystemOne {
            answers: BTreeMap::from([("judge".to_string(), 0.05)]),
            delay: None,
        },
    );
    unsafe {
        std::env::set_var("ANTHROPIC_BASE_URL", &url);
    }
    let call = call_reducer(
        &fixture,
        Some(HelperJudge {
            model: "jev-latest",
            floor: 0.10,
            apply: true,
        }),
    );
    unsafe {
        std::env::remove_var("ANTHROPIC_BASE_URL");
    }

    assert!(call.outcome.ok, "{call:?}");
    assert!(
        call.outcome.text.contains("three failures found"),
        "the helper's own answer is never withheld: {}",
        call.outcome.text
    );
    assert!(
        call.outcome
            .text
            .contains("decision: this result may not answer what was asked (0.05)"),
        "{}",
        call.outcome.text
    );
    assert_eq!(systemone.load(Ordering::SeqCst), 1);
}

/// A confident yes (0.9) carries nothing. Also kills the
/// `helper_no_below`-comparison mutation: widening the floor to `<= 1.0`
/// would flag this case too.
#[test]
fn a_confident_yes_carries_nothing() {
    let _guard = ENV_LOCK.lock().unwrap();
    let fixture = Fixture::new("confident-yes");
    let (url, _messages, _systemone) = fake(
        "three failures found",
        SystemOne {
            answers: BTreeMap::from([("judge".to_string(), 0.9)]),
            delay: None,
        },
    );
    unsafe {
        std::env::set_var("ANTHROPIC_BASE_URL", &url);
    }
    let call = call_reducer(
        &fixture,
        Some(HelperJudge {
            model: "jev-latest",
            floor: 0.10,
            apply: true,
        }),
    );
    unsafe {
        std::env::remove_var("ANTHROPIC_BASE_URL");
    }

    assert!(call.outcome.ok, "{call:?}");
    assert_eq!(
        call.outcome.text, "three failures found",
        "{}",
        call.outcome.text
    );
    assert!(
        !call.outcome.text.contains("decision:"),
        "{}",
        call.outcome.text
    );
}

/// `shadow`: the question is asked and would-be-flagged, but the record is
/// byte-identical to a call with no judge at all.
#[test]
fn shadow_records_and_shows_nothing() {
    let _guard = ENV_LOCK.lock().unwrap();
    let fixture = Fixture::new("shadow");
    let (url, _messages, systemone) = fake(
        "three failures found",
        SystemOne {
            answers: BTreeMap::from([("judge".to_string(), 0.05)]),
            delay: None,
        },
    );
    unsafe {
        std::env::set_var("ANTHROPIC_BASE_URL", &url);
    }
    let call = call_reducer(
        &fixture,
        Some(HelperJudge {
            model: "jev-latest",
            floor: 0.10,
            apply: false,
        }),
    );
    unsafe {
        std::env::remove_var("ANTHROPIC_BASE_URL");
    }

    assert!(call.outcome.ok, "{call:?}");
    assert_eq!(
        call.outcome.text, "three failures found",
        "shadow changes nothing about what the caller sees: {}",
        call.outcome.text
    );
    assert_eq!(
        systemone.load(Ordering::SeqCst),
        1,
        "shadow still asks and records"
    );
}

/// `judge: None` is byte-identical to [`pane::helpers::run`] -- the escape
/// hatch every other decision in this project shares (no model, `mode =
/// off`, or a caller that chooses not to ask).
#[test]
fn no_judge_never_touches_the_result() {
    let fixture = Fixture::new("no-judge");
    let (url, _messages, systemone) = fake(
        "three failures found",
        SystemOne {
            answers: BTreeMap::new(),
            delay: None,
        },
    );
    let _guard = ENV_LOCK.lock().unwrap();
    unsafe {
        std::env::set_var("ANTHROPIC_BASE_URL", &url);
    }
    let call = call_reducer(&fixture, None);
    unsafe {
        std::env::remove_var("ANTHROPIC_BASE_URL");
    }

    assert_eq!(call.outcome.text, "three failures found");
    assert_eq!(
        systemone.load(Ordering::SeqCst),
        0,
        "no judge, no question asked"
    );
}
