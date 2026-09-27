//! Wire-level vocabulary: what a harness or provider speaks over the
//! wire, and whether a fact about it is verified or merely declared.
//! Moved out of `harness/mod.rs` (Cut 6) because these describe the
//! wire, not a harness, and the standalone `inference-gateway` crate
//! needs them without importing `crate::harness`.

/// A fact about a harness, and where it came from.
///
/// `Verified` carries the evidence string so a diagnostic can show *why*
/// Glasshouse believes something — "because `--chrome` is in its `--help`" is
/// an answer a user can check, and "because Glasshouse says so" is not.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Declared<T> {
    /// Established from the installed harness itself. `evidence` names the
    /// source concretely enough to re-check: a `--help` line, a configuration
    /// file, an on-disk session record.
    Verified { value: T, evidence: &'static str },
    /// Nothing available in this environment established it. Not "no", and
    /// never a guess — see the module documentation.
    Unverified,
}

impl<T> Declared<T> {
    /// Declare `value`, citing `evidence`.
    pub const fn verified(value: T, evidence: &'static str) -> Self {
        Self::Verified { value, evidence }
    }

    pub fn value(&self) -> Option<&T> {
        match self {
            Self::Verified { value, .. } => Some(value),
            Self::Unverified => None,
        }
    }

    pub fn evidence(&self) -> Option<&'static str> {
        match self {
            Self::Verified { evidence, .. } => Some(evidence),
            Self::Unverified => None,
        }
    }

    pub fn is_verified(&self) -> bool {
        matches!(self, Self::Verified { .. })
    }
}

impl Declared<bool> {
    /// Whether the harness is known to have the capability.
    ///
    /// `Unverified` reads as `false` here, which is the safe direction: a
    /// caller asking "may I rely on this" must be told no when nobody has
    /// checked. Callers that need to distinguish "verified absent" from "not
    /// checked" match on the variant instead.
    pub fn is_known_present(&self) -> bool {
        matches!(self, Self::Verified { value: true, .. })
    }
}

/// A backend wire protocol, in the vocabulary Phase 9C fixes for provider
/// compatibility. Kept identical on purpose: a protocol a harness speaks and a
/// protocol a provider serves have to be comparable without a translation
/// table between two spellings of the same idea.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WireProtocol {
    AnthropicMessages,
    OpenAiResponses,
    OpenAiChat,
    /// Google's Generative Language API — `generateContent` and
    /// `streamGenerateContent` under `…/v1beta/models/<model>:<method>`,
    /// credentialled with `x-goog-api-key`.
    ///
    /// Added by Phase 56's T3 package for the *provider* side: the gateway
    /// translates a harness protocol **to** it. No installed harness speaks
    /// it at the ingress yet — the Gemini CLI adapter is T3b — so every
    /// `gemini-generate-content -> …` row in the gateway's pair table is
    /// refused by name rather than merely untested.
    GeminiGenerateContent,
    /// TypeSafe AI's System One decision protocol — `POST …/v1/systemone`,
    /// a typed-question request and a typed-answer response with its own
    /// `usage` shape. Relay-only: a decision request has no messages, tools
    /// or stream to translate, so no codec exists or may be inferred for it
    /// (nothing in `translate/` names it, and the pair table refuses every
    /// pair with it by name). This holds as long as System One stays a
    /// decision wire rather than a chat wire — see
    /// `archive/glasshouse:docs/product/evidence/phase-66.md`, *Provider facts*.
    TypesafeSystemOne,
}

impl WireProtocol {
    pub fn slug(self) -> &'static str {
        match self {
            WireProtocol::AnthropicMessages => "anthropic-messages",
            WireProtocol::OpenAiResponses => "openai-responses",
            WireProtocol::OpenAiChat => "openai-chat",
            WireProtocol::GeminiGenerateContent => "gemini-generate-content",
            WireProtocol::TypesafeSystemOne => "typesafe-systemone",
        }
    }
}

impl std::fmt::Display for WireProtocol {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.pad(self.slug())
    }
}
