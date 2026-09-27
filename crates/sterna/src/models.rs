//! What the gateway knows about the models it serves, for the one caller that
//! needs it: the subagent roster in the system block.
//!
//! **The figures are the gateway's, and sterna only asks.** `inference-gateway
//! models --json` answers with the snapshot baked into that binary overlaid
//! by whatever the user's own Artificial Analysis key last fetched
//! (`inference_gateway::models`), so a session with no Glasshouse installed
//! anywhere still gets them. This module parses that document and keeps the
//! models this session can actually reach.
//!
//! **An absence is never an error.** No gateway, an older gateway that does
//! not know the subcommand, a document that will not parse -- each leaves the
//! roster unmeasured, and an unmeasured model is still offered to the model,
//! by name, without figures.

use std::collections::BTreeMap;
use std::sync::OnceLock;

use serde::Deserialize;

use crate::gateway::Gateway;

/// One model a session may delegate to.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct RosterModel {
    /// The id `agent.run({model})` names -- the served spelling, not the
    /// published slug.
    pub id: String,
    /// The headline index, higher is better; `None` when nobody published one.
    pub intelligence: Option<f64>,
    pub coding: Option<f64>,
}

/// The `models` half of `inference-gateway models --json`, and the
/// `observed` half beside it.
#[derive(Debug, Clone, Default, Deserialize)]
struct Document {
    #[serde(default)]
    models: BTreeMap<String, Facts>,
    /// Windows the gateway watched a route enforce. Absent from an older
    /// gateway, which is why it defaults rather than being required.
    #[serde(default)]
    observed: BTreeMap<String, ObservedFacts>,
    /// What each subscription account's own provider says it is served
    /// with, per plan. Absent from an older gateway.
    #[serde(default)]
    served: BTreeMap<String, ServedFacts>,
}

#[derive(Debug, Clone, Default, Deserialize)]
struct ServedFacts {
    #[serde(default)]
    context_window_tokens: Option<u64>,
    #[serde(default)]
    max_output_tokens: Option<u64>,
}

#[derive(Debug, Clone, Default, Deserialize)]
struct ObservedFacts {
    #[serde(default)]
    context_window_tokens: Option<u64>,
}

#[derive(Debug, Clone, Default, Deserialize)]
struct Facts {
    #[serde(default)]
    intelligence: Option<f64>,
    #[serde(default)]
    coding: Option<f64>,
    #[serde(default)]
    context_window_tokens: Option<u64>,
    #[serde(default)]
    max_output_tokens: Option<u64>,
}

/// The name a model is looked up under: lower case, `.` and `_` as `-`.
///
/// The same one line as `inference_gateway::models::normalise` and
/// `glasshouse::routing::analysis::normalise`; these three processes share no
/// library, and a published slug (`gpt-5-6-sol`) has to find a served id
/// (`gpt-5.6-sol`).
#[must_use]
pub fn normalise(model: &str) -> String {
    model.trim().to_ascii_lowercase().replace(['.', '_'], "-")
}

/// Whether `id` answers a conversation. An account's list also carries
/// image, speech, embedding and batch ids, which cannot run a turn and are
/// never offered as one.
#[must_use]
pub fn chat_capable(id: &str) -> bool {
    const NOT_CHAT: [&str; 7] = [
        "gpt-image-",
        "dall-e-",
        "whisper-",
        "tts-",
        "text-embedding-",
        "omni-moderation",
        "codex-auto-review",
    ];
    let id = normalise(id);
    !id.ends_with(":batch") && !NOT_CHAT.iter().any(|prefix| id.starts_with(prefix))
}

/// How well `id` answers a search, best first: the exact id, then a prefix,
/// then the terms side by side (`sonnet 5` in `claude-sonnet-5`), then every
/// term a whole word somewhere in the id, then a plain substring.
#[must_use]
pub fn search_rank(id: &str, terms: &[String]) -> u8 {
    let id = normalise(id);
    let joined = normalise(&terms.join("-"));
    let words: Vec<&str> = id.split(['-', '/', ':']).collect();
    if id == joined {
        0
    } else if id.starts_with(&joined) {
        1
    } else if words.windows(terms.len().max(1)).any(|run| {
        run.iter()
            .zip(terms)
            .all(|(word, term)| *word == normalise(term))
    }) {
        2
    } else if terms
        .iter()
        .all(|term| words.contains(&normalise(term).as_str()))
    {
        3
    } else {
        4
    }
}

/// Every model in `served`, carrying whatever the gateway has measured for
/// it. Strongest first, unmeasured last by name.
#[must_use]
pub fn roster(gateway: &Gateway, served: &[String]) -> Vec<RosterModel> {
    measure(served, &published(gateway))
}

/// [`roster`] with the figures already in hand -- the seam the tests drive,
/// and the only place the ordering is decided.
#[must_use]
pub fn measure(served: &[String], published: &BTreeMap<String, MeasuredFacts>) -> Vec<RosterModel> {
    let mut models: Vec<RosterModel> = served
        .iter()
        .map(|id| {
            let facts = published.get(&normalise(id));
            RosterModel {
                id: id.clone(),
                intelligence: facts.and_then(|facts| facts.intelligence),
                coding: facts.and_then(|facts| facts.coding),
            }
        })
        .collect();
    // Strongest first; a model nobody measured sorts after every measured
    // one rather than as a zero, because unmeasured is not weak.
    models.sort_by(|a, b| match (a.intelligence, b.intelligence) {
        (Some(x), Some(y)) => y
            .partial_cmp(&x)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.id.cmp(&b.id)),
        (Some(_), None) => std::cmp::Ordering::Less,
        (None, Some(_)) => std::cmp::Ordering::Greater,
        (None, None) => a.id.cmp(&b.id),
    });
    models
}

/// The figures the roster reads, and the two limits a session reads.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct MeasuredFacts {
    pub intelligence: Option<f64>,
    pub coding: Option<f64>,
    /// The model's context window as a catalogue published it -- a prior,
    /// true of the model as described, not necessarily of the route in use.
    pub context_window_tokens: Option<u64>,
    /// The window the gateway watched a route actually enforce, when a
    /// provider has refused an over-long request and said so. Outranks the
    /// published figure because it is a measurement of the thing itself.
    pub observed_context_window_tokens: Option<u64>,
    /// The window the account's own provider says this plan is served with.
    pub served_context_window_tokens: Option<u64>,
    /// The most it may produce in one response, when the gateway knows it --
    /// the account's own figure where there is one.
    pub max_output_tokens: Option<u64>,
}

/// The two figures a session cannot choose for itself.
///
/// **Absent is the honest answer, not a default.** A window Sterna guessed is
/// worse than a window it admits it does not know: compaction would run
/// against a number nobody supplied, and the person would read it as fact.
/// Every caller therefore treats `None` as "say so" rather than as a cue to
/// substitute something.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ModelLimits {
    /// How much context the model accepts, as a catalogue published it.
    pub context_window_tokens: Option<u64>,
    /// How much the route in use was watched accepting.
    pub observed_context_window_tokens: Option<u64>,
    /// What the account's own provider says this plan is served with.
    pub served_context_window_tokens: Option<u64>,
    /// The most it may produce in one response.
    pub max_output_tokens: Option<u64>,
}

/// What the gateway published, remembered once for the whole process.
///
/// A `OnceLock` rather than a value threaded through every caller because the
/// readers are a request builder (`wire`) and the context meter, neither of
/// which has the session in hand, and because the answer cannot change inside
/// one process: the gateway is asked at startup and a model the person
/// switches to later was in the same document.
static PUBLISHED: OnceLock<BTreeMap<String, ModelLimits>> = OnceLock::new();

/// Remembers `published` for [`limits_for`]. The first call wins; a second is
/// ignored rather than refused, because two sessions in one process (the
/// tests) must not fight over it.
pub fn remember(published: &BTreeMap<String, MeasuredFacts>) {
    let _ = PUBLISHED.set(
        published
            .iter()
            .map(|(id, facts)| {
                (
                    id.clone(),
                    ModelLimits {
                        context_window_tokens: facts.context_window_tokens,
                        observed_context_window_tokens: facts.observed_context_window_tokens,
                        served_context_window_tokens: facts.served_context_window_tokens,
                        max_output_tokens: facts.max_output_tokens,
                    },
                )
            })
            .collect(),
    );
}

/// Where a window came from, which decides how much the screen may claim for
/// it.
///
/// **A percentage is a claim.** Drawn against a figure nobody measured, it
/// tells the person how much room is left with a confidence the number does
/// not have -- and being wrong about that is worse than admitting the figure
/// came from a table (`archive/glasshouse:docs/product/design-decisions.md`, *A context window
/// is a property of the route, not of the model*).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum WindowSource {
    /// The person said so on the command line. Trusted: they know what their
    /// route does, and they may be behind a proxy that narrows it.
    Configured,
    /// A provider refused an over-long request on this route and named the
    /// limit. Trusted: the route answering for itself.
    Observed,
    /// The subscription account's own provider listed it for this plan.
    /// Trusted: the provider describing what it serves this login.
    Served,
    /// A catalogue published it for the model. An estimate: a re-host caps
    /// what it resells, and a subscription tier can narrow it again.
    Published,
    /// Nobody knows.
    #[default]
    Unknown,
}

impl WindowSource {
    /// Whether a percentage drawn against this figure is a measurement rather
    /// than a guess.
    #[must_use]
    pub fn is_trusted(self) -> bool {
        matches!(self, Self::Configured | Self::Observed | Self::Served)
    }
}

/// The window a session reports and compacts against, and where it came from.
///
/// Precedence, most authoritative first: what the person configured with
/// `--context-window-tokens`; then what a provider was watched enforcing on
/// this route; then what the subscription account's provider lists for its
/// plan; then what a catalogue published for the model; then nothing,
/// which is printed as the count alone rather than filled in.
///
/// The person's own figure wins because they may be running behind a proxy
/// that narrows it, and no other source can know that. An observation beats a
/// published figure because a refusal is the route describing itself, while a
/// catalogue describes a model -- and of 223 exact name matches between two
/// published catalogues on 2026-09-17, 126 disagreed across providers.
#[must_use]
pub fn window_with_source(
    configured: Option<u64>,
    published: ModelLimits,
) -> (Option<u64>, WindowSource) {
    if let Some(configured) = configured {
        return (Some(configured), WindowSource::Configured);
    }
    if let Some(observed) = published.observed_context_window_tokens {
        return (Some(observed), WindowSource::Observed);
    }
    if let Some(served) = published.served_context_window_tokens {
        return (Some(served), WindowSource::Served);
    }
    match published.context_window_tokens {
        Some(published) => (Some(published), WindowSource::Published),
        None => (None, WindowSource::Unknown),
    }
}

/// [`window_with_source`]'s figure alone, for a caller that only has to fit a
/// request inside the window rather than describe it to a person.
#[must_use]
pub fn window_from(configured: Option<u64>, published: ModelLimits) -> Option<u64> {
    window_with_source(configured, published).0
}

/// [`window_from`] for the common caller: the model's name, and whatever the
/// session was told on the command line.
#[must_use]
pub fn window_for(model: &str, configured: Option<u64>) -> Option<u64> {
    window_from(configured, limits_for(model))
}

/// [`window_for`] with the provenance the meter needs.
#[must_use]
pub fn window_source_for(model: &str, configured: Option<u64>) -> (Option<u64>, WindowSource) {
    window_with_source(configured, limits_for(model))
}

/// What the gateway said bounds `model`, or an empty answer when nothing was
/// published for it -- including when no gateway was ever asked.
#[must_use]
pub fn limits_for(model: &str) -> ModelLimits {
    PUBLISHED
        .get()
        .and_then(|published| published.get(&normalise(model)).copied())
        .unwrap_or_default()
}

/// The gateway's measurements, keyed by [`normalise`]d name; empty whenever
/// the gateway cannot answer.
#[must_use]
pub fn published(gateway: &Gateway) -> BTreeMap<String, MeasuredFacts> {
    let Some(bytes) = gateway.run(&["models", "--json"], None) else {
        return BTreeMap::new();
    };
    let Ok(document) = serde_json::from_slice::<Document>(&bytes) else {
        return BTreeMap::new();
    };
    let observed = document.observed;
    let served = document.served;
    let mut measured: BTreeMap<String, MeasuredFacts> = document
        .models
        .into_iter()
        .map(|(id, facts)| {
            let id = normalise(&id);
            let seen = observed
                .get(&id)
                .and_then(|seen| seen.context_window_tokens);
            (
                id,
                MeasuredFacts {
                    intelligence: facts.intelligence,
                    coding: facts.coding,
                    context_window_tokens: facts.context_window_tokens,
                    observed_context_window_tokens: seen,
                    served_context_window_tokens: None,
                    max_output_tokens: facts.max_output_tokens,
                },
            )
        })
        .collect();
    // A route can be watched enforcing a window for a model no catalogue
    // measured. That is still the most authoritative figure there is for it,
    // so it is kept rather than dropped for want of a published sibling.
    for (id, seen) in observed {
        let Some(tokens) = seen.context_window_tokens else {
            continue;
        };
        measured
            .entry(normalise(&id))
            .or_default()
            .observed_context_window_tokens = Some(tokens);
    }
    // The account's own figures, for models the index measured and for the
    // many it has not caught up with yet.
    for (id, facts) in served {
        let entry = measured.entry(normalise(&id)).or_default();
        entry.served_context_window_tokens = facts.context_window_tokens;
        if facts.max_output_tokens.is_some() {
            entry.max_output_tokens = facts.max_output_tokens;
        }
    }
    measured
}

#[cfg(test)]
mod tests {
    use super::*;

    fn facts(intelligence: Option<f64>, coding: Option<f64>) -> MeasuredFacts {
        MeasuredFacts {
            intelligence,
            coding,
            ..Default::default()
        }
    }

    #[test]
    fn a_served_model_carries_the_published_figures_for_its_normalised_name() {
        let published =
            BTreeMap::from([("gpt-5-6-sol".to_string(), facts(Some(47.1), Some(77.4)))]);
        let roster = measure(&["gpt-5.6-sol".to_string()], &published);
        assert_eq!(roster[0].intelligence, Some(47.1));
        assert_eq!(roster[0].coding, Some(77.4));
        assert_eq!(roster[0].id, "gpt-5.6-sol", "the served spelling is kept");
    }

    #[test]
    fn the_strongest_leads_and_an_unmeasured_model_is_listed_last_not_as_a_zero() {
        let published = BTreeMap::from([
            ("weak".to_string(), facts(Some(10.0), None)),
            ("strong".to_string(), facts(Some(50.0), None)),
        ]);
        let roster = measure(
            &[
                "weak".to_string(),
                "unmeasured".to_string(),
                "strong".to_string(),
            ],
            &published,
        );
        let order: Vec<&str> = roster.iter().map(|model| model.id.as_str()).collect();
        assert_eq!(order, ["strong", "weak", "unmeasured"]);
        assert_eq!(roster[2].intelligence, None);
    }

    #[test]
    fn nothing_published_still_lists_every_served_model() {
        let roster = measure(&["a".to_string(), "b".to_string()], &BTreeMap::new());
        assert_eq!(roster.len(), 2);
        assert!(roster.iter().all(|model| model.intelligence.is_none()));
    }

    #[test]
    fn an_unreachable_gateway_publishes_nothing_rather_than_failing() {
        let absent = Gateway::Command {
            gateway: std::path::PathBuf::from("/nonexistent/inference-gateway"),
        };
        assert!(published(&absent).is_empty());
    }

    #[test]
    fn a_published_document_carries_the_two_limits_a_session_cannot_choose() {
        let document = br#"{"models":{"gpt-5.6-sol":{"intelligence":47.1,"context_window_tokens":400000,"max_output_tokens":128000},"quiet":{"coding":1.0}}}"#;
        let parsed: Document = serde_json::from_slice(document).unwrap();
        let facts = &parsed.models["gpt-5.6-sol"];
        assert_eq!(facts.context_window_tokens, Some(400_000));
        assert_eq!(facts.max_output_tokens, Some(128_000));
        let quiet = &parsed.models["quiet"];
        assert_eq!(quiet.context_window_tokens, None, "absent stays absent");
        assert_eq!(quiet.max_output_tokens, None);
    }

    #[test]
    fn an_unremembered_model_has_no_limits_rather_than_invented_ones() {
        // `remember` may already hold another test's map; either way a model
        // nobody published must answer with absences.
        assert_eq!(
            limits_for("a-model-nobody-published"),
            ModelLimits::default()
        );
    }

    #[test]
    fn a_window_the_route_was_watched_enforcing_outranks_the_one_a_catalogue_published() {
        // The measurement behind this: of 223 exact name matches between two
        // published catalogues on 2026-09-17, 126 disagreed across providers,
        // because a re-host caps what it resells. The refusal is the route
        // describing itself.
        let limits = ModelLimits {
            context_window_tokens: Some(1_000_000),
            observed_context_window_tokens: Some(200_000),
            served_context_window_tokens: None,
            max_output_tokens: None,
        };
        assert_eq!(
            window_with_source(None, limits),
            (Some(200_000), WindowSource::Observed),
            "the re-host's own refusal beats the vendor's published figure"
        );
        assert_eq!(
            window_with_source(Some(64_000), limits),
            (Some(64_000), WindowSource::Configured),
            "the person may be behind a proxy narrower than either"
        );
    }

    #[test]
    fn the_window_an_accounts_provider_lists_for_its_plan_outranks_the_published_one() {
        // The published index predates gpt-6-sol and says nothing; a Max and
        // a Pro login see different windows for one model. The account's own
        // list is the plan describing itself -- only a refusal outranks it.
        let document = br#"{"models":{"claude-opus-5-5":{"context_window_tokens":200000,"max_output_tokens":64000}},"served":{"claude-opus-5-5":{"context_window_tokens":1000000,"max_output_tokens":128000,"account":"claude-max","fetched_at_unix":1789000000},"gpt-6-sol":{"context_window_tokens":272000,"account":"chatgpt","fetched_at_unix":1789000000}}}"#;
        let parsed: Document = serde_json::from_slice(document).unwrap();
        assert_eq!(
            parsed.served["gpt-6-sol"].context_window_tokens,
            Some(272_000)
        );
        let limits = ModelLimits {
            context_window_tokens: Some(200_000),
            observed_context_window_tokens: None,
            served_context_window_tokens: Some(1_000_000),
            max_output_tokens: None,
        };
        assert_eq!(
            window_with_source(None, limits),
            (Some(1_000_000), WindowSource::Served)
        );
        assert_eq!(
            window_with_source(
                None,
                ModelLimits {
                    observed_context_window_tokens: Some(900_000),
                    ..limits
                }
            ),
            (Some(900_000), WindowSource::Observed),
            "a refusal is the route answering for itself"
        );
        assert!(WindowSource::Served.is_trusted());
    }

    #[test]
    fn only_a_measured_window_is_trusted_enough_for_a_percentage() {
        assert!(WindowSource::Configured.is_trusted());
        assert!(WindowSource::Observed.is_trusted());
        assert!(
            !WindowSource::Published.is_trusted(),
            "a catalogue describes the model, not the route serving it"
        );
        assert!(!WindowSource::Unknown.is_trusted());
    }

    #[test]
    fn an_observed_window_is_read_from_the_gateways_own_document() {
        let document = br#"{"models":{"gpt-5.6-sol":{"intelligence":47.1,"context_window_tokens":922000}},"observed":{"gpt-5-6-sol":{"context_window_tokens":128000,"route":"openrouter","observed_at_unix":1789000000}}}"#;
        let parsed: Document = serde_json::from_slice(document).unwrap();
        assert_eq!(
            parsed.observed["gpt-5-6-sol"].context_window_tokens,
            Some(128_000)
        );
        // An older gateway says nothing about observations, and that must
        // parse as "none observed" rather than as a failure that loses the
        // published figures too.
        let older: Document =
            serde_json::from_slice(br#"{"models":{"m":{"coding":1.0}}}"#).unwrap();
        assert!(older.observed.is_empty());
        assert!(older.models.contains_key("m"));
    }

    #[test]
    fn a_published_window_is_used_and_the_persons_own_figure_still_wins() {
        let published = ModelLimits {
            context_window_tokens: Some(400_000),
            observed_context_window_tokens: None,
            served_context_window_tokens: None,
            max_output_tokens: None,
        };
        assert_eq!(window_from(None, published), Some(400_000));
        assert_eq!(
            window_from(Some(64_000), published),
            Some(64_000),
            "the person may be behind a proxy that narrows it"
        );
        assert_eq!(
            window_from(None, ModelLimits::default()),
            None,
            "unknown stays unknown"
        );
    }
}
