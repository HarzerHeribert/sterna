//! The provider-facing tool ABI: familiar spellings over Sterna's own kernel.
//!
//! `docs/tools.md` is the specification. The one invariant
//! this module exists to hold is §1's: **there is no executor here.** A
//! provider tool call is decoded into a canonical intent, lowered into cell
//! source, and handed to the same cell executor that runs a model-authored
//! program — so `Read` called directly and `read` called inside a cell are
//! the same code path, not two implementations that must be kept agreeing.

pub mod dialect;
pub mod intent;
pub mod lift;
pub mod provenance;
pub mod telemetry;
pub mod types;

use serde_json::{Map, Value, json};

pub use dialect::{Dialect, Param, ParamType, Shape, Target};
pub use intent::{AbiError, ErrorKind, Intent, Lowered, LoweredCall, lower};
pub use provenance::{EvidenceClass, Provenance};

/// Which entry points the parent model can see — `tool-abi.md` §3.
///
/// The invariant, and the reason this is a visibility type and not a
/// strategy type: **no variant selects an execution architecture.** Every
/// mode reaches the same kernel, so a benchmark comparing them measures the
/// interface and nothing else.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Interface {
    /// Only `execute_cell` is declared. The familiar bindings still exist
    /// inside the cell. **The default** — the user's decision of
    /// 2026-09-13 on the matched ablation (`smarter-cheaper-roadmap.md`,
    /// *The 2026-09-13 ablation*): cells-only passed 12/12 with the fewest
    /// parent requests; hybrid 11/12 with a third more; tools-only 10/12.
    #[default]
    Cells,
    /// Only the dialect's direct tools are declared.
    Tools,
    /// Both: the measured alternative, kept as an option for callers that
    /// want the familiar tools declared. No quota is forced either way.
    Hybrid,
}

impl Interface {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Cells => "cells",
            Self::Tools => "tools",
            Self::Hybrid => "hybrid",
        }
    }

    /// Parses `--interface=` and the `[interface] mode` config key.
    pub fn parse(value: &str) -> Result<Self, String> {
        match value.trim().to_ascii_lowercase().as_str() {
            "cells" => Ok(Self::Cells),
            "tools" => Ok(Self::Tools),
            "hybrid" => Ok(Self::Hybrid),
            other => Err(format!(
                "unknown interface mode `{other}`; expected cells, tools or hybrid"
            )),
        }
    }

    /// Whether `execute_cell` is declared to the provider.
    #[must_use]
    pub fn declares_cell(self) -> bool {
        matches!(self, Self::Cells | Self::Hybrid)
    }

    /// Whether the dialect's direct tools are declared to the provider.
    #[must_use]
    pub fn declares_direct_tools(self) -> bool {
        matches!(self, Self::Tools | Self::Hybrid)
    }
}

/// Where one execution frame came from — `tool-abi.md` §19 and §20.
///
/// The ledger and the TUI both need this and neither may infer it: a frame
/// sterna lowered from a direct call contains TypeScript no model wrote, and
/// presenting that as authored would misreport what happened.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Origin {
    /// A model-authored `execute_cell` program.
    #[default]
    AuthoredCell,
    /// One or more provider-native direct tool calls, lowered.
    DirectTool,
}

impl Origin {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::AuthoredCell => "authored_cell",
            Self::DirectTool => "direct_tool",
        }
    }
}

/// The mechanical thresholds the deterministic router reads — `tool-abi.md`
/// §17.
///
/// Every field is a size or a count, which is the whole point: §17 forbids
/// routine routing that needs an inference, so nothing here is a judgement.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Router {
    /// Bytes of text returned inline before the result becomes bounded.
    /// Below the runtime's 32,768-character console budget so a direct
    /// result cannot displace a cell's own output.
    pub inline_bytes: usize,
    /// Items of an array returned inline before the result becomes bounded.
    pub inline_items: usize,
}

impl Default for Router {
    fn default() -> Self {
        Self {
            inline_bytes: 16 * 1024,
            inline_items: 50,
        }
    }
}

/// What the model is shown for one capability result, and what that content
/// is relative to the observation — `tool-abi.md` §10's separation of
/// execution from presentation.
///
/// `count` is the mechanical total of the underlying observation, carried
/// separately from the content because `types::PRELUDE`'s `Bounded` promises
/// it on every result: a model that can read `count` and `complete` off the
/// value never has to guess whether iterating covers everything.
#[derive(Debug, Clone, PartialEq)]
pub struct Presentation {
    pub content: Value,
    pub provenance: Provenance,
    pub count: u64,
}

/// The field each capability's items arrive under, matching the declared
/// result type rather than a generic name.
fn items_key(capability: &str) -> &'static str {
    match capability {
        "glob" => "paths",
        _ => "matches",
    }
}

impl Router {
    /// Decides the presentation for one marshalled capability result.
    ///
    /// Deterministic-first (§8): this function asks no model. It returns
    /// `Exact` when the whole observation fits and `BoundedExact` when it
    /// does not, always naming the handle that still holds the whole.
    #[must_use]
    pub fn present(&self, capability: &str, binding: &str, value: &Value) -> Presentation {
        match capability {
            "read" | "context" => self.present_file(binding, value),
            "grep" | "glob" | "rg" | "fd" => self.present_items(capability, binding, value),
            "bash" | "jq" => self.present_process(binding, value),
            _ => Presentation {
                content: value.clone(),
                provenance: Provenance::exact(binding),
                count: 1,
            },
        }
    }

    fn present_file(&self, binding: &str, value: &Value) -> Presentation {
        let text = value
            .get("text")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let bytes = value
            .get("bytes")
            .and_then(Value::as_u64)
            .unwrap_or(text.len() as u64);
        if text.len() <= self.inline_bytes {
            return Presentation {
                content: value.clone(),
                provenance: Provenance::exact(binding),
                count: bytes,
            };
        }
        let cut = floor_char_boundary(text, self.inline_bytes);
        let mut content = Map::new();
        copy_scalars(
            value,
            &mut content,
            &["path", "bytes", "lineCount", "mtime", "sha256"],
        );
        content.insert("text".into(), json!(&text[..cut]));
        Presentation {
            content: Value::Object(content),
            provenance: Provenance::bounded(binding, bytes.max(text.len() as u64)),
            count: bytes.max(text.len() as u64),
        }
    }

    fn present_items(&self, capability: &str, binding: &str, value: &Value) -> Presentation {
        let key = items_key(capability);
        let Some(items) = value.as_array() else {
            return Presentation {
                content: value.clone(),
                provenance: Provenance::exact(binding),
                count: 1,
            };
        };
        let total = items.len() as u64;
        if items.len() <= self.inline_items {
            return Presentation {
                content: json!({key: items}),
                provenance: Provenance::exact(binding),
                count: total,
            };
        }
        let shown: Vec<Value> = items.iter().take(self.inline_items).cloned().collect();
        Presentation {
            content: json!({key: shown}),
            provenance: Provenance::bounded(binding, total),
            count: total,
        }
    }

    fn present_process(&self, binding: &str, value: &Value) -> Presentation {
        let stdout = value
            .get("stdout")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let stderr = value
            .get("stderr")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let total = (stdout.len() + stderr.len()) as u64;
        // `ok` is decided from the exit status, never from reading stdout —
        // the P1 row in `improvement-register.md` that exists because a model
        // treated misleading stdout as success.
        let ok = value
            .get("exit_code")
            .and_then(Value::as_i64)
            .is_some_and(|code| code == 0);
        if total as usize <= self.inline_bytes {
            let mut content = value.clone();
            if let Some(object) = content.as_object_mut() {
                object.insert("ok".into(), json!(ok));
            }
            return Presentation {
                content,
                provenance: Provenance::exact(binding),
                count: total,
            };
        }
        let budget = self.inline_bytes.saturating_sub(stderr.len().min(2048));
        let cut = floor_char_boundary(stdout, budget);
        let mut content = Map::new();
        content.insert("stdout".into(), json!(&stdout[..cut]));
        content.insert(
            "stderr".into(),
            json!(&stderr[..floor_char_boundary(stderr, 2048)]),
        );
        // `reduced` and `reduction_error` are the derived view beside the
        // exact bytes; a bounded presentation keeps them or the reduction
        // was made for nothing.
        copy_scalars(
            value,
            &mut content,
            &["exit_code", "reduced", "reduction_error"],
        );
        content.insert("ok".into(), json!(ok));
        Presentation {
            content: Value::Object(content),
            provenance: Provenance::bounded(binding, total),
            count: total,
        }
    }
}

/// Copies the named keys through unchanged, so a bounded presentation keeps
/// the metadata that lets the model decide whether it needs the whole.
fn copy_scalars(from: &Value, into: &mut Map<String, Value>, keys: &[&str]) {
    for key in keys {
        if let Some(value) = from.get(*key) {
            into.insert((*key).to_string(), value.clone());
        }
    }
}

/// The largest index at or below `at` that does not split a character.
fn floor_char_boundary(text: &str, at: usize) -> usize {
    if at >= text.len() {
        return text.len();
    }
    let mut index = at;
    while index > 0 && !text.is_char_boundary(index) {
        index -= 1;
    }
    index
}

/// The result value for one capability call, in the shape
/// `types::PRELUDE` declares.
///
/// The invariant: **every result carries `count`, `complete` and `source`,
/// and names an `artifact` whenever `complete` is false.** That is the whole
/// of what a model needs to know before iterating a result, and it is read
/// off the value rather than recalled from the prompt.
#[must_use]
pub fn encode_result(presentation: &Presentation) -> Value {
    let mut object = match &presentation.content {
        Value::Object(fields) => fields.clone(),
        other => {
            let mut fields = Map::new();
            fields.insert("value".into(), other.clone());
            fields
        }
    };
    object.insert("count".into(), json!(presentation.count));
    object.insert(
        "complete".into(),
        json!(presentation.provenance.class == EvidenceClass::Exact),
    );
    object.insert(
        "source".into(),
        json!(presentation.provenance.class.as_str()),
    );
    if presentation.provenance.class != EvidenceClass::Exact
        && let Some(handle) = &presentation.provenance.handle
    {
        object.insert("artifact".into(), json!(handle));
    }
    if let Some(reducer) = &presentation.provenance.reducer {
        object.insert("reducer".into(), json!(reducer));
    }
    Value::Object(object)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hybrid_declares_both_entry_points() {
        assert!(Interface::Hybrid.declares_cell());
        assert!(Interface::Hybrid.declares_direct_tools());
    }

    #[test]
    fn the_ablation_modes_hide_one_entry_point_each() {
        assert!(Interface::Cells.declares_cell());
        assert!(!Interface::Cells.declares_direct_tools());
        assert!(Interface::Tools.declares_direct_tools());
        assert!(!Interface::Tools.declares_cell());
    }

    #[test]
    fn cells_is_the_default_mode() {
        // The user's decision of 2026-09-13 on the matched ablation; the
        // behavioural pin is `tests/prompt_interface.rs`.
        assert_eq!(Interface::default(), Interface::Cells);
    }

    #[test]
    fn an_unknown_mode_is_refused_by_name() {
        let error = Interface::parse("turbo").unwrap_err();
        assert!(error.contains("turbo"));
        assert!(error.contains("hybrid"));
    }

    #[test]
    fn a_small_file_is_complete_and_says_so() {
        let value = json!({"path": "a.rs", "text": "fn main() {}", "bytes": 12});
        let encoded = encode_result(&Router::default().present("read", "read_1_1", &value));
        assert_eq!(encoded["complete"], json!(true));
        assert_eq!(encoded["source"], json!("exact"));
        assert_eq!(encoded["text"], json!("fn main() {}"));
        assert!(encoded.get("artifact").is_none());
    }

    #[test]
    fn a_large_file_is_incomplete_and_names_the_artifact() {
        let text = "x".repeat(40 * 1024);
        let value = json!({"path": "big.rs", "text": text, "bytes": 40 * 1024, "lineCount": 1});
        let presentation = Router::default().present("read", "read_1_1", &value);
        assert_eq!(presentation.provenance.class, EvidenceClass::BoundedExact);
        let encoded = encode_result(&presentation);
        assert_eq!(encoded["complete"], json!(false));
        assert_eq!(encoded["artifact"], json!("read_1_1"));
        assert_eq!(encoded["count"], json!(40 * 1024));
        assert_eq!(encoded["text"].as_str().unwrap().len(), 16 * 1024);
    }

    /// The property the whole typed-result design exists for: a partial
    /// match set can never be iterated as though it were whole, because the
    /// value itself says it is not.
    #[test]
    fn a_large_match_set_is_iterable_only_behind_its_own_complete_flag() {
        let items: Vec<Value> = (0..1195).map(|n| json!({"line": n})).collect();
        let encoded =
            encode_result(&Router::default().present("grep", "grep_2_1", &Value::Array(items)));
        assert_eq!(encoded["count"], json!(1195));
        assert_eq!(encoded["complete"], json!(false));
        assert_eq!(encoded["matches"].as_array().unwrap().len(), 50);
        assert_eq!(encoded["artifact"], json!("grep_2_1"));
    }

    #[test]
    fn a_small_match_set_is_complete_under_the_same_field_name() {
        let items: Vec<Value> = (0..3).map(|n| json!({"line": n})).collect();
        let encoded =
            encode_result(&Router::default().present("grep", "grep_2_1", &Value::Array(items)));
        assert_eq!(encoded["complete"], json!(true));
        assert_eq!(encoded["matches"].as_array().unwrap().len(), 3);
    }

    #[test]
    fn a_glob_result_uses_the_field_its_type_declares() {
        let items: Vec<Value> = (0..2).map(|n| json!(format!("src/{n}.rs"))).collect();
        let encoded =
            encode_result(&Router::default().present("glob", "glob_1_1", &Value::Array(items)));
        assert!(encoded.get("paths").is_some());
        assert!(encoded.get("matches").is_none());
    }

    #[test]
    fn ok_is_decided_by_the_exit_status_and_not_by_stdout() {
        let failing = json!({"stdout": "All tests passed!", "stderr": "", "exit_code": 1});
        let encoded = encode_result(&Router::default().present("bash", "bash_1_1", &failing));
        assert_eq!(encoded["ok"], json!(false));

        let passing = json!({"stdout": "error: none", "stderr": "", "exit_code": 0});
        let encoded = encode_result(&Router::default().present("bash", "bash_1_2", &passing));
        assert_eq!(encoded["ok"], json!(true));
    }

    #[test]
    fn a_bounded_process_result_keeps_its_derived_view_beside_the_exact_bytes() {
        let stdout = "x".repeat(40 * 1024);
        let value = json!({"stdout": stdout, "stderr": "", "exit_code": 0,
            "reduced": "21 lines, all passing", "reduction_error": "none"});
        let presentation = Router::default().present("bash", "bash_1_1", &value);
        assert_eq!(presentation.provenance.class, EvidenceClass::BoundedExact);
        let encoded = encode_result(&presentation);
        assert_eq!(encoded["reduced"], json!("21 lines, all passing"));
        assert_eq!(encoded["reduction_error"], json!("none"));
        assert_eq!(encoded["exit_code"], json!(0));
        assert_eq!(encoded["complete"], json!(false));
        assert_eq!(encoded["stdout"].as_str().unwrap().len(), 16 * 1024);
    }

    #[test]
    fn a_check_result_keeps_reuse_visible() {
        let value = json!({"name": "tests", "executed": false, "reused": true, "exit_code": 0});
        let encoded =
            encode_result(&Router::default().present("checks.run", "checks_run_1_1", &value));
        assert_eq!(encoded["reused"], json!(true));
        assert_eq!(encoded["executed"], json!(false));
        assert_eq!(encoded["complete"], json!(true));
    }

    #[test]
    fn a_derived_view_says_derived_and_names_its_reducer() {
        let presentation = Presentation {
            content: json!({"summary": "21 failures"}),
            provenance: Provenance::derived("log_1", "test-log-reducer-v2"),
            count: 21,
        };
        let encoded = encode_result(&presentation);
        assert_eq!(encoded["source"], json!("derived"));
        assert_eq!(encoded["complete"], json!(false));
        assert_eq!(encoded["artifact"], json!("log_1"));
        assert_eq!(encoded["reducer"], json!("test-log-reducer-v2"));
    }

    #[test]
    fn a_bounded_cut_never_splits_a_character() {
        let text = "é".repeat(20 * 1024);
        let value = json!({"path": "u.rs", "text": text, "bytes": 40 * 1024});
        let presentation = Router::default().present("read", "read_1_1", &value);
        let shown = presentation.content["text"].as_str().unwrap();
        assert!(shown.len() <= 16 * 1024);
        assert!(shown.chars().all(|c| c == 'é'));
    }

    #[test]
    fn origins_are_distinguishable_for_the_ledger() {
        assert_eq!(Origin::default().as_str(), "authored_cell");
        assert_eq!(Origin::DirectTool.as_str(), "direct_tool");
    }
}
