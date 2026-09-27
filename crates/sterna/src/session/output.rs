//! Versioned machine output comes from typed session records, never the TUI.
use std::cell::RefCell;
use std::collections::BTreeMap;
use std::io::Write;
use std::time::Instant;

use crate::abi::lift::Family;
use crate::abi::telemetry::{self as taxonomy, FailureKind, RequestCause};
use crate::abi::{Dialect, Interface, Origin};
use crate::contract::{Block, Message, Role};
use crate::helpers::HelperRecord;
use crate::runtime::observation::{ObservationStats, ReductionStats};
use crate::runtime::outcome::{CellOutcomeKind, CellRecord, Ended};
use crate::telemetry::RequestMeasurement;
use serde_json::{Value, json};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, clap::ValueEnum)]
pub enum Format {
    #[default]
    Text,
    Json,
    StreamJson,
}

struct State {
    format: Format,
    sequence: u64,
    session_id: Option<String>,
    events: Vec<Value>,
    answer: Option<String>,
    delivery_error: Option<String>,
    telemetry: Telemetry,
}

#[derive(Default)]
struct Usage {
    requests: u64,
    responses: u64,
    reported_requests: u64,
    input_tokens: u64,
    output_tokens: u64,
    cache_read_input_tokens: u64,
    cache_creation_input_tokens: u64,
    input_reported_requests: u64,
    output_reported_requests: u64,
    cache_read_reported_requests: u64,
    cache_creation_reported_requests: u64,
}

#[derive(Default)]
struct ModelUsage {
    model: String,
    calls: u64,
    usage_known_calls: u64,
    failed_calls: u64,
    usage: Usage,
}

/// Failures counted by [`FailureKind`], indexed in `FailureKind::ALL` order.
#[derive(Default)]
struct KindCounts([u64; 8]);

impl KindCounts {
    fn add(&mut self, kind: FailureKind) {
        let index = FailureKind::ALL
            .iter()
            .position(|candidate| *candidate == kind)
            .expect("every kind is in ALL");
        self.0[index] = self.0[index].saturating_add(1);
    }

    fn value(&self) -> Value {
        Value::Object(
            FailureKind::ALL
                .iter()
                .zip(self.0)
                .map(|(kind, count)| (kind.as_str().to_string(), json!(count)))
                .collect(),
        )
    }
}

/// One origin's frames, operations and failures.
#[derive(Default)]
struct OriginStats {
    executed: u64,
    failed: u64,
    operations: u64,
    failures: KindCounts,
}

const ORIGINS: [Origin; 3] = [
    Origin::AuthoredCell,
    Origin::DirectTool,
    Origin::LittleHelper,
];

const FAMILIES: [Family; 5] = [
    Family::Search,
    Family::Read,
    Family::List,
    Family::RepositoryState,
    Family::Verification,
];

/// Parent usage attributed to one [`RequestCause`].
#[derive(Default)]
struct CauseUsage {
    requests: u64,
    input_tokens: u64,
    output_tokens: u64,
    cache_read_input_tokens: u64,
    cache_creation_input_tokens: u64,
    wall_time_ms: u64,
}

impl CauseUsage {
    fn value(&self) -> Value {
        json!({
            "requests": self.requests,
            "input_tokens": self.input_tokens,
            "output_tokens": self.output_tokens,
            "cache_read_input_tokens": self.cache_read_input_tokens,
            "cache_creation_input_tokens": self.cache_creation_input_tokens,
            "known_tokens": self.input_tokens
                .saturating_add(self.output_tokens)
                .saturating_add(self.cache_read_input_tokens)
                .saturating_add(self.cache_creation_input_tokens),
            "wall_time_ms": self.wall_time_ms,
        })
    }
}

/// The observation-delta figures summed over every frame.
#[derive(Default)]
struct ObservationTotals {
    rows_rendered: u64,
    rows_suppressed: u64,
    bytes_rendered: u64,
    bytes_suppressed: u64,
    full_inventories: u64,
    repeated_observations: u64,
}

struct Telemetry {
    started: Instant,
    parent: Usage,
    parent_models: Vec<ModelUsage>,
    helpers: Usage,
    helper_calls: u64,
    helper_usage_known_calls: u64,
    helper_failures: u64,
    helper_models: Vec<ModelUsage>,
    preflight_helpers: Vec<Value>,
    cells: u64,
    cell_failures: u64,
    tool_calls: u64,
    tool_failures: u64,
    interface: Option<(Interface, Dialect)>,
    execute_cell_calls: u64,
    direct_tool_calls: u64,
    direct_tools_by_name: BTreeMap<String, u64>,
    /// Indexed in [`ORIGINS`] order.
    by_origin: [OriginStats; 3],
    single_intent_cells: u64,
    failures: KindCounts,
    shell_shaped: u64,
    lifted: u64,
    /// Indexed in [`FAMILIES`] order.
    by_family: [u64; 5],
    observation: ObservationTotals,
    reductions: ReductionStats,
    /// Indexed in `RequestCause::ALL` order.
    recovery: [CauseUsage; 4],
    current_cause: RequestCause,
    completion: Option<Value>,
    /// What the gate noted without holding, and what the checker behind
    /// the answer said (`after.rs`).
    after_answer: Option<Value>,
    no_progress_notices: u64,
    stall_notices: u64,
    acceptance: Option<Value>,
    capsule: Option<Value>,
    decisions: Option<Value>,
    /// Every candidate a Scout ranking answered (2644), across the whole
    /// session -- counted here rather than on `TaskState` because the
    /// ranking runs in `preflight_block`, before a task exists to carry it.
    helpers_ranked: u32,
    /// Candidates a Scout ranking answered below the floor (2644).
    helpers_skipped: u32,
    /// Judged helper returns, from either call site the judge reaches: the
    /// preflight Scout's own result and the completion gate's fresh
    /// checker (2645).
    helpers_checked: u32,
    /// Judged returns whose `noul` crossed the floor -- counted the same in
    /// `shadow` and `on`, since `shadow` asks and counts every question
    /// this project's decisions make.
    helpers_flagged: u32,
    /// Total latency of every ranking and judge request folded into the
    /// four counters above.
    helpers_latency_ms: u64,
}

impl Default for Telemetry {
    fn default() -> Self {
        Self {
            started: Instant::now(),
            parent: Usage::default(),
            parent_models: Vec::new(),
            helpers: Usage::default(),
            helper_calls: 0,
            helper_usage_known_calls: 0,
            helper_failures: 0,
            helper_models: Vec::new(),
            preflight_helpers: Vec::new(),
            cells: 0,
            cell_failures: 0,
            tool_calls: 0,
            tool_failures: 0,
            interface: None,
            execute_cell_calls: 0,
            direct_tool_calls: 0,
            direct_tools_by_name: BTreeMap::new(),
            by_origin: Default::default(),
            single_intent_cells: 0,
            failures: KindCounts::default(),
            shell_shaped: 0,
            lifted: 0,
            by_family: [0; 5],
            observation: ObservationTotals::default(),
            reductions: ReductionStats::default(),
            recovery: Default::default(),
            current_cause: RequestCause::Implementation,
            completion: None,
            after_answer: None,
            no_progress_notices: 0,
            stall_notices: 0,
            acceptance: None,
            capsule: None,
            decisions: None,
            helpers_ranked: 0,
            helpers_skipped: 0,
            helpers_checked: 0,
            helpers_flagged: 0,
            helpers_latency_ms: 0,
        }
    }
}

fn origin_index(origin: Origin) -> usize {
    ORIGINS
        .iter()
        .position(|candidate| *candidate == origin)
        .expect("every origin is in ORIGINS")
}

fn cause_index(cause: RequestCause) -> usize {
    RequestCause::ALL
        .iter()
        .position(|candidate| *candidate == cause)
        .expect("every cause is in ALL")
}

fn mean(total: u64, count: u64) -> f64 {
    if count == 0 {
        0.0
    } else {
        total as f64 / count as f64
    }
}

thread_local! { static STATE: RefCell<Option<State>> = const { RefCell::new(None) }; }

pub(super) struct Output;
impl Output {
    pub(super) fn start(format: Format) -> Self {
        if format != Format::Text {
            STATE.with(|state| {
                *state.borrow_mut() = Some(State {
                    format,
                    sequence: 0,
                    session_id: None,
                    events: Vec::new(),
                    answer: None,
                    delivery_error: None,
                    telemetry: Telemetry::default(),
                })
            });
        }
        Self
    }

    pub(super) fn finish(self, outcome: &Result<(), String>) -> Result<(), String> {
        STATE.with(|state| {
            if let Some(mut state) = state.borrow_mut().take() {
                if let Some(error) = state.delivery_error.take() {
                    return Err(error);
                }
                let mut result = json!({
                    "schema_version": 1, "type": "result", "sequence": state.sequence,
                    "session_id": state.session_id, "success": outcome.is_ok(),
                    "answer": state.answer, "error": outcome.as_ref().err(),
                    // Additive fields do not change the v1 event contract.
                    "telemetry": telemetry_value(&state.telemetry),
                });
                if state.format == Format::Json {
                    result["events"] = Value::Array(std::mem::take(&mut state.events));
                }
                print_json(&result)?;
            }
            Ok(())
        })
    }
}

fn known_tokens(usage: &Usage) -> u64 {
    usage
        .input_tokens
        .saturating_add(usage.output_tokens)
        .saturating_add(usage.cache_read_input_tokens)
        .saturating_add(usage.cache_creation_input_tokens)
}

fn usage_value(usage: &Usage) -> Value {
    json!({
        "known_tokens": known_tokens(usage),
        "requests": usage.requests,
        "successful_responses": usage.responses,
        "requests_without_successful_response": usage.requests.saturating_sub(usage.responses),
        "reported_requests": usage.reported_requests,
        "coverage_complete": usage.reported_requests == usage.requests
            && usage.cache_read_reported_requests == usage.reported_requests
            && usage.cache_creation_reported_requests == usage.reported_requests,
        "input_tokens": usage.input_tokens,
        "output_tokens": usage.output_tokens,
        "cache_read_input_tokens": usage.cache_read_input_tokens,
        "cache_creation_input_tokens": usage.cache_creation_input_tokens,
        "input_reported_requests": usage.input_reported_requests,
        "output_reported_requests": usage.output_reported_requests,
        "cache_read_reported_requests": usage.cache_read_reported_requests,
        "cache_creation_reported_requests": usage.cache_creation_reported_requests,
    })
}

fn usage_model_value(model: &ModelUsage) -> Value {
    let mut value = usage_value(&model.usage);
    value["model"] = json!(model.model);
    value
}

fn helper_model_value(model: &ModelUsage) -> Value {
    let mut value = usage_model_value(model);
    value["calls"] = json!(model.calls);
    value["usage_known_calls"] = json!(model.usage_known_calls);
    value["request_count_coverage_complete"] = json!(model.usage_known_calls == model.calls);
    value["failed_calls"] = json!(model.failed_calls);
    value
}

fn telemetry_value(telemetry: &Telemetry) -> Value {
    let provider_requests = telemetry
        .parent
        .requests
        .saturating_add(telemetry.helpers.requests);
    let reported_requests = telemetry
        .parent
        .reported_requests
        .saturating_add(telemetry.helpers.reported_requests);
    let responses = telemetry
        .parent
        .responses
        .saturating_add(telemetry.helpers.responses);
    let mut parent = usage_value(&telemetry.parent);
    parent["models"] = Value::Array(
        telemetry
            .parent_models
            .iter()
            .map(usage_model_value)
            .collect(),
    );
    let mut helpers = usage_value(&telemetry.helpers);
    helpers["calls"] = json!(telemetry.helper_calls);
    helpers["usage_known_calls"] = json!(telemetry.helper_usage_known_calls);
    helpers["request_count_coverage_complete"] =
        json!(telemetry.helper_usage_known_calls == telemetry.helper_calls);
    helpers["coverage_complete"] = json!(
        telemetry.helper_usage_known_calls == telemetry.helper_calls
            && telemetry.helpers.reported_requests == telemetry.helpers.requests
            && telemetry.helpers.cache_read_reported_requests
                == telemetry.helpers.reported_requests
            && telemetry.helpers.cache_creation_reported_requests
                == telemetry.helpers.reported_requests
    );
    helpers["failed_calls"] = json!(telemetry.helper_failures);
    helpers["models"] = Value::Array(
        telemetry
            .helper_models
            .iter()
            .map(helper_model_value)
            .collect(),
    );
    json!({
        // Time to the answer: the wait for the work behind it is its own
        // figure (`after_answer.wait_ms`), not the task's.
        "wall_time_ms": (telemetry.started.elapsed().as_millis().min(u128::from(u64::MAX)) as u64)
            .saturating_sub(telemetry.after_answer.as_ref().and_then(|a| a["wait_ms"].as_u64()).unwrap_or(0)),
        "cells": {"executed": telemetry.cells, "failed": telemetry.cell_failures},
        "tools": {"calls": telemetry.tool_calls, "failures": telemetry.tool_failures},
        "provider_requests": {
            "total": provider_requests,
            "successful_responses": responses,
            "requests_without_successful_response": provider_requests.saturating_sub(responses),
            "reported": reported_requests,
            "coverage_complete": reported_requests == provider_requests
                && telemetry.helper_usage_known_calls == telemetry.helper_calls
                && telemetry.parent.cache_read_reported_requests == telemetry.parent.reported_requests
                && telemetry.parent.cache_creation_reported_requests == telemetry.parent.reported_requests
                && telemetry.helpers.cache_read_reported_requests == telemetry.helpers.reported_requests
                && telemetry.helpers.cache_creation_reported_requests == telemetry.helpers.reported_requests,
        },
        "tokens": {
            "known_total": known_tokens(&telemetry.parent).saturating_add(known_tokens(&telemetry.helpers)),
            "input_tokens": telemetry.parent.input_tokens.saturating_add(telemetry.helpers.input_tokens),
            "output_tokens": telemetry.parent.output_tokens.saturating_add(telemetry.helpers.output_tokens),
            "cache_read_input_tokens": telemetry.parent.cache_read_input_tokens.saturating_add(telemetry.helpers.cache_read_input_tokens),
            "cache_creation_input_tokens": telemetry.parent.cache_creation_input_tokens.saturating_add(telemetry.helpers.cache_creation_input_tokens),
            "parent": parent,
            "helpers": helpers,
        },
        "preflight_helpers": telemetry.preflight_helpers,
        "interface": {
            "mode": telemetry.interface.map(|(mode, _)| mode.as_str()),
            "dialect": telemetry.interface.map(|(_, dialect)| dialect.as_str()),
            "provider_selected": {
                "execute_cell_calls": telemetry.execute_cell_calls,
                "direct_tool_calls": telemetry.direct_tool_calls,
                "direct_tools_by_name": telemetry.direct_tools_by_name,
            },
        },
        "frames": {
            "by_origin": by_origin(telemetry, |stats| json!({
                "executed": stats.executed, "failed": stats.failed, "operations": stats.operations,
            })),
            "single_intent_cells": telemetry.single_intent_cells,
            "operations_per_frame_mean": mean(telemetry.tool_calls, telemetry.cells),
            "operations_per_parent_request_mean": mean(telemetry.tool_calls, telemetry.parent.requests),
        },
        "failures": {
            "by_kind": telemetry.failures.value(),
            "by_origin": by_origin(telemetry, |stats| json!({"by_kind": stats.failures.value()})),
        },
        "lifting": {
            "shell_shaped": telemetry.shell_shaped,
            "recognized": telemetry.lifted,
            "fallback": telemetry.shell_shaped.saturating_sub(telemetry.lifted),
            "by_family": Value::Object(
                FAMILIES.iter().zip(telemetry.by_family)
                    .map(|(family, count)| (family.as_str().to_string(), json!(count)))
                    .collect(),
            ),
        },
        "observation": {
            "rows_rendered": telemetry.observation.rows_rendered,
            "rows_suppressed": telemetry.observation.rows_suppressed,
            "bytes_rendered": telemetry.observation.bytes_rendered,
            "bytes_suppressed": telemetry.observation.bytes_suppressed,
            "full_inventories": telemetry.observation.full_inventories,
            "repeated_observations": telemetry.observation.repeated_observations,
        },
        "reductions": telemetry.reductions,
        "recovery": {
            "by_cause": Value::Object(
                RequestCause::ALL.iter().zip(&telemetry.recovery)
                    .map(|(cause, usage)| (cause.as_str().to_string(), usage.value()))
                    .collect(),
            ),
            "repair_usage": telemetry.recovery[cause_index(RequestCause::Repair)].value(),
        },
        "completion": telemetry.completion,
        "after_answer": telemetry.after_answer,
        "progress": {
            "no_progress_notices": telemetry.no_progress_notices,
            "stall_notices": telemetry.stall_notices,
        },
        "acceptance": telemetry.acceptance,
        "capsule": telemetry.capsule,
        "decisions": decisions_value(telemetry),
    })
}

/// `telemetry.decisions` with `helpers: {ranked, skipped, checked, flagged,
/// latency_ms}` folded in (2644, 2645). Those five counters live on
/// [`Telemetry`] itself rather than `TaskState`: the Scout's own ranking
/// happens in `preflight_block`, before a task exists to carry it, so they
/// are recorded here at the point they happen ([`helpers_ranking`],
/// [`helpers_checked`]) and merged in at render time instead of round-
/// tripping through the per-task `decisions_telemetry` this forwards
/// otherwise verbatim.
fn decisions_value(telemetry: &Telemetry) -> Value {
    let Some(mut decisions) = telemetry.decisions.clone() else {
        return Value::Null;
    };
    if let Some(object) = decisions.as_object_mut() {
        object.insert(
            "helpers".to_string(),
            json!({
                "ranked": telemetry.helpers_ranked,
                "skipped": telemetry.helpers_skipped,
                "checked": telemetry.helpers_checked,
                "flagged": telemetry.helpers_flagged,
                "latency_ms": telemetry.helpers_latency_ms,
            }),
        );
    }
    decisions
}

fn by_origin(telemetry: &Telemetry, render: impl Fn(&OriginStats) -> Value) -> Value {
    Value::Object(
        ORIGINS
            .iter()
            .zip(&telemetry.by_origin)
            .map(|(origin, stats)| (origin.as_str().to_string(), render(stats)))
            .collect(),
    )
}

impl Drop for Output {
    fn drop(&mut self) {
        STATE.with(|state| {
            state.borrow_mut().take();
        });
    }
}

pub(super) fn active() -> bool {
    STATE.with(|state| state.borrow().is_some())
}

fn print_json(value: &Value) -> Result<(), String> {
    let mut stdout = std::io::stdout().lock();
    write_json(&mut stdout, value)
        .map_err(|error| format!("could not deliver machine output: {error}"))
}

fn write_json(writer: &mut impl Write, value: &Value) -> std::io::Result<()> {
    serde_json::to_writer(&mut *writer, value).map_err(std::io::Error::other)?;
    writeln!(writer)?;
    writer.flush()
}

fn emit(kind: &str, data: Value) {
    STATE.with(|state| {
        if let Some(state) = state.borrow_mut().as_mut() {
            let event = json!({"schema_version": 1, "type": kind, "sequence": state.sequence, "session_id": state.session_id, "data": data});
            state.sequence += 1;
            match state.format {
                Format::Json => state.events.push(event),
                Format::StreamJson => {
                    if state.delivery_error.is_none() {
                        state.delivery_error = print_json(&event).err();
                    }
                }
                Format::Text => {}
            }
        }
    });
}

pub(super) fn session(id: &str) {
    STATE.with(|state| {
        if let Some(state) = state.borrow_mut().as_mut() {
            state.session_id = Some(id.into());
        }
    });
    emit("session_started", json!({"id": id}));
}

fn model<'a>(models: &'a mut Vec<ModelUsage>, name: &str) -> &'a mut ModelUsage {
    let index = models
        .iter()
        .position(|model| model.model == name)
        .unwrap_or_else(|| {
            models.push(ModelUsage {
                model: name.to_string(),
                ..ModelUsage::default()
            });
            models.len() - 1
        });
    &mut models[index]
}

fn add_parent_response(usage: &mut Usage, measurement: &RequestMeasurement) {
    usage.responses = usage.responses.saturating_add(1);
    if let Some(tokens) = measurement.input_tokens {
        usage.input_tokens = usage.input_tokens.saturating_add(tokens);
        usage.input_reported_requests = usage.input_reported_requests.saturating_add(1);
    }
    if let Some(tokens) = measurement.output_tokens {
        usage.output_tokens = usage.output_tokens.saturating_add(tokens);
        usage.output_reported_requests = usage.output_reported_requests.saturating_add(1);
    }
    if measurement.input_tokens.is_some() && measurement.output_tokens.is_some() {
        usage.reported_requests = usage.reported_requests.saturating_add(1);
    }
    if let Some(tokens) = measurement.cached_input_tokens {
        usage.cache_read_input_tokens = usage.cache_read_input_tokens.saturating_add(tokens);
        usage.cache_read_reported_requests = usage.cache_read_reported_requests.saturating_add(1);
    }
    if let Some(tokens) = measurement.cache_creation_input_tokens {
        usage.cache_creation_input_tokens =
            usage.cache_creation_input_tokens.saturating_add(tokens);
        usage.cache_creation_reported_requests =
            usage.cache_creation_reported_requests.saturating_add(1);
    }
}

/// Counts the attempt before transport begins, so an HTTP, protocol, or
/// context-overflow failure remains part of the benchmark denominator, and
/// charges it to `cause`: every later [`parent_response`] lands in that
/// cause's bucket until the next request starts.
pub(super) fn parent_request_started_with(model_name: &str, cause: RequestCause) {
    STATE.with(|state| {
        let mut state = state.borrow_mut();
        let Some(state) = state.as_mut() else {
            return;
        };
        state.telemetry.parent.requests = state.telemetry.parent.requests.saturating_add(1);
        let model = model(&mut state.telemetry.parent_models, model_name);
        model.calls = model.calls.saturating_add(1);
        model.usage.requests = model.usage.requests.saturating_add(1);
        state.telemetry.current_cause = cause;
        let bucket = &mut state.telemetry.recovery[cause_index(cause)];
        bucket.requests = bucket.requests.saturating_add(1);
    });
}

/// Records which entry points the parent was shown, once per session.
pub(super) fn interface(mode: Interface, dialect: Dialect) {
    STATE.with(|state| {
        if let Some(state) = state.borrow_mut().as_mut() {
            state.telemetry.interface = Some((mode, dialect));
        }
    });
}

/// Adds provider-reported categories for the successful parent response.
/// Missing categories stay visible through their independent coverage counts.
pub(super) fn parent_response(measurement: &RequestMeasurement) {
    STATE.with(|state| {
        let mut state = state.borrow_mut();
        let Some(state) = state.as_mut() else {
            return;
        };
        add_parent_response(&mut state.telemetry.parent, measurement);
        let model = model(&mut state.telemetry.parent_models, &measurement.model);
        if measurement.input_tokens.is_some() && measurement.output_tokens.is_some() {
            model.usage_known_calls = model.usage_known_calls.saturating_add(1);
        }
        add_parent_response(&mut model.usage, measurement);
        let cause = state.telemetry.current_cause;
        let bucket = &mut state.telemetry.recovery[cause_index(cause)];
        bucket.input_tokens = bucket
            .input_tokens
            .saturating_add(measurement.input_tokens.unwrap_or(0));
        bucket.output_tokens = bucket
            .output_tokens
            .saturating_add(measurement.output_tokens.unwrap_or(0));
        bucket.cache_read_input_tokens = bucket
            .cache_read_input_tokens
            .saturating_add(measurement.cached_input_tokens.unwrap_or(0));
        bucket.cache_creation_input_tokens = bucket
            .cache_creation_input_tokens
            .saturating_add(measurement.cache_creation_input_tokens.unwrap_or(0));
        bucket.wall_time_ms = bucket.wall_time_ms.saturating_add(measurement.elapsed_ms);
    });
}

fn add_helper_usage(usage: &mut Usage, record: &HelperRecord) {
    let measured = &record.usage;
    usage.requests = usage.requests.saturating_add(u64::from(measured.requests));
    usage.responses = usage
        .responses
        .saturating_add(u64::from(measured.responses));
    usage.reported_requests = usage
        .reported_requests
        .saturating_add(u64::from(measured.reported_requests));
    usage.input_tokens = usage.input_tokens.saturating_add(measured.input_tokens);
    usage.output_tokens = usage.output_tokens.saturating_add(measured.output_tokens);
    usage.cache_read_input_tokens = usage
        .cache_read_input_tokens
        .saturating_add(measured.cache_read_input_tokens);
    usage.cache_creation_input_tokens = usage
        .cache_creation_input_tokens
        .saturating_add(measured.cache_creation_input_tokens);
    usage.input_reported_requests = usage
        .input_reported_requests
        .saturating_add(u64::from(measured.reported_requests));
    usage.output_reported_requests = usage
        .output_reported_requests
        .saturating_add(u64::from(measured.reported_requests));
    usage.cache_read_reported_requests = usage
        .cache_read_reported_requests
        .saturating_add(u64::from(measured.cache_read_reported_requests));
    usage.cache_creation_reported_requests = usage
        .cache_creation_reported_requests
        .saturating_add(u64::from(measured.cache_creation_reported_requests));
}

fn helper(call_site: &str, record: &HelperRecord) {
    STATE.with(|state| {
        let mut state = state.borrow_mut();
        let Some(state) = state.as_mut() else {
            return;
        };
        state.telemetry.helper_calls = state.telemetry.helper_calls.saturating_add(1);
        if record.usage.coverage_known {
            state.telemetry.helper_usage_known_calls =
                state.telemetry.helper_usage_known_calls.saturating_add(1);
        }
        if !record.outcome.ok {
            state.telemetry.helper_failures = state.telemetry.helper_failures.saturating_add(1);
        }
        add_helper_usage(&mut state.telemetry.helpers, record);
        let model_name = if record.usage.model.is_empty() {
            "unknown"
        } else {
            &record.usage.model
        };
        let model = model(&mut state.telemetry.helper_models, model_name);
        model.calls = model.calls.saturating_add(1);
        if record.usage.coverage_known {
            model.usage_known_calls = model.usage_known_calls.saturating_add(1);
        }
        if !record.outcome.ok {
            model.failed_calls = model.failed_calls.saturating_add(1);
        }
        add_helper_usage(&mut model.usage, record);
        if call_site == "preflight" {
            state.telemetry.preflight_helpers.push(json!({
                "call_site": call_site,
                "record": record,
            }));
        }
    });
    emit("helper", json!({"call_site": call_site, "record": record}));
}

pub(super) fn preflight(record: &HelperRecord) {
    helper("preflight", record);
}

/// The acceptance lister ran before the first turn.
pub(super) fn acceptance_helper(record: &HelperRecord) {
    helper("acceptance", record);
}

/// Counts a stall notice at the head of a feedback.
pub(super) fn stall_notice() {
    STATE.with(|state| {
        if let Some(state) = state.borrow_mut().as_mut() {
            state.telemetry.stall_notices = state.telemetry.stall_notices.saturating_add(1);
        }
    });
}

/// Stores the acceptance list's latest evaluation on the result and emits
/// it as one `acceptance` event; a later evaluation replaces it.
pub(super) fn acceptance(value: Value) {
    if !active() {
        return;
    }
    STATE.with(|state| {
        if let Some(state) = state.borrow_mut().as_mut() {
            state.telemetry.acceptance = Some(value.clone());
        }
    });
    emit("acceptance", value);
}

pub(super) fn cell_helpers(records: &[HelperRecord]) {
    for record in records {
        helper("cell", record);
    }
}

pub(super) fn message(message: &Message) {
    if !active() {
        return;
    }
    let blocks: Vec<Value> = message.content.iter().map(|block| match block {
        Block::Text(text) => json!({"type": "text", "text": text}),
        Block::Image { media_type, .. } => json!({"type": "image", "media_type": media_type}),
        Block::ToolUse { id, name, input } => json!({"type": "tool_use", "id": id, "name": name, "input": input}),
        Block::ToolResult { tool_use_id, content, is_error } => json!({"type": "tool_result", "tool_use_id": tool_use_id, "content": content, "is_error": is_error}),
        Block::Thinking { thinking, .. } => json!({"type": "thinking", "thinking": thinking}),
        Block::RedactedThinking { .. } => json!({"type": "redacted_thinking"}),
    }).collect();
    if message.role == Role::Assistant {
        STATE.with(|state| {
            if let Some(state) = state.borrow_mut().as_mut() {
                for block in &message.content {
                    let Block::ToolUse { name, .. } = block else {
                        continue;
                    };
                    let telemetry = &mut state.telemetry;
                    if name == "execute_cell" {
                        telemetry.execute_cell_calls =
                            telemetry.execute_cell_calls.saturating_add(1);
                    } else {
                        telemetry.direct_tool_calls = telemetry.direct_tool_calls.saturating_add(1);
                        let count = telemetry
                            .direct_tools_by_name
                            .entry(name.clone())
                            .or_default();
                        *count = count.saturating_add(1);
                    }
                }
            }
        });
        let texts: Vec<&str> = message
            .content
            .iter()
            .filter_map(|block| {
                if let Block::Text(text) = block {
                    Some(text.as_str())
                } else {
                    None
                }
            })
            .collect();
        if !texts.is_empty() {
            STATE.with(|state| {
                if let Some(state) = state.borrow_mut().as_mut() {
                    state.answer = Some(texts.join("\n"));
                }
            });
        }
    }
    emit(
        "message",
        json!({"role": message.role.as_str(), "content": blocks}),
    );
}

/// Records one execution frame under its origin, with the cell's own throw
/// (`class`, `message`) when it threw, and what its rendering cost.
///
/// A failure is counted once per failed call, and once for the frame's own
/// throw only when no call failed — so a frame that threw because a call
/// threw is one failure, not two.
pub(super) fn cell_frame(
    record: &CellRecord,
    origin: Origin,
    error: Option<(&str, &str)>,
    observation: ObservationStats,
    reduction: ReductionStats,
    source_is_single_intent: bool,
) {
    if !active() {
        return;
    }
    STATE.with(|state| {
        let mut state = state.borrow_mut();
        let state = state.as_mut().expect("active machine output has state");
        let telemetry = &mut state.telemetry;
        let threw = record.outcome == CellOutcomeKind::Threw;
        let first_frame = telemetry.cells == 0;
        telemetry.cells = telemetry.cells.saturating_add(1);
        if threw {
            telemetry.cell_failures = telemetry.cell_failures.saturating_add(1);
        }
        telemetry.tool_calls = telemetry
            .tool_calls
            .saturating_add(record.calls.len() as u64);
        telemetry.tool_failures = telemetry.tool_failures.saturating_add(
            record
                .calls
                .iter()
                .filter(|call| !matches!(&call.ended, Ended::Ok))
                .count() as u64,
        );

        let index = origin_index(origin);
        let stats = &mut telemetry.by_origin[index];
        stats.executed = stats.executed.saturating_add(1);
        if threw {
            stats.failed = stats.failed.saturating_add(1);
        }
        stats.operations = stats.operations.saturating_add(record.calls.len() as u64);

        let mut failed_call = false;
        let mut repeated = 0u64;
        for call in &record.calls {
            if let Some(kind) = taxonomy::classify_call(call) {
                failed_call = true;
                telemetry.failures.add(kind);
                telemetry.by_origin[index].failures.add(kind);
            }
            if call.tool == "bash" || call.lifted_from.is_some() {
                telemetry.shell_shaped = telemetry.shell_shaped.saturating_add(1);
                if call.lifted_from.is_some() {
                    telemetry.lifted = telemetry.lifted.saturating_add(1);
                }
                if let Some(family) = taxonomy::shell_family(call)
                    && let Some(slot) = FAMILIES.iter().position(|f| *f == family)
                {
                    telemetry.by_family[slot] = telemetry.by_family[slot].saturating_add(1);
                }
            }
            if call.repeat_of.is_some() {
                repeated = repeated.saturating_add(1);
            }
        }
        if threw && !failed_call {
            let kind = error.map_or(FailureKind::Runtime, |(class, message)| {
                taxonomy::classify_cell_error(class, message)
            });
            telemetry.failures.add(kind);
            telemetry.by_origin[index].failures.add(kind);
        }
        if origin == Origin::AuthoredCell && source_is_single_intent {
            telemetry.single_intent_cells = telemetry.single_intent_cells.saturating_add(1);
        }

        let totals = &mut telemetry.observation;
        totals.rows_rendered = totals
            .rows_rendered
            .saturating_add(observation.rows_rendered as u64);
        totals.rows_suppressed = totals
            .rows_suppressed
            .saturating_add(observation.rows_suppressed as u64);
        totals.bytes_rendered = totals
            .bytes_rendered
            .saturating_add(observation.bytes_rendered as u64);
        totals.bytes_suppressed = totals
            .bytes_suppressed
            .saturating_add(observation.bytes_suppressed() as u64);
        if observation.full_inventory && !first_frame {
            totals.full_inventories = totals.full_inventories.saturating_add(1);
        }
        // Either producer may name a repeat: the trajectory's `repeat_of` or
        // the renderer's count. The larger is the count, never the sum.
        totals.repeated_observations = totals
            .repeated_observations
            .saturating_add(repeated.max(observation.repeated_observations as u64));
        telemetry.reductions.add(&reduction);
    });
    let mut data = serde_json::to_value(record).expect("cell record is serializable");
    data["origin"] = json!(origin.as_str());
    emit("cell", data);
}

/// Records the findings the gate noted without holding the answer.
pub(super) fn completion_notes(notes: &[String]) {
    STATE.with(|state| {
        if let Some(state) = state.borrow_mut().as_mut() {
            let entry = state
                .telemetry
                .after_answer
                .get_or_insert_with(|| json!({"notes": [], "checks": []}));
            if let Some(list) = entry["notes"].as_array_mut() {
                list.extend(notes.iter().map(|note| json!(note)));
            }
        }
    });
}

/// Records the checks that finished behind the answer.
/// `wait_ms` is how long the exit waited for them: the answer was already
/// out, so time-to-answer is the run's wall time less it.
pub(super) fn after_checks(
    notes: &[super::after::Note],
    learned: &[String],
    failed: &[String],
    wait_ms: u64,
) {
    STATE.with(|state| {
        if let Some(state) = state.borrow_mut().as_mut() {
            let entry = state
                .telemetry
                .after_answer
                .get_or_insert_with(|| json!({"notes": [], "checks": []}));
            entry["learned"] = json!(learned);
            entry["wait_ms"] = json!(wait_ms);
            entry["failed"] = json!(failed);
            if let Some(list) = entry["checks"].as_array_mut() {
                list.extend(notes.iter().map(|note| {
                    json!({
                        "verdict": note.verdict,
                        "text": note.text,
                        "input_tokens": note.record.usage.input_tokens,
                        "output_tokens": note.record.usage.output_tokens,
                    })
                }));
            }
        }
    });
}

/// Records the completion claim and what verified it.
pub(super) fn completion(claimed: bool, verified: bool, findings: &[String], deferred: u32) {
    STATE.with(|state| {
        if let Some(state) = state.borrow_mut().as_mut() {
            state.telemetry.completion = Some(json!({
                "claimed": claimed,
                "verified": verified,
                "findings": findings,
                "deferred": deferred,
            }));
        }
    });
}

/// Counts one no-progress notice shown to the parent.
pub(super) fn no_progress_notice() {
    STATE.with(|state| {
        if let Some(state) = state.borrow_mut().as_mut() {
            state.telemetry.no_progress_notices =
                state.telemetry.no_progress_notices.saturating_add(1);
        }
    });
}

/// Stores this task's decision summary (`decide-model.md`): `None` only when
/// no decision model is configured. A later call replaces the stored one, so
/// the result always carries the latest hold/override counts and, once the
/// completion question has been asked (2616), the `completion` sub-object.
pub(super) fn decisions(value: Option<Value>) {
    let Some(value) = value else {
        return;
    };
    STATE.with(|state| {
        if let Some(state) = state.borrow_mut().as_mut() {
            state.telemetry.decisions = Some(value);
        }
    });
}

/// Counts one Scout candidate ranking (2644): every candidate the question
/// actually answered, however many of them cleared the floor, plus the
/// request's own latency -- folded into `decisions.helpers` at render time
/// ([`decisions_value`]).
pub(super) fn helpers_ranking(ranking: &crate::helpers::ScoutRanking) {
    STATE.with(|state| {
        if let Some(state) = state.borrow_mut().as_mut() {
            let telemetry = &mut state.telemetry;
            telemetry.helpers_ranked = telemetry.helpers_ranked.saturating_add(ranking.ranked);
            telemetry.helpers_skipped = telemetry.helpers_skipped.saturating_add(ranking.skipped);
            telemetry.helpers_latency_ms = telemetry
                .helpers_latency_ms
                .saturating_add(ranking.latency_ms);
        }
    });
}

/// Counts one judged helper return (2645), from either call site the judge
/// reaches -- the preflight Scout's own result or the completion gate's
/// fresh checker. `flagged` is the judge's own signal (`noul <= floor`),
/// counted the same in `shadow` and `on`: `shadow` asks and counts every
/// question this project's decisions make, and never changes what a
/// helper's own result carries.
pub(super) fn helpers_checked(noul: f64, floor: f64, latency_ms: u64) {
    STATE.with(|state| {
        if let Some(state) = state.borrow_mut().as_mut() {
            let telemetry = &mut state.telemetry;
            telemetry.helpers_checked = telemetry.helpers_checked.saturating_add(1);
            if noul <= floor {
                telemetry.helpers_flagged = telemetry.helpers_flagged.saturating_add(1);
            }
            telemetry.helpers_latency_ms = telemetry.helpers_latency_ms.saturating_add(latency_ms);
        }
    });
}

/// Stores the task capsule, emits it as one `capsule` event, and carries it
/// on the result. A later capsule replaces the stored one.
pub(super) fn capsule(value: Value) {
    if !active() {
        return;
    }
    STATE.with(|state| {
        if let Some(state) = state.borrow_mut().as_mut() {
            state.telemetry.capsule = Some(value.clone());
        }
    });
    emit("capsule", value);
}

#[cfg(test)]
mod tests {
    use super::*;

    struct BrokenWriter {
        fail_write: bool,
    }
    impl Write for BrokenWriter {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            if self.fail_write {
                Err(std::io::Error::new(
                    std::io::ErrorKind::BrokenPipe,
                    "fixture write failure",
                ))
            } else {
                Ok(bytes.len())
            }
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Err(std::io::Error::new(
                std::io::ErrorKind::BrokenPipe,
                "fixture flush failure",
            ))
        }
    }

    #[test]
    fn machine_delivery_propagates_write_and_flush_failures() {
        for fail_write in [true, false] {
            assert!(
                write_json(&mut BrokenWriter { fail_write }, &json!({"success":true})).is_err()
            );
        }
    }

    fn call(tool: &str, ended: Ended) -> crate::runtime::outcome::CallRecord {
        crate::runtime::outcome::CallRecord {
            tool: tool.into(),
            args: BTreeMap::new(),
            evidence: None,
            lifted_from: None,
            exit_code: None,
            repeat_of: None,
            error: None,
            ended,
        }
    }

    fn record(
        source: &str,
        outcome: CellOutcomeKind,
        calls: Vec<crate::runtime::outcome::CallRecord>,
    ) -> CellRecord {
        CellRecord {
            cell: 1,
            source: source.into(),
            description: None,
            outcome,
            handles: Vec::new(),
            calls,
        }
    }

    fn current_telemetry() -> Value {
        STATE.with(|state| telemetry_value(&state.borrow().as_ref().unwrap().telemetry))
    }

    #[test]
    fn frames_are_counted_by_origin_and_a_failure_once_per_frame() {
        let _output = Output::start(Format::Json);
        // A direct frame whose one call was denied: the denial is the
        // frame's failure and the throw is not counted a second time.
        let denied = record(
            "await bash({command: 'rm -rf /'})",
            CellOutcomeKind::Threw,
            vec![call(
                "bash",
                Ended::Denied {
                    rule: "no allow".into(),
                },
            )],
        );
        cell_frame(
            &denied,
            Origin::DirectTool,
            Some(("PermissionDenied", "no allow")),
            ObservationStats::default(),
            ReductionStats::default(),
            false,
        );
        // An authored frame that threw before any call ran.
        let syntax = record("const = ;", CellOutcomeKind::Threw, Vec::new());
        cell_frame(
            &syntax,
            Origin::AuthoredCell,
            Some(("SyntaxError", "Unexpected token")),
            ObservationStats::default(),
            ReductionStats::default(),
            false,
        );
        // An authored single-intent frame with two lifted searches.
        let mut lifted = call("rg", Ended::Ok);
        lifted.lifted_from = Some("rg".into());
        let mut plain = call("bash", Ended::Ok);
        plain.args.insert("command".into(), "cargo test".into());
        plain.repeat_of = Some(1);
        let searched = record(
            "await rg({pattern: 'x'});",
            CellOutcomeKind::Yielded,
            vec![lifted, plain],
        );
        cell_frame(
            &searched,
            Origin::AuthoredCell,
            None,
            ObservationStats {
                rows_rendered: 2,
                rows_suppressed: 3,
                bytes_rendered: 100,
                bytes_full_inventory: 400,
                full_inventory: true,
                repeated_observations: 0,
            },
            ReductionStats {
                attempted: 1,
                made: 1,
                bytes_in: 5_000,
                bytes_out: 200,
                ..ReductionStats::default()
            },
            true,
        );
        let value = current_telemetry();
        assert_eq!(value["cells"]["executed"], 3);
        assert_eq!(value["cells"]["failed"], 2);
        assert_eq!(value["frames"]["by_origin"]["direct_tool"]["executed"], 1);
        assert_eq!(value["frames"]["by_origin"]["direct_tool"]["failed"], 1);
        assert_eq!(value["frames"]["by_origin"]["direct_tool"]["operations"], 1);
        assert_eq!(value["frames"]["by_origin"]["authored_cell"]["executed"], 2);
        assert_eq!(value["frames"]["by_origin"]["authored_cell"]["failed"], 1);
        assert_eq!(
            value["frames"]["by_origin"]["authored_cell"]["operations"],
            2
        );
        assert_eq!(value["frames"]["single_intent_cells"], 1);
        assert_eq!(value["frames"]["operations_per_frame_mean"], 1.0);
        assert_eq!(value["failures"]["by_kind"]["denial"], 1);
        assert_eq!(value["failures"]["by_kind"]["syntax"], 1);
        assert_eq!(value["failures"]["by_kind"]["runtime"], 0);
        assert_eq!(
            value["failures"]["by_origin"]["direct_tool"]["by_kind"]["denial"],
            1
        );
        assert_eq!(
            value["failures"]["by_origin"]["authored_cell"]["by_kind"]["syntax"],
            1
        );
        assert_eq!(value["lifting"]["shell_shaped"], 3);
        assert_eq!(value["lifting"]["recognized"], 1);
        assert_eq!(value["lifting"]["fallback"], 2);
        assert_eq!(value["lifting"]["by_family"]["search"], 1);
        assert_eq!(value["lifting"]["by_family"]["verification"], 1);
        assert_eq!(value["observation"]["rows_rendered"], 2);
        assert_eq!(value["observation"]["rows_suppressed"], 3);
        assert_eq!(value["observation"]["bytes_rendered"], 100);
        assert_eq!(value["observation"]["bytes_suppressed"], 300);
        assert_eq!(value["observation"]["full_inventories"], 1);
        assert_eq!(value["observation"]["repeated_observations"], 1);
        assert_eq!(value["reductions"]["attempted"], 1);
        assert_eq!(value["reductions"]["bytes_out"], 200);
        let origins: Vec<Value> = STATE.with(|state| {
            state
                .borrow()
                .as_ref()
                .unwrap()
                .events
                .iter()
                .map(|event| event["data"]["origin"].clone())
                .collect()
        });
        assert_eq!(
            origins,
            vec![
                json!("direct_tool"),
                json!("authored_cell"),
                json!("authored_cell")
            ]
        );
    }

    #[test]
    fn the_first_full_inventory_is_free_and_a_throw_without_a_cause_is_runtime() {
        let _output = Output::start(Format::Json);
        let full = ObservationStats {
            full_inventory: true,
            ..ObservationStats::default()
        };
        let threw = record("throw new Error('x')", CellOutcomeKind::Threw, Vec::new());
        cell_frame(
            &threw,
            Origin::AuthoredCell,
            None,
            full,
            ReductionStats::default(),
            false,
        );
        cell_frame(
            &threw,
            Origin::AuthoredCell,
            None,
            full,
            ReductionStats::default(),
            false,
        );
        let value = current_telemetry();
        assert_eq!(value["observation"]["full_inventories"], 1);
        assert_eq!(value["failures"]["by_kind"]["runtime"], 2);
    }

    #[test]
    fn a_response_lands_in_the_bucket_of_the_cause_its_request_started_with() {
        let _output = Output::start(Format::Json);
        let measurement = |input: u64, elapsed: u64| RequestMeasurement {
            cell: 1,
            model: "m".into(),
            elapsed_ms: elapsed,
            input_tokens: Some(input),
            output_tokens: Some(1),
            cached_input_tokens: Some(2),
            cache_creation_input_tokens: None,
            served: crate::contract::ServedBy::default(),
        };
        parent_request_started_with("m", RequestCause::Implementation);
        parent_response(&measurement(10, 5));
        parent_request_started_with("m", RequestCause::Repair);
        parent_response(&measurement(100, 7));
        parent_request_started_with("m", RequestCause::Repair);
        // No response: the request still counts, its tokens do not.
        let value = current_telemetry();
        let by_cause = &value["recovery"]["by_cause"];
        assert_eq!(by_cause["implementation"]["requests"], 1);
        assert_eq!(by_cause["implementation"]["input_tokens"], 10);
        assert_eq!(by_cause["implementation"]["known_tokens"], 13);
        assert_eq!(by_cause["implementation"]["wall_time_ms"], 5);
        assert_eq!(by_cause["repair"]["requests"], 2);
        assert_eq!(by_cause["repair"]["input_tokens"], 100);
        assert_eq!(by_cause["repair"]["cache_read_input_tokens"], 2);
        assert_eq!(by_cause["repair"]["wall_time_ms"], 7);
        assert_eq!(by_cause["exploration"]["requests"], 0);
        assert_eq!(value["recovery"]["repair_usage"], by_cause["repair"]);
        assert_eq!(value["tokens"]["parent"]["requests"], 3);
        assert_eq!(value["frames"]["operations_per_parent_request_mean"], 0.0);
    }

    #[test]
    fn the_interface_and_the_provider_selection_are_recorded() {
        let _output = Output::start(Format::Json);
        interface(Interface::Tools, Dialect::OpenAi);
        message(&Message {
            role: Role::Assistant,
            content: vec![
                Block::ToolUse {
                    id: "1".into(),
                    name: "shell".into(),
                    input: json!({}),
                },
                Block::ToolUse {
                    id: "2".into(),
                    name: "execute_cell".into(),
                    input: json!({}),
                },
                Block::ToolUse {
                    id: "3".into(),
                    name: "shell".into(),
                    input: json!({}),
                },
            ],
            historical: None,
        });
        let value = current_telemetry();
        assert_eq!(value["interface"]["mode"], "tools");
        assert_eq!(value["interface"]["dialect"], "openai");
        let selected = &value["interface"]["provider_selected"];
        assert_eq!(selected["execute_cell_calls"], 1);
        assert_eq!(selected["direct_tool_calls"], 2);
        assert_eq!(selected["direct_tools_by_name"]["shell"], 2);
    }

    #[test]
    fn completion_progress_and_the_capsule_ride_the_result_and_the_capsule_is_an_event() {
        let _output = Output::start(Format::Json);
        completion(true, false, &["tests not run".into()], 2);
        no_progress_notice();
        no_progress_notice();
        capsule(json!({"summary": "did the thing"}));
        let value = current_telemetry();
        assert_eq!(value["completion"]["claimed"], true);
        assert_eq!(value["completion"]["verified"], false);
        assert_eq!(value["completion"]["findings"][0], "tests not run");
        assert_eq!(value["completion"]["deferred"], 2);
        assert_eq!(value["progress"]["no_progress_notices"], 2);
        assert_eq!(value["capsule"]["summary"], "did the thing");
        let events = STATE.with(|state| state.borrow().as_ref().unwrap().events.clone());
        assert_eq!(events.len(), 1);
        assert_eq!(events[0]["type"], "capsule");
        assert_eq!(events[0]["data"]["summary"], "did the thing");
    }

    #[test]
    fn decisions_carries_the_completion_sub_object_and_a_later_call_replaces_it() {
        let _output = Output::start(Format::Json);
        decisions(Some(json!({"model": "jev-latest", "completion": null})));
        assert!(current_telemetry()["decisions"]["completion"].is_null());
        decisions(Some(json!({
            "model": "jev-latest",
            "completion": {
                "noul": 0.06,
                "latency_ms": 12,
                "truncated": false,
                "finding_added": true,
                "checker_skipped": null,
            },
        })));
        let value = current_telemetry();
        assert_eq!(value["decisions"]["completion"]["noul"], 0.06);
        assert_eq!(value["decisions"]["completion"]["finding_added"], true);
        assert!(value["decisions"]["completion"]["checker_skipped"].is_null());
    }

    #[test]
    fn failed_stream_delivery_cannot_finish_as_success() {
        let output = Output::start(Format::StreamJson);
        STATE.with(|state| {
            state.borrow_mut().as_mut().unwrap().delivery_error =
                Some("fixture delivery failure".into())
        });
        assert_eq!(
            output.finish(&Ok(())),
            Err("fixture delivery failure".into())
        );
        assert!(!active());
    }
}
