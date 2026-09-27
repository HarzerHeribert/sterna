//! Catalogue to [`Upstream`]: what a standalone gateway forwards to, built
//! from its own account catalogue.
//!
//! The invariant this file holds is the one the crate's own header states:
//! **choosing a provider, an account or an entitlement is allowed; changing
//! the model or the effort is not.** Nothing here inspects a request. It
//! reads a catalogue, resolves the credentials that catalogue *names*, and
//! hands `gateway::upstream` a list of backends in a fixed order; every
//! per-request decision after that is `routing::interactive`'s, unchanged.
//!
//! `gateway_upstream`, `subscription_pool` and `subscription_models` started
//! as copies of the host's own `profile::gateway_upstream`,
//! `subscription_pool` and `subscription_models`: where the host reads a
//! `ProviderConfig`, these read [`AccountEntry`] and [`crate::provider::Provider`],
//! and the host's own error types differ from this crate's, so the three
//! functions themselves were **not** unifiable — `GH-CLEANUP-POOL-DUPLICATE`
//! found the divergence and left them as they were. Their leaf helpers — each
//! a pure function of its arguments — were not: `ingress_targets`,
//! `declared_base_url`, `tool_semantics`, `protocol_list` and
//! `describe_provider_protocols` are `pub` because the host's `profile` module
//! imports them rather than keeping its own verbatim copies, still calling
//! them from its own `gateway_upstream`, which stayed host-owned.
//! `GATEWAY_INGRESS_PROTOCOLS` and `gateway_routes` were the one exception
//! `GH-CLEANUP-POOL-DUPLICATE` found and left alone (its list was four
//! protocols, this one five) — `GH-GATEWAY-INGRESS-ONCE` closed that gap:
//! the host's `profile` module now imports this crate's `GATEWAY_INGRESS_PROTOCOLS`
//! and `gateway_routes` directly, so the host's embedded gateway serves the
//! same five ingress targets, `typesafe-systemone` included, as the
//! standalone binary. `valid_cliproxyapi_version` in [`crate::config`] is
//! `pub` for the same reason, called from
//! the host's `paths::RuntimePaths::cliproxyapi_executable`.

use std::collections::BTreeMap;

use crate::entitlement::AccountEntry;
use crate::gateway::subscription_broker::{BrokerPaths, RunningSubscriptionBroker};
use crate::gateway::upstream::UpstreamBackend;
use crate::gateway::{Route, Upstream, UpstreamError};
use crate::provider::{ProtocolCompatibleProviders, Provider};
use crate::routing::wire::{Declared, WireProtocol};
use crate::routing::{Cost, CredentialId, ToolSemantics};
use crate::secret::{SecretRef, SecretStore};

/// The protocols this gateway's ingress knows how to serve.
///
/// A capability, not a promise: what a *running* gateway carries is
/// narrower, because a route exists only for a protocol some backend
/// declared a base URL for. [`crate::gateway::Gateway::served_protocols`] is
/// that narrower answer.
///
/// The order matters in one place only: [`gateway_upstream`] builds routes
/// in it, so it is the order a diagnostic lists protocols in.
///
/// the host's `profile::gateway_upstream` builds its embedded gateway's
/// ingress from this same list, so a Sterna launched by Glasshouse serves
/// exactly the protocols the standalone binary does.
pub const GATEWAY_INGRESS_PROTOCOLS: &[WireProtocol] = &[
    WireProtocol::AnthropicMessages,
    WireProtocol::OpenAiResponses,
    WireProtocol::OpenAiChat,
    WireProtocol::GeminiGenerateContent,
    WireProtocol::TypesafeSystemOne,
];

/// The protocols a subscription broker (CLIProxyAPI) is given routes for.
///
/// **The invariant: a broker claims only the wires it translates; a
/// relay-only protocol is claimed by its own provider's account.** Give the
/// broker `TypesafeSystemOne` too and every `/v1/systemone` request has two
/// claimants, [`crate::gateway::Upstream::serving_for_target`] falls back to
/// the bound account, and the decision request ends at CLIProxyAPI's 404 —
/// measured 2026-09-16 through a fresh standalone gateway.
pub const BROKER_PROTOCOLS: &[WireProtocol] = &[
    WireProtocol::AnthropicMessages,
    WireProtocol::OpenAiResponses,
    WireProtocol::OpenAiChat,
    WireProtocol::GeminiGenerateContent,
];

/// The request-target path prefixes that belong to each ingress protocol.
///
/// Each entry is a prefix matched at a path-segment boundary, so one entry
/// covers a protocol's whole surface. Every prefix was read off a real
/// request line — Claude Code sends `POST /v1/messages?beta=true`, Codex
/// sends `POST /responses` — never guessed.
pub const fn ingress_targets(protocol: WireProtocol) -> &'static [&'static str] {
    match protocol {
        WireProtocol::AnthropicMessages => &["/messages"],
        WireProtocol::OpenAiResponses => &["/responses"],
        WireProtocol::OpenAiChat => &["/chat/completions"],
        // Two spellings, because Google's version segment is `v1beta` and
        // the `/v1` the gateway strips before matching does not cover it.
        WireProtocol::GeminiGenerateContent => &["/models", "/v1beta/models"],
        WireProtocol::TypesafeSystemOne => &["/systemone"],
    }
}

/// Why a standalone gateway could not be given an upstream to forward to.
///
/// Every variant carries names only — a credential value never reaches a
/// diagnostic, which is precisely the case
/// [`Self::NoCredentialResolves`] is printed in.
#[derive(Debug, thiserror::Error)]
pub enum PoolRefusal {
    #[error(
        "this gateway can forward requests for {}, but no configured provider serves any of \
         them with a base URL; configured declarations: {served}. Configure one before \
         serving",
        protocol_list(.protocols),
    )]
    NoProviderServesTheIngress {
        /// Every protocol the ingress offers — what the caller could
        /// configure a provider for, not one of them picked out.
        protocols: Vec<WireProtocol>,
        /// The configured providers' protocol declarations, including an
        /// explicit marker for an empty base URL.
        served: String,
    },

    /// No provider this gateway could forward to has a credential. The
    /// message names every variable that would have been read, grouped by
    /// provider, so the message is the fix — and a name is all it holds.
    #[error("{}", no_credential_message(.candidates))]
    NoCredentialResolves {
        /// Each candidate provider and the variable **names** it declares.
        /// An empty list is a provider that declares no variable at all.
        candidates: Vec<(String, Vec<String>)>,
    },

    /// Every account in the catalogue was skipped, and this says why each
    /// one was. Distinct from [`Self::NoCredentialResolves`] because a
    /// catalogue can fail for reasons a raw provider list cannot: an
    /// account naming a provider nothing declares, or a broker that would
    /// not start.
    #[error(
        "no account in the catalogue can serve a request: {}. A provider key is stored with \
         `inference-gateway credentials set <provider>` (the key on stdin)",
        .notes.join("; ")
    )]
    NoAccountUsable { notes: Vec<String> },

    #[error(transparent)]
    Unusable(#[from] UpstreamError),
}

/// An upstream, and what was skipped on the way to building it.
///
/// The notes are diagnostics, not errors: an account that could not be used
/// alongside three that could is a line on stderr, never a refusal to serve.
/// A caller decides where they go — this file does not print.
pub struct Pool {
    pub upstream: Upstream,
    pub notes: Vec<String>,
}

/// Build the pool a standalone `serve` forwards through.
///
/// **Order is preference, and it is the account catalogue's own name
/// order.** A TOML table deserialises into a [`BTreeMap`], which sorts; the
/// alternative — file order — would make the same catalogue behave
/// differently after an editor moved a block, so the sort is the stable
/// choice rather than an accident of the parser.
///
/// Two kinds of account, and an account that is neither is skipped with a
/// note rather than refused:
///
/// - **broker-backed** (`subscription_broker` is set): a CLIProxyAPI sidecar
///   is started for it, and the subscription's own model catalogue decides
///   which models that backend serves.
/// - **provider-backed** (`provider` names something in `providers`): the
///   account's own `credential` is resolved if it states one, and the
///   provider's declared `credential_env` names are tried if it does not.
///   An account with `models` set gets its backend built `.with_models(…)`,
///   the same declaration a broker-backed account's own catalogue already
///   provides; unset behaves exactly as before this existed — the backend
///   is a candidate for any model.
///
/// An **empty catalogue** is not an error: it falls through to
/// [`gateway_upstream`] over `providers` alone, which is the shape a
/// configuration that names only `[providers.…]` has.
///
/// `free` answers, by name, whether the caller marks a provider free-tier;
/// unasked answers `false`, [`Cost::Metered`]'s fail-closed default.
pub fn pool_from_catalogue(
    accounts: &BTreeMap<String, AccountEntry>,
    providers: &[Provider],
    secrets: &dyn SecretStore,
    broker_paths: &dyn Fn(&str) -> BrokerPaths,
    free: &dyn Fn(&str) -> bool,
) -> Result<Pool, PoolRefusal> {
    if accounts.is_empty() {
        return Ok(Pool {
            upstream: gateway_upstream(providers, secrets, free)?,
            notes: vec![
                "the account catalogue is empty; serving every configured provider whose \
                 credential resolves"
                    .to_owned(),
            ],
        });
    }

    let mut backends = Vec::new();
    let mut notes = Vec::new();
    for (name, entry) in accounts {
        if entry.subscription_broker().is_some() {
            let paths = broker_paths(name);
            match RunningSubscriptionBroker::start(&paths, name) {
                Ok(broker) => match subscription_backend(broker) {
                    Ok(backend) => {
                        crate::provider::subscription_models::refresh_in_background(
                            paths.entitlement_dir.clone(),
                            paths.auth_dir.clone(),
                            name.clone(),
                        );
                        backends.push(backend.with_account(name));
                    }
                    Err(error) => notes.push(format!("account `{name}`: {error}")),
                },
                Err(error) => {
                    notes.push(format!(
                        "account `{name}`: its broker would not start: {error}"
                    ));
                }
            }
            continue;
        }

        let Some(provider_name) = entry.provider() else {
            notes.push(format!(
                "account `{name}` names neither a provider nor a subscription broker, so \
                 nothing can be forwarded to it"
            ));
            continue;
        };
        let Some(provider) = providers.iter().find(|p| p.name == provider_name) else {
            notes.push(format!(
                "account `{name}` names the provider `{provider_name}`, which is neither \
                 configured nor a built-in template"
            ));
            continue;
        };
        let routes = gateway_routes(provider);
        if routes.is_empty() {
            notes.push(format!(
                "account `{name}`: the provider `{provider_name}` declares a base URL for none \
                 of the protocols this ingress serves"
            ));
            continue;
        }
        // An account that names a credential uses exactly that one — never
        // another account's key by way of the provider's variable names,
        // which only an account naming nothing falls back to. Two accounts
        // of one provider are two keys, and the per-account reference is
        // what distinguishes them; two that name nothing resolve to the
        // same credential and the second is never selected.
        let references: Vec<SecretRef> = match entry.credential() {
            Some(credential) => vec![credential.secret_ref().clone()],
            None => provider.secret_refs(),
        };
        let resolved = references
            .into_iter()
            .find_map(|reference| secrets.resolve(&reference).map(|value| (reference, value)));
        let Some((reference, credential)) = resolved else {
            notes.push(format!(
                "account `{name}`: nothing the catalogue names for it currently holds a \
                 credential, so it was skipped"
            ));
            continue;
        };
        match UpstreamBackend::new(
            provider.name.clone(),
            routes,
            credential,
            CredentialId::new(provider.name.clone(), reference),
            if free(&provider.name) {
                Cost::Free
            } else {
                Cost::Metered
            },
        ) {
            Ok(backend) => backends.push(
                match entry.models() {
                    Some(models) => backend.with_models(models),
                    None => backend,
                }
                .with_account(name),
            ),
            Err(error) => notes.push(format!("account `{name}`: {error}")),
        }
    }

    if backends.is_empty() {
        return Err(PoolRefusal::NoAccountUsable { notes });
    }
    Ok(Pool {
        upstream: Upstream::with_failover(backends)?,
        notes,
    })
}

/// Which configured providers this gateway may forward to when no account
/// catalogue narrows the question: every provider that serves an ingress
/// protocol with a base URL, once per credential variable that resolves.
///
/// Each credential a provider declares that currently resolves is its own
/// backend — several credentials for one provider are several backends,
/// which is what makes per-key rotation possible. A variable with no value
/// is skipped rather than refused: a user who has one of two keys set has
/// one working backend, and failing the whole start over the other would be
/// worse than using what is there.
pub fn gateway_upstream(
    providers: &[Provider],
    secrets: &dyn SecretStore,
    free: &dyn Fn(&str) -> bool,
) -> Result<Upstream, PoolRefusal> {
    // The routing constraint before any selection. Its result has a distinct
    // type so a future model-quality scorer can only be handed providers
    // which have already passed it.
    let candidates =
        ProtocolCompatibleProviders::for_any_protocol(providers, GATEWAY_INGRESS_PROTOCOLS);

    if candidates.is_empty() {
        return Err(PoolRefusal::NoProviderServesTheIngress {
            protocols: GATEWAY_INGRESS_PROTOCOLS.to_vec(),
            served: describe_provider_protocols(providers),
        });
    }

    let mut backends = Vec::new();
    let mut named = Vec::new();
    for candidate in candidates.iter() {
        let provider = candidate.provider();
        named.push((provider.name.clone(), provider.credential_env.clone()));
        for var in &provider.credential_env {
            let reference = SecretRef::Environment { var: var.clone() };
            let Some(credential) = secrets.resolve(&reference) else {
                continue;
            };
            backends.push(UpstreamBackend::new(
                provider.name.clone(),
                gateway_routes(provider),
                credential,
                CredentialId::new(provider.name.clone(), reference),
                if free(&provider.name) {
                    Cost::Free
                } else {
                    Cost::Metered
                },
            )?);
        }
    }

    if backends.is_empty() {
        return Err(PoolRefusal::NoCredentialResolves { candidates: named });
    }

    Ok(Upstream::with_failover(backends)?)
}

/// One backend over one running subscription sidecar.
///
/// A subscription is the routing unit whose contents are actually knowable:
/// it says which models the account holds. A raw provider key says neither,
/// which is why a pool over several accounts routes per model only for
/// these.
fn subscription_backend(
    broker: RunningSubscriptionBroker,
) -> Result<UpstreamBackend, UpstreamError> {
    let base = broker.base_url().to_owned();
    let routes = BROKER_PROTOCOLS
        .iter()
        .map(|protocol| {
            Route::new(
                protocol.slug().to_owned(),
                ingress_targets(*protocol),
                &base,
            )
            .with_tools(ToolSemantics::Verified)
        })
        .collect();
    let models = subscription_models(&broker);
    UpstreamBackend::from_subscription_broker(routes, broker, models)
}

/// The model identifiers a subscription says it serves.
///
/// Parsed here rather than in `gateway::upstream` because that directory's
/// relay files may not name a deserializer at all — the rule that keeps a
/// body inspection from ever being written there. A catalogue that will not
/// answer or will not parse yields an empty list, which claims nothing: an
/// account declaring no model is never selected for one.
pub fn subscription_models(broker: &RunningSubscriptionBroker) -> Vec<String> {
    let Ok(document) = broker.model_catalogue_document() else {
        return Vec::new();
    };
    parse_model_catalogue(&document)
}

/// The `id` of every entry in an OpenAI-shaped `{"data":[…]}` model list.
///
/// Shared by the pool and by `entitlements --json --refresh`, so the models
/// a request can be routed to and the models a caller is told about are read
/// out of the same document by the same code.
pub fn parse_model_catalogue(document: &[u8]) -> Vec<String> {
    let Ok(parsed) = serde_json::from_slice::<serde_json::Value>(document) else {
        return Vec::new();
    };
    parsed
        .get("data")
        .and_then(serde_json::Value::as_array)
        .map(|models| {
            models
                .iter()
                .filter_map(|model| {
                    model
                        .get("id")
                        .and_then(serde_json::Value::as_str)
                        .filter(|id| !id.is_empty())
                        .map(str::to_owned)
                })
                .collect()
        })
        .unwrap_or_default()
}

/// One [`Route`] per ingress protocol `provider` actually serves, in the
/// ingress's own order.
///
/// A protocol it does not serve gets no route, which is what makes a request
/// for it a refusal rather than a request sent to some other protocol's base
/// URL. `pub` because the host's `profile::gateway_upstream` calls this
/// directly now (`GH-GATEWAY-INGRESS-ONCE`) rather than keeping its own copy
/// over a narrower `GATEWAY_INGRESS_PROTOCOLS`.
pub fn gateway_routes(provider: &Provider) -> Vec<Route> {
    GATEWAY_INGRESS_PROTOCOLS
        .iter()
        .filter_map(|protocol| {
            declared_base_url(provider, *protocol).map(|base_url| {
                Route::new(
                    protocol.slug().to_owned(),
                    ingress_targets(*protocol),
                    base_url,
                )
                .with_tools(tool_semantics(provider, *protocol))
            })
        })
        .collect()
}

/// What a provider declares about tool calls on one protocol, as the three
/// states a routing policy needs.
///
/// [`Declared::is_known_present`] collapses "verified absent" into "nobody
/// checked", which is exactly the distinction routing turns on, so the
/// translation is explicit here rather than done with that helper.
pub fn tool_semantics(provider: &Provider, protocol: WireProtocol) -> ToolSemantics {
    match provider.serves(protocol).map(|support| &support.tool_calls) {
        Some(Declared::Verified { value: true, .. }) => ToolSemantics::Verified,
        Some(Declared::Verified { value: false, .. }) => ToolSemantics::KnownAbsent,
        Some(Declared::Unverified) | None => ToolSemantics::Unverified,
    }
}

/// The base URL `provider` declares for `protocol`, or `None` when it
/// declares none — or declares an empty one.
///
/// An empty base URL is not a base URL: the generic templates ship one so a
/// user can supply their own, and forwarding to `""` must never happen.
pub fn declared_base_url(provider: &Provider, protocol: WireProtocol) -> Option<&str> {
    provider
        .serves(protocol)
        .map(|support| support.base_url.as_str())
        .filter(|base_url| !base_url.is_empty())
}

/// The one sentence a user reads when nothing resolves: every variable that
/// was looked for, by provider, and the command that stores one.
fn no_credential_message(candidates: &[(String, Vec<String>)]) -> String {
    let variables: Vec<String> = candidates
        .iter()
        .flat_map(|(provider, vars)| vars.iter().map(move |var| format!("{var} ({provider})")))
        .collect();
    let keyless: Vec<&str> = candidates
        .iter()
        .filter(|(_, vars)| vars.is_empty())
        .map(|(provider, _)| provider.as_str())
        .collect();
    let mut message = if variables.is_empty() {
        "no provider has a credential: none of the configured providers declares a credential \
         variable"
            .to_owned()
    } else {
        format!(
            "no provider has a credential: none of {} holds a value in the gateway's credential \
             file, the native secure store or the environment. Store one with \
             `inference-gateway credentials set <provider>` (the key on stdin)",
            variables.join(", ")
        )
    };
    if !keyless.is_empty() {
        message.push_str(&format!(
            "; {} declare no credential variable",
            keyless.join(", ")
        ));
    }
    message
}

/// `a`, `b` and `c` — a list of protocols for a message a user reads.
pub fn protocol_list(protocols: &[WireProtocol]) -> String {
    protocols
        .iter()
        .map(|protocol| protocol.slug())
        .collect::<Vec<_>>()
        .join(", ")
}

/// The configured protocol declarations for a refusal after compatibility
/// filtering left no candidates.
///
/// An empty base URL is named rather than elided: it is a declaration with
/// no destination, which is precisely why it could not pass the filter.
pub fn describe_provider_protocols(providers: &[Provider]) -> String {
    if providers.is_empty() {
        return "no configured providers".to_owned();
    }

    providers
        .iter()
        .map(|provider| {
            let protocols = if provider.protocols.is_empty() {
                "no protocol at all".to_owned()
            } else {
                provider
                    .protocols
                    .iter()
                    .map(|support| {
                        if support.base_url.is_empty() {
                            format!("`{}` (no base URL)", support.protocol)
                        } else {
                            format!("`{}`", support.protocol)
                        }
                    })
                    .collect::<Vec<_>>()
                    .join(", ")
            };
            format!("`{}` declares {protocols}", provider.name)
        })
        .collect::<Vec<_>>()
        .join("; ")
}

#[cfg(test)]
mod tests {
    #[test]
    fn a_broker_claims_every_translated_wire_and_no_relay_only_one() {
        // A subscription broker translates the four chat wires; a decision
        // request must find its provider's own account as the unique claimant.
        assert!(!BROKER_PROTOCOLS.contains(&WireProtocol::TypesafeSystemOne));
        for protocol in GATEWAY_INGRESS_PROTOCOLS {
            assert_eq!(
                BROKER_PROTOCOLS.contains(protocol),
                *protocol != WireProtocol::TypesafeSystemOne,
                "{}",
                protocol.slug()
            );
        }
    }

    use super::*;
    use crate::provider::unverified_provider;
    use crate::secret::EnvironmentSecretStore;

    fn fake_provider(base_url: &str) -> Provider {
        unverified_provider(
            "fake",
            WireProtocol::AnthropicMessages,
            base_url,
            vec!["POOL_TEST_KEY".to_owned()],
        )
    }

    fn broker_paths(_: &str) -> BrokerPaths {
        BrokerPaths {
            brokers_dir: std::path::PathBuf::from("/nonexistent"),
            entitlement_dir: std::path::PathBuf::from("/nonexistent/e"),
            auth_dir: std::path::PathBuf::from("/nonexistent/e/auth"),
            executable: std::path::PathBuf::from("/nonexistent/cliproxyapi"),
        }
    }

    /// An account naming a provider and a credential that resolves becomes
    /// exactly one backend, and it is the one the account named.
    #[test]
    fn an_account_with_a_resolvable_credential_becomes_a_backend() {
        // SAFETY: single-threaded within this test; the variable name is
        // unique to this file so no other test observes it.
        unsafe { std::env::set_var("POOL_TEST_KEY", "not-a-real-key") };
        let providers = vec![fake_provider("http://127.0.0.1:9/")];
        let mut accounts = BTreeMap::new();
        let mut entry = AccountEntry::default();
        entry.set_provider(Some("fake".to_owned()));
        accounts.insert("one".to_owned(), entry);

        let pool = pool_from_catalogue(
            &accounts,
            &providers,
            &EnvironmentSecretStore::new(),
            &broker_paths,
            &|_| false,
        )
        .expect("the catalogue names a provider whose credential resolves");
        assert_eq!(pool.upstream.backends().len(), 1);
        assert_eq!(
            pool.upstream.backends()[0].credential_id().provider(),
            "fake"
        );
        unsafe { std::env::remove_var("POOL_TEST_KEY") };
    }

    /// An account naming a provider nothing declares is skipped with a note
    /// naming it, and — being the only account — that leaves a refusal that
    /// carries the note rather than an empty pool that would 404 later.
    #[test]
    fn an_account_naming_an_unknown_provider_is_refused_by_name() {
        let mut accounts = BTreeMap::new();
        let mut entry = AccountEntry::default();
        entry.set_provider(Some("nowhere".to_owned()));
        accounts.insert("one".to_owned(), entry);

        let Err(error) = pool_from_catalogue(
            &accounts,
            &[],
            &EnvironmentSecretStore::new(),
            &broker_paths,
            &|_| false,
        ) else {
            panic!("nothing declares `nowhere`")
        };
        let rendered = error.to_string();
        assert!(rendered.contains("nowhere"), "{rendered}");
        assert!(rendered.contains("account `one`"), "{rendered}");
    }
}
