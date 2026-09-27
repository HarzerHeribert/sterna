//! Every result the ABI returns says which of three things it is, and a
//! weaker one can never stand in for a stronger claim.
//!
//! `tool-abi.md` §11 fixes the three classes and §21 fixes their authority
//! order. The class is decided from mechanical facts about what was observed
//! — bytes returned against bytes present, a helper having run or not — and
//! never from a description of the result.

use serde::{Deserialize, Serialize};

/// What a returned value is, relative to the observation behind it.
///
/// Deliberately not a `bool` pair and not `Option<Reducer>`: `tool-abi.md`
/// §11 needs three states, and the middle one — an exact subset whose
/// remainder is still addressable — is the one a two-state type loses.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceClass {
    /// The content is the complete observation within the requested scope.
    Exact,
    /// The content is an exact subset of a larger observation whose
    /// remainder stays reachable through a handle or continuation.
    BoundedExact,
    /// A semantic transformation ran. Any Little Helper output is this,
    /// whatever its quality.
    Derived,
}

impl EvidenceClass {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Exact => "exact",
            Self::BoundedExact => "bounded_exact",
            Self::Derived => "derived",
        }
    }

    /// Whether a result of this class may substantiate a claim about complete
    /// exact content on its own — `tool-abi.md` §11's closing rule and
    /// acceptance criterion 23.
    ///
    /// `BoundedExact` is false because a subset cannot establish a property
    /// of the whole, even though every byte in it was observed.
    #[must_use]
    pub fn substantiates_exact_claim(self) -> bool {
        matches!(self, Self::Exact)
    }

    /// Whether the class carries model-generated interpretation, which the
    /// trust order in §21 places below every deterministic observation.
    #[must_use]
    pub fn is_derived(self) -> bool {
        matches!(self, Self::Derived)
    }
}

impl std::fmt::Display for EvidenceClass {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// The provenance an ABI result carries beside its content.
///
/// The invariant: whenever `class` is not `Exact`, `handle` names something
/// the model can still reach. `tool-abi.md` §12 requires the complete
/// observation to stay addressable, so a bounded or derived result without a
/// handle is a bug this struct makes visible rather than a shape it allows.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Provenance {
    pub class: EvidenceClass,
    /// The live handle holding the complete observation, when one exists.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub handle: Option<String>,
    /// Bytes in the complete observation, when mechanically known.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exact_bytes: Option<u64>,
    /// The reducer or helper that produced a `Derived` view.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reducer: Option<String>,
}

impl Provenance {
    /// A complete observation returned whole.
    #[must_use]
    pub fn exact(handle: impl Into<String>) -> Self {
        Self {
            class: EvidenceClass::Exact,
            handle: Some(handle.into()),
            exact_bytes: None,
            reducer: None,
        }
    }

    /// An exact subset, with the whole still reachable through `handle`.
    #[must_use]
    pub fn bounded(handle: impl Into<String>, exact_bytes: u64) -> Self {
        Self {
            class: EvidenceClass::BoundedExact,
            handle: Some(handle.into()),
            exact_bytes: Some(exact_bytes),
            reducer: None,
        }
    }

    /// A helper or reducer interpretation of the observation in `handle`.
    #[must_use]
    pub fn derived(handle: impl Into<String>, reducer: impl Into<String>) -> Self {
        Self {
            class: EvidenceClass::Derived,
            handle: Some(handle.into()),
            exact_bytes: None,
            reducer: Some(reducer.into()),
        }
    }

    /// Whether this provenance keeps the complete observation reachable, as
    /// §12 requires of everything that is not already complete.
    #[must_use]
    pub fn keeps_exact_reachable(&self) -> bool {
        self.class == EvidenceClass::Exact || self.handle.is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_exact_substantiates_an_exact_content_claim() {
        assert!(EvidenceClass::Exact.substantiates_exact_claim());
        assert!(!EvidenceClass::BoundedExact.substantiates_exact_claim());
        assert!(!EvidenceClass::Derived.substantiates_exact_claim());
    }

    #[test]
    fn a_helper_view_is_derived_and_names_its_reducer() {
        let provenance = Provenance::derived("tests_7", "test-log-reducer-v2");
        assert!(provenance.class.is_derived());
        assert_eq!(provenance.reducer.as_deref(), Some("test-log-reducer-v2"));
        assert!(provenance.keeps_exact_reachable());
    }

    #[test]
    fn a_bounded_result_keeps_the_whole_addressable() {
        let provenance = Provenance::bounded("log_3", 18_422_931);
        assert_eq!(provenance.class, EvidenceClass::BoundedExact);
        assert_eq!(provenance.exact_bytes, Some(18_422_931));
        assert!(provenance.keeps_exact_reachable());
    }

    #[test]
    fn the_three_classes_serialise_as_the_spec_spells_them() {
        assert_eq!(EvidenceClass::Exact.as_str(), "exact");
        assert_eq!(EvidenceClass::BoundedExact.as_str(), "bounded_exact");
        assert_eq!(EvidenceClass::Derived.as_str(), "derived");
    }
}
