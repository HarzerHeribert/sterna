//! Local measurements for provider requests. These values are presentation
//! state only: they are never rendered into a model message or rollout.

use crate::contract::ServedBy;
use crate::wire::Usage;

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct RequestMeasurement {
    pub cell: usize,
    pub model: String,
    pub elapsed_ms: u64,
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    pub cached_input_tokens: Option<u64>,
    pub cache_creation_input_tokens: Option<u64>,
    pub served: ServedBy,
}

impl RequestMeasurement {
    /// `served` must be correlated to this exact request. Project-wide routing
    /// observations cannot safely be attributed here during concurrent work.
    pub fn from_response(
        cell: usize,
        model: String,
        elapsed_ms: u64,
        served: ServedBy,
        usage: Option<&Usage>,
    ) -> Self {
        let input_tokens = served
            .input_tokens
            .or_else(|| usage.map(|row| row.input_tokens));
        let output_tokens = served
            .output_tokens
            .or_else(|| usage.map(|row| row.output_tokens));
        Self {
            cell,
            model,
            elapsed_ms,
            input_tokens,
            output_tokens,
            cached_input_tokens: served
                .cached_input_tokens
                .or_else(|| usage.and_then(|row| row.cache_read_input_tokens)),
            cache_creation_input_tokens: usage.and_then(|row| row.cache_creation_input_tokens),
            served,
        }
    }

    /// Tokens occupying the request context at the provider boundary.
    ///
    /// Cache reads and writes still occupy the model's context window even
    /// when the provider bills them differently. Sterna therefore adds the
    /// three input classes and deliberately excludes output: this is the
    /// context of the request that produced the response, not cumulative
    /// task spend.
    pub fn context_tokens(&self) -> Option<u64> {
        let mut known = false;
        let mut total = 0u64;
        for value in [
            self.input_tokens,
            self.cached_input_tokens,
            self.cache_creation_input_tokens,
        ]
        .into_iter()
        .flatten()
        {
            known = true;
            total = total.saturating_add(value);
        }
        known.then_some(total)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn correlated_gateway_fields_take_precedence_individually() {
        let served = ServedBy {
            input_tokens: Some(30),
            output_tokens: None,
            cached_input_tokens: Some(20),
            ..ServedBy::default()
        };
        let usage = Usage {
            input_tokens: 300,
            output_tokens: 40,
            cache_read_input_tokens: Some(200),
            cache_creation_input_tokens: Some(50),
        };
        let measured = RequestMeasurement::from_response(
            2,
            "requested".into(),
            17,
            served.clone(),
            Some(&usage),
        );
        assert_eq!(measured.input_tokens, Some(30));
        assert_eq!(measured.output_tokens, Some(40));
        assert_eq!(measured.cached_input_tokens, Some(20));
        assert_eq!(measured.cache_creation_input_tokens, Some(50));
        assert_eq!(measured.served, served);
    }

    #[test]
    fn response_usage_is_used_only_when_gateway_tokens_are_absent() {
        let served = ServedBy {
            provider: Some("provider".into()),
            ..ServedBy::default()
        };
        let usage = Usage {
            input_tokens: 12,
            output_tokens: 4,
            cache_read_input_tokens: Some(100),
            cache_creation_input_tokens: Some(25),
        };
        let measured =
            RequestMeasurement::from_response(1, "model".into(), 9, served, Some(&usage));
        assert_eq!(measured.input_tokens, Some(12));
        assert_eq!(measured.output_tokens, Some(4));
        assert_eq!(measured.cached_input_tokens, Some(100));
        assert_eq!(measured.cache_creation_input_tokens, Some(25));
    }

    #[test]
    fn explicit_cache_zero_is_preserved_and_missing_cache_fields_stay_unknown() {
        for (read, written) in [(None, None), (Some(0), Some(0))] {
            let usage = Usage {
                input_tokens: 5,
                output_tokens: 2,
                cache_read_input_tokens: read,
                cache_creation_input_tokens: written,
            };
            let measured = RequestMeasurement::from_response(
                1,
                "model".into(),
                0,
                ServedBy::default(),
                Some(&usage),
            );
            assert_eq!(measured.cached_input_tokens, read);
            assert_eq!(measured.cache_creation_input_tokens, written);
        }
    }

    #[test]
    fn absent_usage_stays_unknown() {
        let measured =
            RequestMeasurement::from_response(1, "model".into(), 9, ServedBy::default(), None);
        assert_eq!(measured.input_tokens, None);
        assert_eq!(measured.output_tokens, None);
        assert_eq!(measured.cached_input_tokens, None);
        assert_eq!(measured.cache_creation_input_tokens, None);
    }

    #[test]
    fn context_is_one_request_and_includes_every_input_class_only() {
        let measured = RequestMeasurement::from_response(
            1,
            "model".into(),
            9,
            ServedBy::default(),
            Some(&Usage {
                input_tokens: 12,
                output_tokens: 99,
                cache_read_input_tokens: Some(100),
                cache_creation_input_tokens: Some(25),
            }),
        );
        assert_eq!(measured.context_tokens(), Some(137));
    }
}
