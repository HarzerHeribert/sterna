//! The standalone gateway's own configuration file: which accounts exist,
//! and which providers they name.
//!
//! Two tables, and the split between them is the same one the library draws.
//! `[accounts.<name>]` deserialises straight into
//! [`crate::entitlement::AccountEntry`] — the *catalogue* half, six keys,
//! `deny_unknown_fields`, and a credential that is a **reference and never a
//! value**. `[providers.<name>]` is the destination half: a base URL per
//! protocol, the environment variable names a key may come from, and any
//! extra headers. Neither table may hold a secret; the reference in an
//! account is resolved through [`crate::secret::SecretStore`] at the moment
//! of use and never earlier.
//!
//! **A missing file is an empty catalogue, not an error.** A gateway with
//! nothing configured is a gateway that will refuse to serve for a reason it
//! can name, which is a better first run than a parse error about a file the
//! user has never opened.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::Deserialize;

use crate::entitlement::{AccountEntry, deserialize_credential_env_names};
use crate::gateway::subscription_broker::BrokerPaths;
use crate::provider::{ProtocolSupport, Provider, unverified_support};
use crate::routing::wire::WireProtocol;

/// The qualifier/organisation/application triple every platform location is
/// derived from. Empty qualifier and organisation, so the layout is
/// `~/.config/inference-gateway` on Linux and
/// `~/Library/Application Support/inference-gateway` on macOS.
const APPLICATION: &str = "inference-gateway";

/// The configuration file's name inside the platform configuration
/// directory.
const CONFIG_FILE: &str = "gateway.toml";

/// The protocol a `[providers.<name>]` entry serves when it names none.
///
/// Anthropic Messages, because that is the ingress Sterna points at: it sets
/// `ANTHROPIC_BASE_URL` to this gateway and its client sends
/// `POST /v1/messages`. A default of anything else would make the common
/// configuration the one that has to say the most.
const DEFAULT_PROTOCOL: WireProtocol = WireProtocol::AnthropicMessages;

/// The whole of `gateway.toml`.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GatewayConfig {
    /// `[accounts.<name>]` — the catalogue this gateway serves from.
    #[serde(default)]
    pub accounts: BTreeMap<String, AccountEntry>,
    /// `[providers.<name>]` — destinations an account may name. A name that
    /// matches a built-in template overrides it.
    #[serde(default)]
    pub providers: BTreeMap<String, ProviderEntry>,
}

/// One `[providers.<name>]` table: where requests for this provider go.
///
/// `base_url` with an optional `protocol` is the shorthand for the single
/// protocol case; `protocols` is the table form for a provider that serves
/// more than one. Both may be present, and the shorthand is merged in first,
/// so the ingress order is the shorthand's protocol followed by the rest in
/// slug order.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProviderEntry {
    /// The single protocol's base URL. Absent when `protocols` says it all.
    #[serde(default)]
    pub base_url: Option<String>,
    /// Which protocol `base_url` serves. Defaults to `anthropic-messages`.
    #[serde(default)]
    pub protocol: Option<String>,
    /// Protocol slug to base URL, for a provider serving several.
    #[serde(default)]
    pub protocols: BTreeMap<String, String>,
    /// Environment variable **names** a credential for this provider may
    /// come from. Deserialised through the catalogue's own shape check, so a
    /// key pasted where a name belongs is refused without being echoed.
    #[serde(default, deserialize_with = "deserialize_credential_env_names")]
    pub credential_env: Vec<String>,
    /// Extra request headers this provider needs. Configuration, not
    /// credentials — a header value here is written by the user and is not
    /// resolved through a secret store.
    #[serde(default)]
    pub headers: BTreeMap<String, String>,
}

/// What a load produced, and where it came from.
///
/// `path` is `None` only when no platform configuration directory could be
/// determined at all. `present` is false when the file simply is not there,
/// which is the empty-catalogue case and not a failure.
pub struct Loaded {
    pub config: GatewayConfig,
    pub path: Option<PathBuf>,
    pub present: bool,
}

impl Loaded {
    /// The one line a caller prints to stderr about where its configuration
    /// came from. Never printed on stdout: `serve`'s stdout carries exactly
    /// one line and it is the ready line.
    pub fn note(&self) -> String {
        match (&self.path, self.present) {
            (Some(path), true) => format!(
                "configuration: {} ({} account(s), {} provider(s))",
                path.display(),
                self.config.accounts.len(),
                self.config.providers.len()
            ),
            (Some(path), false) => format!(
                "configuration: {} does not exist; starting with an empty catalogue",
                path.display()
            ),
            (None, _) => "configuration: no platform configuration directory could be \
                          determined; starting with an empty catalogue"
                .to_owned(),
        }
    }
}

/// Parses one configuration text. **The refusal carries toml's message and
/// never its source excerpt**: a credential pasted as a value is refused by
/// `deserialize_credential` without being echoed, and toml's default
/// rendering would print the offending line back — which is the one thing
/// the refusal exists to avoid.
pub fn parse(text: &str) -> Result<GatewayConfig> {
    toml::from_str(text).map_err(|error: toml::de::Error| anyhow::anyhow!("{}", error.message()))
}

/// Read the configuration from `explicit`, or from the platform location.
///
/// A file that is not there yields an empty catalogue. A file that is there
/// and will not parse is an error: the user wrote it, and silently ignoring
/// what they wrote is how a gateway ends up serving from a catalogue nobody
/// intended.
pub fn load(explicit: Option<&Path>) -> Result<Loaded> {
    let path = match explicit {
        Some(path) => Some(path.to_path_buf()),
        None => default_config_path(),
    };
    let Some(path) = path else {
        return Ok(Loaded {
            config: GatewayConfig::default(),
            path: None,
            present: false,
        });
    };
    match std::fs::read_to_string(&path) {
        Ok(text) => {
            let config = parse(&text)
                .with_context(|| format!("could not read the gateway configuration {path:?}"))?;
            Ok(Loaded {
                config,
                path: Some(path),
                present: true,
            })
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Loaded {
            config: GatewayConfig::default(),
            path: Some(path),
            present: false,
        }),
        Err(error) => {
            Err(error).with_context(|| format!("could not read the gateway configuration {path:?}"))
        }
    }
}

/// Appends one `[accounts.<name>]` or `[providers.<name>]` table to the
/// configuration at `path` unless it is already declared, answering whether
/// it wrote. **Appended as text, never re-serialised**: a person's comments
/// and ordering in the file stay exactly as they wrote them. The result must
/// parse before it is written, and it replaces the file through a rename, so
/// a refusal leaves the file untouched.
pub fn declare_table(path: &Path, table: &str, body: &str) -> Result<bool> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(error) => {
            return Err(error).with_context(|| format!("could not read {path:?}"));
        }
    };
    let config = parse(&text).with_context(|| format!("could not read {path:?}"))?;
    let declared = match table.split_once('.') {
        Some(("accounts", name)) => config.accounts.contains_key(name),
        Some(("providers", name)) => config.providers.contains_key(name),
        _ => anyhow::bail!("`{table}` is not an accounts or providers table"),
    };
    if declared {
        return Ok(false);
    }
    let mut next = text;
    if !next.is_empty() && !next.ends_with('\n') {
        next.push('\n');
    }
    if !next.is_empty() {
        next.push('\n');
    }
    next.push_str(&format!("[{table}]\n{body}"));
    parse(&next).context("the new table would not parse")?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).with_context(|| format!("could not create {parent:?}"))?;
    }
    let temporary = path.with_extension("toml.tmp");
    std::fs::write(&temporary, next).with_context(|| format!("could not write {temporary:?}"))?;
    std::fs::rename(&temporary, path).with_context(|| format!("could not replace {path:?}"))?;
    Ok(true)
}

/// `<platform config dir>/gateway.toml`, or `None` when no such directory
/// can be determined.
/// `INFERENCE_GATEWAY_CONFIG` names the file outright — the override a
/// caller that spawns this binary without passing `--config` (sterna) and a
/// test that must not touch the user's own catalogue both need.
pub fn default_config_path() -> Option<PathBuf> {
    if let Some(path) = std::env::var_os("INFERENCE_GATEWAY_CONFIG") {
        return Some(PathBuf::from(path));
    }
    directories::ProjectDirs::from("", "", APPLICATION)
        .map(|dirs| dirs.config_dir().join(CONFIG_FILE))
}

/// The private state root brokers, auth directories and caches hang off.
/// `INFERENCE_GATEWAY_DATA_DIR` overrides the platform location, for the
/// same two callers as [`default_config_path`].
pub fn default_data_dir() -> Option<PathBuf> {
    if let Some(path) = std::env::var_os("INFERENCE_GATEWAY_DATA_DIR") {
        return Some(PathBuf::from(path));
    }
    directories::ProjectDirs::from("", "", APPLICATION).map(|dirs| dirs.data_dir().to_path_buf())
}

/// The gateway's own credential file, where `credentials set` stores a
/// provider key — see [`crate::secret::file`] for why a gateway keeps one.
pub fn credentials_path(data_dir: &Path) -> PathBuf {
    data_dir.join("credentials.toml")
}

/// The four directories one entitlement's subscription broker needs,
/// derived from `data_dir`.
///
/// Laid out exactly as the host lays them out, hex-encoding the entitlement
/// name: that preserves identity without putting a user-controlled path
/// separator into a filesystem path, and it means a host and a standalone
/// gateway pointed at the same data directory find the same login.
pub fn broker_paths(data_dir: &Path, entitlement: &str) -> BrokerPaths {
    let brokers_dir = data_dir.join("subscription-brokers");
    let entitlement_dir = brokers_dir.join(format!(
        "entitlement-{}",
        hex::encode(entitlement.as_bytes())
    ));
    let auth_dir = entitlement_dir.join("auth");
    BrokerPaths {
        brokers_dir,
        entitlement_dir,
        auth_dir,
        executable: cliproxyapi_executable(data_dir),
    }
}

/// The stable OAuth directory for one entitlement — the half of
/// [`broker_paths`] that a login writes and a status read looks at.
pub fn broker_auth_dir(data_dir: &Path, entitlement: &str) -> PathBuf {
    broker_paths(data_dir, entitlement).auth_dir
}

/// The managed CLIProxyAPI executable, unless `INFERENCE_GATEWAY_CLIPROXYAPI_BIN`
/// names one — which the broker itself checks, so this is only the fallback.
///
/// Laid out exactly as a host lays it out, for the same reason the broker
/// directories are: `tools/cliproxyapi/current` holds a `sha256-…` marker
/// naming the extracted release directory, whose executable is
/// `cliproxyapi`. The flat `tools/CLIProxyAPI` is what a data directory
/// nothing managed has — a binary placed by hand. Measured 2026-09-11: the
/// flat path alone never existed on a machine a host had managed, so every
/// standalone subscription account failed to start its broker.
fn cliproxyapi_executable(data_dir: &Path) -> PathBuf {
    let root = data_dir.join("tools").join("cliproxyapi");
    if let Ok(marker) = std::fs::read_to_string(root.join("current")) {
        let version = marker.trim();
        if valid_cliproxyapi_version(version) {
            return root.join(version).join(if cfg!(windows) {
                "cliproxyapi.exe"
            } else {
                "cliproxyapi"
            });
        }
    }
    let name = if cfg!(windows) {
        "CLIProxyAPI.exe"
    } else {
        "CLIProxyAPI"
    };
    data_dir.join("tools").join(name)
}

/// A release marker is `sha256-` and sixty-four hex digits — the digest of
/// the archive the directory was extracted from — and nothing else, so a
/// marker can never name a path outside `tools/cliproxyapi`.
///
/// `pub` for the host's `paths::RuntimePaths::cliproxyapi_executable`, whose
/// own marker-validation logic was a verbatim twin of this one; its
/// surrounding fallback logic differs (a `.trim()`, and two extra marker
/// states) and stays there.
pub fn valid_cliproxyapi_version(version: &str) -> bool {
    version.strip_prefix("sha256-").is_some_and(|digest| {
        digest.len() == 64 && digest.bytes().all(|byte| byte.is_ascii_hexdigit())
    })
}

/// Where a model catalogue read back by `entitlements --json` is cached.
pub fn model_cache_dir(data_dir: &Path) -> PathBuf {
    data_dir.join("model-catalogues")
}

/// Every provider an account may name: the configured entries first, then
/// the built-in templates a configured entry did not override.
///
/// Configured first because an entry naming a template's name is an
/// override, and an override that lost to the thing it overrides would be
/// the opposite of what was written.
pub fn providers(config: &GatewayConfig) -> Vec<Provider> {
    let mut out: Vec<Provider> = config
        .providers
        .iter()
        .map(|(name, entry)| entry.to_provider(name))
        .collect();
    // The provider a standalone gateway is most often pointed at with no
    // catalogue written yet: Anthropic's own API through `ANTHROPIC_API_KEY`.
    // A configured `[providers.anthropic]` replaces it; the shared templates
    // below stay what every host ships.
    if !out.iter().any(|provider| provider.name == "anthropic") {
        out.push(
            ProviderEntry {
                base_url: Some("https://api.anthropic.com".to_owned()),
                protocol: Some("anthropic-messages".to_owned()),
                protocols: BTreeMap::new(),
                credential_env: vec!["ANTHROPIC_API_KEY".to_owned()],
                headers: Default::default(),
            }
            .to_provider("anthropic"),
        );
    }
    for template in crate::provider::templates() {
        if !out.iter().any(|provider| provider.name == template.name) {
            out.push(template);
        }
    }
    out
}

impl ProviderEntry {
    /// This entry as the catalogue type the pool builds routes from.
    ///
    /// Everything is `Declared::Unverified`: nothing probed this provider,
    /// and recording a capability nobody checked is the one thing the
    /// provider model refuses.
    pub fn to_provider(&self, name: &str) -> Provider {
        let mut protocols: Vec<ProtocolSupport> = Vec::new();
        if let Some(base_url) = &self.base_url {
            let protocol = self
                .protocol
                .as_deref()
                .and_then(protocol_from_slug)
                .unwrap_or(DEFAULT_PROTOCOL);
            protocols.push(unverified_support(protocol, base_url));
        }
        for (slug, base_url) in &self.protocols {
            let Some(protocol) = protocol_from_slug(slug) else {
                continue;
            };
            if protocols.iter().any(|s| s.protocol == protocol) {
                continue;
            }
            protocols.push(unverified_support(protocol, base_url));
        }
        Provider {
            name: name.to_owned(),
            protocols,
            model_list_endpoint: crate::routing::wire::Declared::Unverified,
            usage_telemetry: crate::routing::wire::Declared::Unverified,
            credential_env: self.credential_env.clone(),
            headers: self
                .headers
                .iter()
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect(),
        }
    }
}

/// A protocol slug back to its [`WireProtocol`], or `None`.
///
/// The inverse of [`WireProtocol::slug`], written out rather than derived so
/// that a slug this build does not know is `None` — an entry skipped — and
/// never a protocol guessed from a neighbouring spelling.
pub fn protocol_from_slug(slug: &str) -> Option<WireProtocol> {
    match slug {
        "anthropic-messages" => Some(WireProtocol::AnthropicMessages),
        "openai-responses" => Some(WireProtocol::OpenAiResponses),
        "openai-chat" => Some(WireProtocol::OpenAiChat),
        "gemini-generate-content" => Some(WireProtocol::GeminiGenerateContent),
        "typesafe-systemone" => Some(WireProtocol::TypesafeSystemOne),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn a_declared_table_is_appended_once_and_the_file_keeps_its_comments() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("gateway.toml");
        std::fs::write(&path, "# mine\n[accounts.groq]\nprovider = \"groq\"\n").unwrap();

        assert!(
            declare_table(
                &path,
                "accounts.chatgpt-subscription",
                "kind = \"chatgpt\"\nvendor = \"openai\"\nsubscription_broker = \"cliproxyapi\"\n"
            )
            .unwrap()
        );
        assert!(
            !declare_table(
                &path,
                "accounts.chatgpt-subscription",
                "kind = \"chatgpt\"\n"
            )
            .unwrap(),
            "declared once"
        );
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.starts_with("# mine\n[accounts.groq]"), "{text}");
        let config = parse(&text).unwrap();
        assert!(
            config.accounts["chatgpt-subscription"]
                .subscription_broker()
                .is_some()
        );

        // A body that would not parse leaves the file exactly as it was.
        assert!(declare_table(&path, "accounts.broken", "kind = \n").is_err());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), text);

        // No file yet: the table is the whole file.
        let fresh = dir.path().join("new/gateway.toml");
        assert!(declare_table(&fresh, "accounts.groq", "provider = \"groq\"\n").unwrap());
        assert_eq!(
            std::fs::read_to_string(&fresh).unwrap(),
            "[accounts.groq]\nprovider = \"groq\"\n"
        );
    }

    /// With no configuration at all, Anthropic's API is a provider through
    /// The broker executable follows the managed layout's release marker,
    /// and only a well-formed marker; anything else is the flat fallback.
    #[test]
    fn the_broker_executable_follows_a_managed_release_marker() {
        let scratch = tempfile::tempdir().expect("a scratch directory");
        let data_dir = scratch.path();
        let flat = data_dir.join("tools").join(if cfg!(windows) {
            "CLIProxyAPI.exe"
        } else {
            "CLIProxyAPI"
        });
        assert_eq!(broker_paths(data_dir, "acct").executable, flat);

        let root = data_dir.join("tools").join("cliproxyapi");
        std::fs::create_dir_all(&root).expect("created");
        std::fs::write(root.join("current"), "not-a-digest\n").expect("written");
        assert_eq!(
            broker_paths(data_dir, "acct").executable,
            flat,
            "a malformed marker cannot name a directory"
        );

        let digest = format!("sha256-{}", "ab".repeat(32));
        std::fs::write(root.join("current"), format!("{digest}\n")).expect("written");
        assert_eq!(
            broker_paths(data_dir, "acct").executable,
            root.join(&digest).join(if cfg!(windows) {
                "cliproxyapi.exe"
            } else {
                "cliproxyapi"
            })
        );
    }

    /// `ANTHROPIC_API_KEY`; a configured `[providers.anthropic]` replaces it.
    #[test]
    fn anthropics_api_is_a_provider_out_of_the_box_and_a_configured_one_replaces_it() {
        let bare = providers(&GatewayConfig::default());
        let anthropic = bare
            .iter()
            .find(|provider| provider.name == "anthropic")
            .expect("anthropic is a provider with no configuration");
        assert_eq!(anthropic.protocols[0].base_url, "https://api.anthropic.com");
        assert_eq!(
            anthropic.credential_env,
            vec!["ANTHROPIC_API_KEY".to_owned()]
        );

        let config: GatewayConfig = toml::from_str(
            r#"
[providers.anthropic]
base_url = "http://127.0.0.1:4321"
protocol = "anthropic-messages"
credential_env = ["MY_KEY"]
"#,
        )
        .expect("parses");
        let configured = providers(&config);
        let anthropic: Vec<_> = configured
            .iter()
            .filter(|provider| provider.name == "anthropic")
            .collect();
        assert_eq!(
            anthropic.len(),
            1,
            "one anthropic provider, the configured one"
        );
        assert_eq!(anthropic[0].protocols[0].base_url, "http://127.0.0.1:4321");
    }

    use super::*;

    /// The two tables parse, and an account's credential arrives as a
    /// reference.
    #[test]
    fn both_tables_parse() {
        let config: GatewayConfig = toml::from_str(
            r#"
[providers.fake]
base_url = "http://127.0.0.1:1234"
credential_env = ["FAKE_KEY"]

[accounts.work]
kind = "claude"
provider = "fake"
credential = { env = "FAKE_KEY" }
"#,
        )
        .expect("the documented shape parses");
        assert_eq!(config.accounts.len(), 1);
        let provider = config.providers["fake"].to_provider("fake");
        assert_eq!(provider.protocols.len(), 1);
        assert_eq!(provider.protocols[0].protocol, DEFAULT_PROTOCOL);
        assert_eq!(provider.protocols[0].base_url, "http://127.0.0.1:1234");
        assert!(
            config.accounts["work"].credential().is_some(),
            "a credential reference survives the round trip"
        );
    }

    /// A credential written as a value rather than a reference is refused,
    /// and the refusal does not repeat what was written.
    #[test]
    fn a_pasted_credential_is_refused_without_being_echoed() {
        let error = parse(
            r#"
[accounts.work]
credential = "sk-ant-notarealkey-000"
"#,
        )
        .expect_err("a bare string is not a reference");
        let rendered = error.to_string();
        assert!(!rendered.contains("sk-ant-notarealkey-000"), "{rendered}");
        assert!(rendered.contains("never a value"), "{rendered}");
    }

    /// An account that declares `models` parses it as the list it wrote.
    #[test]
    fn an_account_declaring_models_parses_the_list() {
        let config: GatewayConfig = toml::from_str(
            r#"
[accounts.work]
kind = "api-key"
provider = "fake"
models = ["a", "b"]
"#,
        )
        .expect("the documented shape parses");
        assert_eq!(
            config.accounts["work"].models(),
            Some(["a".to_owned(), "b".to_owned()].as_slice())
        );
    }

    /// An account that says nothing about `models` parses to `None` — the
    /// account serves whatever is asked, unchanged from before this field
    /// existed.
    #[test]
    fn an_account_without_models_parses_to_none() {
        let config: GatewayConfig = toml::from_str(
            r#"
[accounts.work]
kind = "api-key"
provider = "fake"
"#,
        )
        .expect("the documented shape parses");
        assert_eq!(config.accounts["work"].models(), None);
    }

    /// A path that does not exist is an empty catalogue, not a crash.
    #[test]
    fn a_missing_config_is_an_empty_catalogue() {
        let loaded = load(Some(Path::new("/nonexistent/gateway.toml")))
            .expect("a missing file is not an error");
        assert!(loaded.config.accounts.is_empty());
        assert!(!loaded.present);
        assert!(
            loaded.note().contains("does not exist"),
            "{}",
            loaded.note()
        );
    }

    /// A configured provider overrides the built-in template of the same
    /// name rather than sitting behind it.
    #[test]
    fn a_configured_provider_overrides_its_template() {
        let config: GatewayConfig = toml::from_str(
            r#"
[providers.openrouter]
base_url = "http://127.0.0.1:1"
protocol = "openai-chat"
"#,
        )
        .expect("parses");
        let providers = providers(&config);
        let openrouter: Vec<_> = providers
            .iter()
            .filter(|p| p.name == "openrouter")
            .collect();
        assert_eq!(openrouter.len(), 1, "one entry, not two");
        assert_eq!(openrouter[0].protocols[0].base_url, "http://127.0.0.1:1");
    }
}
