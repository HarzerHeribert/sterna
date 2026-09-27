//! One place that enumerates every kind of model resource this gateway can
//! describe, honest about the fact that their quotas do not work the same way.
//!
//! The kinds themselves already ship: a client's own native subscription and
//! a direct provider are both resolved for every launch, and the concrete
//! providers are [`crate::provider::templates`]. This module adds the one
//! thing neither states on its own: **which quota shape each entry actually
//! has**. That is [`Locality`] and [`QuotaModel`].
//!
//! # It is told which clients exist; it does not know what one is
//!
//! [`registry`] takes its native subscriptions from the caller as
//! [`NativeClient`]s — an opaque slug and the caller's own display name — for
//! the reason `crate::routing::interactive::Assignment` carries a harness
//! slug rather than a harness type: the gateway serves clients and must not
//! reproduce their reasoning. Nothing here parses a slug or branches on one.
//!
//! It adds no network call — every entry is built from
//! [`crate::provider::templates`] and the caller's client list, both static.
//! It does not read or hold a credential: [`ResourceKind::DirectProvider`]
//! carries a provider *name*. And it does not track live quota telemetry;
//! [`QuotaModel`] names the *shape* a resource's quota takes, not its state.
// History: design-decisions.md, "Trims: provider module docs", registry.rs module doc.

use crate::provider;

/// Whether a resource's compute runs on this machine or somewhere else.
///
/// Capability map line 1185: local inference must be represented separately
/// from remote resources, and this is the field that does it — a
/// [`ResourceKind::DirectProvider`] carries one, and it is looked up in this
/// module's `LOCAL_INFERENCE_PROVIDERS` table rather than guessed at from a
/// name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Locality {
    /// Served on this machine — Ollama and llama.cpp today.
    Local,
    /// Reached over the network — every configured router, gateway or
    /// metered API.
    Remote,
}

/// How a resource's capacity is known to run out, if it can at all.
///
/// This is the phase's fixed requirement in code: a registry that flattened
/// every kind of capacity to one "it has capacity or it does not" boolean
/// would have satisfied [`ResourceKind`]'s existence and broken the
/// requirement in the same motion. Each variant names a *shape*, never a
/// number — no rolling-window reset time, no spent balance, no request
/// count. Reading the live state behind any of them is quota telemetry,
/// which Phase 32B owns and which does not exist yet.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QuotaModel {
    /// A client's own first-party subscription. Its capacity resets on a
    /// rolling window this gateway does not measure — a request count and a
    /// dollar balance are both the wrong shape for it.
    RollingWindowSubscription,
    /// An account balance, spent per request. Whether a particular model on
    /// this provider is actually billed against it, or marked free-tier
    /// instead, is a per-model fact this registry does not carry — see
    /// [`crate::routing::Cost`] and the caller's own per-provider
    /// `free_models` configuration, which already own that distinction.
    MeteredBalance,
    /// No metering at all. A local inference server cannot run out of
    /// money, and pretending it has a balance would be inventing a number
    /// nobody can read.
    Unmetered,
    /// The local gateway is a router, not a capacity of its own — its quota
    /// is whichever upstream it is currently bound to, which is a per-session
    /// fact `crate::routing::interactive::Assignment` already records. Naming
    /// that here as `MeteredBalance` would claim a shape for a resource that
    /// can, in fact, be bound to an unmetered one.
    DelegatedToUpstream,
}

/// The client one [`ResourceKind::NativeSubscription`] belongs to.
///
/// Both fields come from the caller and neither is interpreted here: the
/// gateway does not know what any particular client is, so `slug` is
/// compared and never parsed, and `display_name` is printed and never
/// matched on. The invariant is the ruling of 2026-09-10 — the gateway may
/// hold *which* client serves an account without knowing what that client
/// does — and it is the same shape
/// `crate::routing::interactive::Assignment` already uses for the harness
/// it is serving.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NativeClient {
    /// Stable, opaque identifier for the client. Compared, never parsed.
    pub slug: String,
    /// What the caller calls this client when it shows it to a person.
    /// Presentation the caller owns; this module only carries it.
    pub display_name: String,
}

impl NativeClient {
    /// A client named `slug`, displayed as `display_name`.
    pub fn new(slug: impl Into<String>, display_name: impl Into<String>) -> Self {
        Self {
            slug: slug.into(),
            display_name: display_name.into(),
        }
    }
}

/// One kind of model resource the registry can describe — independent of
/// whether any caller's launch profile is currently using it.
///
/// Deliberately distinct from whatever the caller resolves a session onto: a
/// resolved backend is *what one profile is pointed at*, launched; a
/// [`ResourceKind`] is *a kind of capacity this gateway knows how to
/// describe*, whether or not a profile exists for it yet.
/// [`ResourceKind::from_direct_provider`] is the bridge between the two, used
/// at the one place a backend is actually resolved for a session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResourceKind {
    /// A client's own first-party authentication, named by which client
    /// serves it — see [`NativeClient`] for why that is a slug.
    NativeSubscription { client: NativeClient },
    /// A provider or router reached directly by a client. Carries the
    /// provider *name*, never a credential.
    DirectProvider {
        provider: String,
        locality: Locality,
    },
    /// The local gateway itself. One entry: there is exactly one gateway
    /// process, whatever it is currently forwarding to.
    GlasshouseGateway,
    /// A configured entitlement — one `[entitlements.<name>]` entry, a
    /// specific subscription or API-credit account with its own
    /// authentication (map lines 1962 and 1963). Keyed by the **user's name
    /// for the account and by nothing else**, because several entitlements
    /// of one vendor and plan coexist as several resources.
    ///
    /// A new shape rather than a name on [`Self::NativeSubscription`],
    /// deliberately: the native variant is keyed by *client* and stands for
    /// "whatever account that client is currently signed into" — a
    /// per-client singleton that exists whether or not anything is
    /// configured — while a configured entitlement is an *account the user
    /// declared*, of which one client's vendor may have several and which
    /// need not be any client's current sign-in at all. Folding the two
    /// together would leave `claude-a` and `claude-b` unrepresentable, which
    /// is the exact inefficiency Phase 56A names.
    ///
    /// Enumerated from the caller's configuration, which this static catalog
    /// cannot see.
    Entitlement { name: String },
}

impl ResourceKind {
    /// Where this resource's compute runs.
    ///
    /// A client's own subscription always reaches a remote model, and the
    /// gateway is answered as [`Locality::Local`] for the process itself —
    /// the loopback listener a client is pointed at — which is a different
    /// question from where the upstream *it* forwards to runs; that is
    /// exactly what [`QuotaModel::DelegatedToUpstream`] exists to avoid
    /// pretending this method answers.
    pub fn locality(&self) -> Locality {
        match self {
            ResourceKind::NativeSubscription { .. } => Locality::Remote,
            ResourceKind::DirectProvider { locality, .. } => *locality,
            ResourceKind::GlasshouseGateway => Locality::Local,
            // An entitlement is an account with a vendor; whatever serves
            // it, its compute is not this machine's.
            ResourceKind::Entitlement { .. } => Locality::Remote,
        }
    }

    /// The shape of this resource's quota — see [`QuotaModel`].
    ///
    /// Projected out of [`crate::provider::quota::CapacityState`] rather
    /// than computed beside it. The shape a resource's quota takes is one
    /// fact *about* its capacity, and deriving it twice is how the two would
    /// come to disagree — a resource whose capacity model said "nothing can
    /// exhaust this" while its quota shape said `MeteredBalance` would be a
    /// bug no test of either half alone could see.
    ///
    /// It is also what puts the capacity model on the production launch path
    /// without a new caller: the launch path already reads this method for
    /// every session's `"resource kind"` mechanism note, so every launch
    /// builds a [`crate::provider::quota::CapacityState`] and reads its
    /// shape out.
    pub fn quota(&self) -> QuotaModel {
        self.capacity().model()
    }

    /// The [`ResourceKind`] a resolved direct provider's name describes.
    ///
    /// This is the one function the launch path actually calls, which is what
    /// keeps this type from being a registry nothing consults: every
    /// direct-provider session records what it resolved to.
    pub fn from_direct_provider(provider: impl Into<String>) -> Self {
        let provider = provider.into();
        let locality = locality_of(&provider);
        ResourceKind::DirectProvider { provider, locality }
    }

    /// A short, stable label for a diagnostic — the launch mechanism note
    /// and the acceptance tests both read this rather than formatting the
    /// variant by hand, so the two cannot drift.
    pub fn label(&self) -> String {
        match self {
            ResourceKind::NativeSubscription { client } => {
                format!("{} subscription", client.display_name)
            }
            ResourceKind::DirectProvider { provider, locality } => {
                format!("{provider} ({})", locality.as_str())
            }
            ResourceKind::GlasshouseGateway => "inference gateway".to_owned(),
            ResourceKind::Entitlement { name } => format!("entitlement `{name}`"),
        }
    }
}

impl Locality {
    pub fn as_str(self) -> &'static str {
        match self {
            Locality::Local => "local",
            Locality::Remote => "remote",
        }
    }
}

impl QuotaModel {
    pub fn as_str(self) -> &'static str {
        match self {
            QuotaModel::RollingWindowSubscription => "rolling-window subscription",
            QuotaModel::MeteredBalance => "metered balance",
            QuotaModel::Unmetered => "unmetered",
            QuotaModel::DelegatedToUpstream => "delegated to its assigned upstream",
        }
    }
}

/// The built-in templates whose server runs on the machine asking for it.
///
/// A table rather than a [`crate::provider::Provider`] field, for the reason
/// [`crate::provider::usage_endpoint`]'s own table gives: a new required
/// field would have to be added to every `Provider` struct literal in and
/// outside this crate, and two entries do not earn that. Exact names, never
/// a prefix — see [`locality_of`].
const LOCAL_INFERENCE_PROVIDERS: &[&str] = &["ollama", "llama-cpp"];

/// Whether `provider_name` names a local-inference server rather than a
/// remote one.
///
/// Matched by exact name against [`LOCAL_INFERENCE_PROVIDERS`] rather than
/// against a prefix or a hostname, so a user-configured provider that merely
/// happens to point its base URL at `localhost` (a self-hosted LiteLLM proxy,
/// say) is not silently reclassified as local inference: this answers "which
/// server is this", not "which address does it use today".
fn locality_of(provider_name: &str) -> Locality {
    if LOCAL_INFERENCE_PROVIDERS.contains(&provider_name) {
        Locality::Local
    } else {
        Locality::Remote
    }
}

/// Every kind of model resource this gateway can describe for a caller whose
/// native subscriptions are `native_clients`.
///
/// Capability map line 1183: this is the registry. It enumerates —
///
/// - a [`ResourceKind::NativeSubscription`] for every entry of
///   `native_clients`, in the order given. **Which clients those are is the
///   caller's fact, not this crate's** — a gateway that folded over its own
///   list of known clients would be reproducing the caller's reasoning about
///   which agents exist, which is exactly what it may not do;
/// - a [`ResourceKind::DirectProvider`] for every
///   [`crate::provider::templates`] entry, which covers OpenRouter (line
///   1189), the user-configured routers UnoRouter/AnyRouter/Kilo/Nous plus
///   the two generic templates a user's own gateway is configured through
///   (line 1190), and Ollama and llama.cpp (lines 1191, 1192) — the last two
///   distinguished from every other entry by [`Locality::Local`];
/// - one [`ResourceKind::GlasshouseGateway`].
// History: design-decisions.md, "Trims: provider module docs", registry.rs `registry` doc.
pub fn registry(native_clients: &[NativeClient]) -> Vec<ResourceKind> {
    let mut out: Vec<ResourceKind> = native_clients
        .iter()
        .map(|client| ResourceKind::NativeSubscription {
            client: client.clone(),
        })
        .collect();

    out.extend(
        provider::templates()
            .into_iter()
            .map(|provider| ResourceKind::from_direct_provider(provider.name)),
    );

    out.push(ResourceKind::GlasshouseGateway);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Deliberately not the slugs of any real client. The gateway must
    /// produce the same shape for any slug the caller hands it, and a
    /// fixture spelled `claude-code` would let a special case hide.
    fn clients() -> Vec<NativeClient> {
        vec![
            NativeClient::new("client-a", "Client A"),
            NativeClient::new("client-b", "Client B"),
        ]
    }

    fn find_direct<'a>(entries: &'a [ResourceKind], name: &str) -> &'a ResourceKind {
        entries
            .iter()
            .find(|entry| matches!(entry, ResourceKind::DirectProvider { provider, .. } if provider == name))
            .unwrap_or_else(|| panic!("registry() has no entry for `{name}`"))
    }

    // --- line 1183: the registry exists and lists something ---------------

    #[test]
    fn the_registry_is_not_empty() {
        assert!(!registry(&clients()).is_empty());
    }

    // --- line 1184: native subscription is a different kind than a direct
    // provider or a gateway, at the quota level, not only the type level ---

    #[test]
    fn a_native_subscription_and_a_direct_provider_have_different_quota_shapes() {
        let native = ResourceKind::NativeSubscription {
            client: NativeClient::new("client-a", "Client A"),
        };
        let direct = ResourceKind::from_direct_provider("openrouter");
        assert_eq!(native.quota(), QuotaModel::RollingWindowSubscription);
        assert_eq!(direct.quota(), QuotaModel::MeteredBalance);
        assert_ne!(native.quota(), direct.quota());
    }

    #[test]
    fn the_gateway_is_a_third_kind_delegated_rather_than_flattened_into_either() {
        let gateway = ResourceKind::GlasshouseGateway;
        assert_eq!(gateway.quota(), QuotaModel::DelegatedToUpstream);
        assert_ne!(gateway.quota(), QuotaModel::RollingWindowSubscription);
        assert_ne!(gateway.quota(), QuotaModel::MeteredBalance);
    }

    // --- line 1185: local inference is locality-tagged, and by which
    // server it is rather than by the address it happens to use today -----

    #[test]
    fn ollama_and_llama_cpp_are_local_and_unmetered() {
        let entries = registry(&clients());
        for name in ["ollama", "llama-cpp"] {
            let entry = find_direct(&entries, name);
            assert_eq!(entry.locality(), Locality::Local, "{name}");
            assert_eq!(entry.quota(), QuotaModel::Unmetered, "{name}");
        }
    }

    #[test]
    fn every_router_and_generic_template_is_remote_and_metered() {
        let entries = registry(&clients());
        for name in [
            "openrouter",
            "unorouter",
            "anyrouter",
            "kilo",
            "nous",
            "zai",
            "opencode-zen",
            "nvidia",
            "litellm",
            "openai-compatible",
            "anthropic-compatible",
        ] {
            let entry = find_direct(&entries, name);
            assert_eq!(entry.locality(), Locality::Remote, "{name}");
            assert_eq!(entry.quota(), QuotaModel::MeteredBalance, "{name}");
        }
    }

    /// A provider whose base URL happens to be `localhost` — a self-hosted
    /// LiteLLM proxy, which this project's own template points at
    /// `http://localhost:4000` — must not be reclassified as local inference
    /// on that basis. Locality is decided by which server this is, not by
    /// which address it answers on today.
    #[test]
    fn a_localhost_base_url_does_not_by_itself_make_a_provider_local_inference() {
        let litellm = provider::template("litellm").expect("litellm is a built-in template");
        assert!(
            litellm
                .protocols
                .iter()
                .any(|p| p.base_url.contains("0.0.0.0") || p.base_url.contains("localhost")),
            "this test's premise requires litellm's base URL to be loopback-shaped"
        );
        assert_eq!(locality_of("litellm"), Locality::Remote);
    }

    // --- lines 1186-1192: every named resource kind is actually reachable
    // through the registry, by the name the map uses for it -----------------

    /// Lines 1186-1188 name three harnesses. **Which** clients get a native
    /// subscription is the caller's list, so what this crate owes is that
    /// every slug handed in comes back out unchanged and none is invented —
    /// the gateway may hold which client serves an account and may not know
    /// what that client is.
    #[test]
    fn every_native_subscription_is_one_the_caller_named_and_keeps_its_slug() {
        let clients = clients();
        let entries = registry(&clients);
        let natives: Vec<&NativeClient> = entries
            .iter()
            .filter_map(|entry| match entry {
                ResourceKind::NativeSubscription { client } => Some(client),
                _ => None,
            })
            .collect();
        assert_eq!(
            natives,
            clients.iter().collect::<Vec<_>>(),
            "the native subscriptions must be exactly the caller's clients, in order"
        );
    }

    /// The label a launch note carries for a native subscription is the
    /// caller's own display name, verbatim — this module never restyles a
    /// slug into a name, because it does not know what any client is called.
    #[test]
    fn a_native_subscriptions_label_is_the_callers_display_name_verbatim() {
        let kind = ResourceKind::NativeSubscription {
            client: NativeClient::new("client-a", "Client A"),
        };
        assert_eq!(kind.label(), "Client A subscription");
    }

    #[test]
    fn openrouter_unorouter_anyrouter_kilo_and_nous_are_all_describable() {
        let entries = registry(&clients());
        for name in ["openrouter", "unorouter", "anyrouter", "kilo", "nous"] {
            find_direct(&entries, name);
        }
    }

    // --- `from_direct_provider` is the bridge a real launch uses ----------

    #[test]
    fn from_direct_provider_agrees_with_the_registrys_own_classification() {
        let entries = registry(&clients());
        for name in ["ollama", "llama-cpp", "openrouter", "nvidia"] {
            let via_registry = find_direct(&entries, name);
            let via_bridge = ResourceKind::from_direct_provider(name);
            assert_eq!(via_registry.locality(), via_bridge.locality());
            assert_eq!(via_registry.quota(), via_bridge.quota());
        }
    }

    #[test]
    fn a_label_never_contains_a_credential_shaped_string() {
        // The label is the text a mechanism note carries into a launch log —
        // it must never be able to grow one from a provider name a user
        // configured, which is why it is built from fixed phrases and the
        // name alone, never from anything resolved through `crate::secret`.
        let entry = ResourceKind::from_direct_provider("openrouter");
        assert_eq!(entry.label(), "openrouter (remote)");
    }
}
