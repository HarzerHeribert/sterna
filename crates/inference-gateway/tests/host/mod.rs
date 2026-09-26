//! What a host gives a gateway in these tests: somewhere its routing
//! observations land. A real host records them in its own store; here they
//! are kept in memory, in arrival order, so a test can read back exactly what
//! the gateway reported through its [`ObservationSink`].
#![allow(dead_code)]

use std::sync::{Arc, Mutex};

use inference_gateway::gateway::{Observation, ObservationSink};
use inference_gateway::routing::evidence::{NewObservation, RoutingObservation};

/// Every routed observation the gateway reported, oldest first.
#[derive(Default)]
pub struct Ledger {
    rows: Mutex<Vec<RoutingObservation>>,
}

impl Ledger {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    pub fn record(&self, observation: NewObservation, observed_at_unix: i64) {
        let mut rows = self.rows.lock().unwrap_or_else(|e| e.into_inner());
        let seq = rows.len() as i64 + 1;
        rows.push(row(observation, seq, observed_at_unix));
    }

    /// Every row recorded so far. The window arguments are the host store's
    /// signature; an in-memory ledger holds only this test's rows, so it
    /// returns them all.
    pub fn observations_in_window(
        &self,
        _now: i64,
        _window: i64,
    ) -> Result<Vec<RoutingObservation>, std::convert::Infallible> {
        Ok(self.rows.lock().unwrap_or_else(|e| e.into_inner()).clone())
    }
}

/// The sink a host installs: routed observations go to `ledger`, anything
/// else to `next` when there is one.
pub fn observation_sink(ledger: Arc<Ledger>, next: Option<ObservationSink>) -> ObservationSink {
    Arc::new(move |observation| match observation {
        Observation::Routed {
            observation,
            observed_at_unix,
        } => ledger.record(*observation, observed_at_unix),
        other => {
            if let Some(next) = &next {
                next(other);
            }
        }
    })
}

fn row(new: NewObservation, seq: i64, observed_at_unix: i64) -> RoutingObservation {
    RoutingObservation {
        seq,
        project_id: "test".to_owned(),
        observed_at_unix,
        provider: new.provider,
        model: new.model,
        route: new.route,
        quota_context: new.quota_context,
        harness: new.harness,
        purpose: new.purpose,
        dispatched_at_unix: new.dispatched_at_unix,
        first_byte_at_unix: new.first_byte_at_unix,
        first_token_at_unix: new.first_token_at_unix,
        first_tool_call_at_unix: new.first_tool_call_at_unix,
        completed_at_unix: new.completed_at_unix,
        first_byte_ms: new.first_byte_ms,
        first_token_ms: new.first_token_ms,
        first_tool_call_ms: new.first_tool_call_ms,
        completed_ms: new.completed_ms,
        input_tokens: new.input_tokens,
        output_tokens: new.output_tokens,
        cached_input_tokens: new.cached_input_tokens,
        cost: new.cost,
        tool_rounds: new.tool_rounds,
        retries: new.retries,
        repairs: new.repairs,
        failovers: new.failovers,
        outcome: new.outcome,
        failure_class: new.failure_class,
        task_class: new.task_class,
        session_id: new.session_id,
        effort_level: new.effort_level,
        turn_shape: new.turn_shape,
        context_state: new.context_state,
    }
}
