//! What each tier of the session actually consumed, and on what.
//!
//! A session is not one model spending tokens. It is a parent that reasons, a
//! helper that absorbs bulk and a subagent that is delegated to, and those can
//! be three different models on three different routes at three prices. A
//! single `spent 278.6k` figure hides all of that: it looks worse than
//! `60k` until you know that 235k of it went to a model costing eighteen times
//! less and never entered the parent's context at all.
//!
//! So the unit here is the **tier**, not the token. Every figure is optional
//! and absent is never zero, because these come from a provider's own usage
//! rows after the fact and a request nobody metered must not render as free.

use std::collections::BTreeMap;

/// Which part of the session spent something.
///
/// Three, and no "other": work that belongs to none of these is work whose
/// cost nobody owns, and the right response to discovering some is to name a
/// fourth tier rather than to hide it in a total.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Tier {
    /// The model the person is talking to.
    Parent,
    /// Bounded, out-of-turn semantic work owned by the cell that asked.
    Helpers,
    /// Delegated goals running as session-background work.
    Subagents,
}

impl Tier {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Parent => "parent",
            Self::Helpers => "helpers",
            Self::Subagents => "subagents",
        }
    }

    /// The singular form, for a sentence about one assignment.
    #[must_use]
    pub fn singular(self) -> &'static str {
        match self {
            Self::Parent => "parent",
            Self::Helpers => "helper",
            Self::Subagents => "subagent",
        }
    }

    /// The tier a person names, in either spelling.
    ///
    /// **This is the same enum the ledger reports**, which is the point:
    /// `/model helper luna` and the `helpers` heading in the breakdown must
    /// be the same tier, or configuring the ladder and reading its bill are
    /// two vocabularies a person has to hold at once.
    #[must_use]
    pub fn parse(word: &str) -> Option<Self> {
        match word {
            "parent" => Some(Self::Parent),
            "helper" | "helpers" => Some(Self::Helpers),
            "subagent" | "subagents" => Some(Self::Subagents),
            _ => None,
        }
    }

    /// The three, in the order work flows through them.
    #[must_use]
    pub fn every() -> [Self; 3] {
        [Self::Parent, Self::Helpers, Self::Subagents]
    }

    /// The next tier, wrapping — what Tab does in the model panel.
    #[must_use]
    pub fn next(self) -> Self {
        match self {
            Self::Parent => Self::Helpers,
            Self::Helpers => Self::Subagents,
            Self::Subagents => Self::Parent,
        }
    }
}

/// One tier's consumption, on one model.
///
/// Keyed by model rather than summed across the tier, because "helpers used
/// 235k" is not a fact anybody can price until it says which helper model.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Consumption {
    /// The route that served it — the entitlement, as the gateway reported
    /// it. Absent when the request went direct, or when nothing said.
    pub route: Option<String>,
    /// Requests for a parent or a helper; turns for a subagent.
    pub calls: u64,
    /// Every token class the provider reported, summed. Absent when no
    /// request in this tier reported any.
    pub tokens: Option<u64>,
    /// Reported cost, never inferred from a price list this crate does not
    /// have.
    pub cost_usd: Option<f64>,
    /// Requests whose usage the provider did not report, so a reader can tell
    /// a small number from an incomplete one.
    pub unreported_calls: u64,
}

impl Consumption {
    /// Adds one request's reported usage.
    fn record(&mut self, tokens: Option<u64>, cost: Option<f64>, route: Option<&str>) {
        self.calls += 1;
        match tokens {
            Some(count) => *self.tokens.get_or_insert(0) += count,
            None => self.unreported_calls += 1,
        }
        if let Some(cost) = cost {
            *self.cost_usd.get_or_insert(0.0) += cost;
        }
        if self.route.is_none()
            && let Some(route) = route
        {
            self.route = Some(route.to_string());
        }
    }

    /// Whether every request in this tier reported its usage.
    #[must_use]
    pub fn is_complete(&self) -> bool {
        self.unreported_calls == 0
    }
}

/// Every tier's spend, by model.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Ledger {
    tiers: BTreeMap<(Tier, String), Consumption>,
}

impl Ledger {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Records one request against a tier and a model.
    pub fn record(
        &mut self,
        tier: Tier,
        model: &str,
        tokens: Option<u64>,
        cost_usd: Option<f64>,
        route: Option<&str>,
    ) {
        self.tiers
            .entry((tier, model.to_string()))
            .or_default()
            .record(tokens, cost_usd, route);
    }

    /// Every entry, parent first, then helpers, then subagents.
    #[must_use]
    pub fn entries(&self) -> Vec<(Tier, &str, &Consumption)> {
        self.tiers
            .iter()
            .map(|((tier, model), used)| (*tier, model.as_str(), used))
            .collect()
    }

    /// The whole session's tokens.
    ///
    /// Deliberately last and deliberately awkward to reach: a total across
    /// tiers is the figure this module exists to stop people quoting on its
    /// own, and it is only meaningful beside the breakdown.
    #[must_use]
    pub fn total_tokens(&self) -> Option<u64> {
        let mut total = None;
        for used in self.tiers.values() {
            if let Some(tokens) = used.tokens {
                *total.get_or_insert(0) += tokens;
            }
        }
        total
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.tiers.is_empty()
    }

    /// The breakdown, as a person reads it.
    ///
    /// Model and route on every line, because the whole point is that a token
    /// spent by one model on one route is not the same thing as a token spent
    /// by another.
    #[must_use]
    pub fn render(&self) -> String {
        if self.is_empty() {
            return "no metered work yet".to_string();
        }
        let mut out = String::new();
        let mut current: Option<Tier> = None;
        for ((tier, model), used) in &self.tiers {
            if current != Some(*tier) {
                if current.is_some() {
                    out.push('\n');
                }
                out.push_str(tier.as_str());
                out.push('\n');
                current = Some(*tier);
            }
            out.push_str(&format!("  model: {model}\n"));
            out.push_str(&format!(
                "  route: {}\n",
                used.route.as_deref().unwrap_or("unknown")
            ));
            let unit = if *tier == Tier::Subagents {
                "turns"
            } else {
                "calls"
            };
            out.push_str(&format!("  {unit}: {}\n", used.calls));
            match used.tokens {
                Some(tokens) if used.is_complete() => {
                    out.push_str(&format!("  tokens: {tokens}\n"));
                }
                Some(tokens) => out.push_str(&format!(
                    "  tokens: {tokens} (partial; {} call(s) unreported)\n",
                    used.unreported_calls
                )),
                None => out.push_str("  tokens: unreported\n"),
            }
            match used.cost_usd {
                Some(cost) => out.push_str(&format!("  cost: ${cost:.4}\n")),
                None => out.push_str("  cost: unreported\n"),
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn heterogeneous() -> Ledger {
        let mut ledger = Ledger::new();
        ledger.record(
            Tier::Parent,
            "gpt-6-astra",
            Some(43_100),
            None,
            Some("chatgpt-subscription"),
        );
        ledger.record(
            Tier::Helpers,
            "gpt-5.6-luna",
            Some(235_500),
            None,
            Some("chatgpt-subscription"),
        );
        ledger.record(
            Tier::Subagents,
            "claude-sonnet-5",
            Some(12_000),
            None,
            Some("claude-max"),
        );
        ledger
    }

    /// Configuring a tier and reading its bill are one vocabulary.
    ///
    /// `/model helper luna` and the `helpers` heading below must name the
    /// same tier, or a person has to hold two spellings at once to connect
    /// what they chose to what it cost.
    #[test]
    fn a_tier_parses_from_either_spelling_it_is_written_in() {
        for tier in Tier::every() {
            assert_eq!(Tier::parse(tier.as_str()), Some(tier));
            assert_eq!(Tier::parse(tier.singular()), Some(tier));
        }
        assert_eq!(Tier::parse("frontier"), None);
        assert_eq!(Tier::parse(""), None);
    }

    #[test]
    fn cycling_visits_all_three_and_returns() {
        let mut seen = vec![Tier::Parent];
        for _ in 0..3 {
            seen.push(seen.last().expect("seeded").next());
        }
        assert_eq!(
            seen,
            [Tier::Parent, Tier::Helpers, Tier::Subagents, Tier::Parent]
        );
    }

    /// The reason the module exists: the same total means different things
    /// depending on which model spent it.
    #[test]
    fn each_tier_names_its_own_model_and_route() {
        let rendered = heterogeneous().render();
        assert!(rendered.contains("parent\n  model: gpt-6-astra"));
        assert!(rendered.contains("helpers\n  model: gpt-5.6-luna"));
        assert!(rendered.contains("subagents\n  model: claude-sonnet-5"));
        assert!(rendered.contains("route: chatgpt-subscription"));
        assert!(rendered.contains("route: claude-max"));
    }

    /// A subagent is measured in turns; a parent and a helper in calls.
    #[test]
    fn a_subagent_is_counted_in_turns_rather_than_calls() {
        let rendered = heterogeneous().render();
        assert!(rendered.contains("turns: 1"));
        assert!(rendered.contains("calls: 1"));
    }

    /// Absent is never zero: a request nobody metered must not render as a
    /// free one, and the reader must be able to see that the figure is short.
    #[test]
    fn an_unreported_request_is_visible_rather_than_counted_as_free() {
        let mut ledger = Ledger::new();
        ledger.record(Tier::Helpers, "luna", Some(100), None, None);
        ledger.record(Tier::Helpers, "luna", None, None, None);
        let rendered = ledger.render();
        assert!(rendered.contains("partial"), "{rendered}");
        assert!(rendered.contains("1 call(s) unreported"), "{rendered}");
    }

    #[test]
    fn a_tier_nobody_metered_says_so_rather_than_showing_a_zero() {
        let mut ledger = Ledger::new();
        ledger.record(Tier::Parent, "astra", None, None, None);
        let rendered = ledger.render();
        assert!(rendered.contains("tokens: unreported"), "{rendered}");
        assert!(rendered.contains("cost: unreported"), "{rendered}");
        assert_eq!(ledger.total_tokens(), None);
    }

    /// Cost is reported or absent, never inferred: this crate holds no price
    /// list and a guessed dollar figure is worse than none.
    #[test]
    fn cost_is_only_ever_what_was_reported() {
        let mut ledger = Ledger::new();
        ledger.record(Tier::Parent, "astra", Some(10), Some(0.25), None);
        ledger.record(Tier::Parent, "astra", Some(10), None, None);
        let rendered = ledger.render();
        assert!(rendered.contains("cost: $0.2500"), "{rendered}");
    }

    /// The route is what makes two identical token counts different work.
    #[test]
    fn an_unknown_route_says_unknown_rather_than_guessing() {
        let mut ledger = Ledger::new();
        ledger.record(Tier::Parent, "astra", Some(1), None, None);
        assert!(ledger.render().contains("route: unknown"));
    }

    #[test]
    fn an_empty_ledger_claims_nothing() {
        assert!(Ledger::new().is_empty());
        assert_eq!(Ledger::new().total_tokens(), None);
        assert_eq!(Ledger::new().render(), "no metered work yet");
    }

    /// Tiers render in the order the work happens in, so the expensive one is
    /// read first.
    #[test]
    fn the_parent_is_reported_before_what_it_delegated_to() {
        let rendered = heterogeneous().render();
        let parent = rendered.find("parent").unwrap();
        let helpers = rendered.find("helpers").unwrap();
        let subagents = rendered.find("subagents").unwrap();
        assert!(parent < helpers && helpers < subagents, "{rendered}");
    }
}
