//! `inference-gateway` — the gateway as its own process.
//!
//! # The contract Sterna holds this binary to
//!
//! ```text
//! inference-gateway serve [--listen 127.0.0.1:0] [--config <path>]
//! ```
//!
//! Long-running. It prints **exactly one line to stdout** when it is ready
//! to serve, and then nothing more on stdout ever:
//!
//! ```json
//! {"listening":"http://127.0.0.1:PORT","token":"<bearer>"}
//! ```
//!
//! The caller sets its client's base URL to `listening` and its bearer token
//! to `token`, then serves through it. This process exits `0` when **stdin
//! reaches EOF** or a termination signal arrives, and every diagnostic it
//! has goes to **stderr**. Those three facts — one line, that shape, stdin
//! as the shutdown channel — are the whole interprocess protocol, and they
//! are what [`serve`] is arranged around: stdout is written once and then
//! left alone, and the shutdown wait is a channel fed by both a stdin reader
//! and a signal handler so that neither can be missed while the other is
//! being waited on.
//!
//! stdin is the shutdown channel rather than a signal because a parent that
//! dies takes its child's stdin with it. A gateway that outlived the process
//! that spawned it would keep a loopback port and a set of resolved
//! credentials alive with nobody to answer for them.
//!
//! # What this binary may decide, and what it may not
//!
//! It may choose the provider, the account and the entitlement a request is
//! served by. It may **not** change the model or the effort the caller asked
//! for unless the caller's own fallback policy permits it — that decision
//! lives in `routing::interactive` and this file adds no second path to it.
//! There is no project scope here, no session memory and no harness
//! identity: a standalone gateway serves HTTP clients and does not know what
//! any of them is.

use std::io::{Read, Write};
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand, ValueEnum};
use serde::Serialize;

use inference_gateway::config::{self, GatewayConfig};
use inference_gateway::entitlement::{EntitlementKind, EntitlementVendor};
use inference_gateway::gateway::subscription_broker::RunningSubscriptionBroker;
use inference_gateway::gateway::{self, BackendDemand};
use inference_gateway::pool::{self, Pool};
use inference_gateway::provider::cache::{
    ModelCache, ModelCatalogue, ModelEntry, now_unix_seconds,
};
use inference_gateway::secret::file::FileSecretStore;
use inference_gateway::secret::native::{PreferNativeSecretStore, Presence, SourceKind};
use inference_gateway::secret::{SecretRef, SecretStore};
use inference_gateway::subscription::connect as flow;

/// The gateway as its own process: one wire format in, many providers out.
#[derive(Debug, Parser)]
#[command(name = "inference-gateway", version, about, long_about = None)]
struct Cli {
    /// The configuration file to read, instead of the platform location.
    #[arg(long, global = true, value_name = "PATH")]
    config: Option<PathBuf>,
    /// Private state root for subscription brokers and cached catalogues,
    /// instead of the platform location.
    #[arg(long, global = true, value_name = "PATH")]
    data_dir: Option<PathBuf>,
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Serve until stdin reaches EOF, printing one ready line first.
    Serve {
        // Clap renders this doc comment as `--help` text, so it says what a
        // user needs and not what a reader of the code does: `serve`'s own
        // doc comment carries the reasoning and the link.
        /// Where to listen. Only an ephemeral loopback port can be bound.
        #[arg(long, value_name = "ADDR", default_value = "127.0.0.1:0")]
        listen: String,
    },
    /// What each configured account is and what it can serve.
    Entitlements {
        /// Print the versioned JSON document instead of prose.
        #[arg(long)]
        json: bool,
        /// Read a model catalogue for every connected subscription account
        /// that has none cached.
        #[arg(long)]
        refresh: bool,
    },
    /// Published measurements for the models this gateway serves.
    Models {
        /// Print the versioned JSON document instead of prose.
        #[arg(long)]
        json: bool,
        /// Only the models whose names contain this text.
        #[arg(long, value_name = "TEXT")]
        filter: Option<String>,
        /// Replace this gateway's overlay with a catalogue read from PATH, or
        /// `-` for standard input -- what a user's own Artificial Analysis
        /// key fetched.
        #[arg(long, value_name = "PATH")]
        import: Option<PathBuf>,
    },
    /// Subscription accounts.
    Subscriptions {
        #[command(subcommand)]
        command: SubscriptionsCommand,
    },
    /// Destinations this gateway can send requests to.
    Providers {
        #[command(subcommand)]
        command: ProvidersCommand,
    },
    /// Provider API keys this gateway stores and resolves.
    Credentials {
        #[command(subcommand)]
        command: CredentialsCommand,
    },
    // Same rule as `Serve::listen`: this line is `--help` text. What a
    // standalone gateway can and cannot answer here is on [`routing_cost`].
    /// What routing has consumed, in the same JSON Lines a host emits.
    RoutingCost {
        /// How far back to look, in hours.
        #[arg(long, value_name = "N", default_value_t = 24)]
        hours: u32,
        /// One JSON object per observation, one per line.
        #[arg(long)]
        json: bool,
        /// Start the window at this Unix second instead of `--hours` ago.
        #[arg(long, value_name = "UNIX", conflicts_with = "hours", requires = "json")]
        since: Option<i64>,
        /// Keep only this session's rows.
        #[arg(long, value_name = "ID", requires = "json")]
        session: Option<String>,
    },
}

#[derive(Debug, Subcommand)]
enum SubscriptionsCommand {
    /// Connect a subscription account with the provider's OAuth flow.
    Connect {
        #[arg(value_enum)]
        provider: SubscriptionProvider,
        /// The `[accounts.<name>]` table to connect. Omitted: the provider's
        /// default account, declared in the configuration if it is not there.
        #[arg(long, value_name = "NAME")]
        entitlement: Option<String>,
        /// Emit each progress step as one JSON object per line.
        #[arg(long)]
        json: bool,
        /// Sign in with a code entered on any device (OpenAI only); the
        /// default when this machine has no browser to open.
        #[arg(long)]
        device_code: bool,
        /// Print the sign-in link instead of opening a browser.
        #[arg(long)]
        no_browser: bool,
    },
    /// Forget one account's login: its broker auth directory is emptied.
    Logout {
        #[arg(value_enum)]
        provider: SubscriptionProvider,
        /// The `[accounts.<name>]` table to log out.
        #[arg(long, value_name = "NAME")]
        entitlement: String,
    },
    /// How much of each subscription's limits is used: the plan, and every
    /// window the provider enforces with its reset time.
    Usage {
        /// Only this `[accounts.<name>]` table.
        #[arg(long, value_name = "NAME")]
        entitlement: Option<String>,
        /// One JSON document instead of prose.
        #[arg(long)]
        json: bool,
    },
    /// Take an account into its pool or out of it. Every account whose
    /// catalogue serves a model is in that model's pool unless taken out;
    /// the change reaches a serving gateway on its next request.
    Pool {
        /// The `[accounts.<name>]` table.
        #[arg(long, value_name = "NAME")]
        entitlement: String,
        /// Take it into the pool.
        #[arg(long, conflicts_with = "exclude")]
        include: bool,
        /// Take it out of the pool.
        #[arg(long)]
        exclude: bool,
    },
    /// Use one account's saved login once: start its broker, read its
    /// catalogue and send one small completion. Exit 0 only if it answered.
    Verify {
        /// The `[accounts.<name>]` table to verify.
        #[arg(long, value_name = "NAME")]
        entitlement: String,
    },
    /// Adopt a CLIProxyAPI executable into this gateway's managed tools, pinned by its digest.
    AdoptBinary {
        /// The executable to copy in.
        path: PathBuf,
    },
}

#[derive(Debug, Subcommand)]
enum ProvidersCommand {
    /// Declare a custom endpoint and an account that uses it; its key is then
    /// stored with `credentials set <name>`.
    Add {
        /// A short name: lower-case letters, digits and `-`.
        name: String,
        /// The endpoint's base URL, such as `https://api.example.com/v1`.
        #[arg(long, value_name = "URL")]
        base_url: String,
        /// What the endpoint speaks: `openai-chat`, `openai-responses` or
        /// `anthropic-messages`.
        #[arg(long, value_name = "PROTOCOL", default_value = "openai-chat")]
        protocol: String,
        /// Print one JSON object instead of prose.
        #[arg(long)]
        json: bool,
    },
}

#[derive(Debug, Subcommand)]
enum CredentialsCommand {
    /// Where each provider's credential comes from. Names only, never a value.
    List {
        /// Print the versioned JSON document instead of prose.
        #[arg(long)]
        json: bool,
    },
    /// Store an API key, read from stdin, in this gateway's credential file.
    Set {
        /// The provider, as `credentials list` names it. Optional when
        /// `--variable` names the variable directly.
        #[arg(required_unless_present = "variable")]
        provider: Option<String>,
        /// The variable to file it under; the provider's first by default.
        #[arg(long, value_name = "VAR")]
        variable: Option<String>,
        /// Print one JSON object instead of prose.
        #[arg(long)]
        json: bool,
    },
    /// Remove an API key from this gateway's credential file.
    Remove {
        /// The provider, as `credentials list` names it. Optional when
        /// `--variable` names the variable directly.
        #[arg(required_unless_present = "variable")]
        provider: Option<String>,
        /// The variable to remove; the provider's first by default.
        #[arg(long, value_name = "VAR")]
        variable: Option<String>,
        /// Print one JSON object instead of prose.
        #[arg(long)]
        json: bool,
    },
}

/// The vendor login flows this binary can drive, each through the broker's
/// own login (`subscription_broker::login::login_flag`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
enum SubscriptionProvider {
    Anthropic,
    Openai,
    Google,
    Kimi,
    Xai,
    Devin,
    Meta,
}

impl SubscriptionProvider {
    /// The spelling `subscription_broker::login::login_flag` keys on, and the one
    /// `entitlements --json` reports as `connect_with`. One table, so a row
    /// that command offers to connect is a row this one accepts.
    fn as_str(self) -> &'static str {
        match self {
            Self::Anthropic => "anthropic",
            Self::Openai => "openai",
            Self::Google => "google",
            Self::Kimi => "kimi",
            Self::Xai => "xai",
            Self::Devin => "devin",
            Self::Meta => "meta",
        }
    }
}

fn main() {
    if let Err(error) = run() {
        eprintln!("inference-gateway: {error:#}");
        std::process::exit(1);
    }
}

fn run() -> Result<()> {
    let cli = Cli::parse();
    match &cli.command {
        Command::Serve { listen } => {
            let config = load_config(&cli)?;
            serve(listen, &config, &data_dir(&cli)?, config_path(&cli).ok())
        }
        Command::Entitlements { json, refresh } => {
            let config = load_config(&cli)?;
            entitlements(&config, &data_dir(&cli)?, *json, *refresh)
        }
        Command::Models {
            json,
            filter,
            import,
        } => models(
            &data_dir(&cli)?,
            *json,
            filter.as_deref(),
            import.as_deref(),
        ),
        Command::Subscriptions {
            command:
                SubscriptionsCommand::Connect {
                    provider,
                    entitlement,
                    json,
                    device_code,
                    no_browser,
                },
        } => {
            let entitlement = match entitlement {
                Some(name) => name.clone(),
                None => declare_default_subscription(&cli, *provider)?,
            };
            let config = load_config(&cli)?;
            let how = ConnectHow {
                json: *json,
                device_code: *device_code,
                no_browser: *no_browser,
            };
            connect(&config, &data_dir(&cli)?, *provider, &entitlement, how)
        }
        Command::Providers {
            command:
                ProvidersCommand::Add {
                    name,
                    base_url,
                    protocol,
                    json,
                },
        } => add_provider(&cli, name, base_url, protocol, *json),
        Command::Subscriptions {
            command:
                SubscriptionsCommand::Logout {
                    provider,
                    entitlement,
                },
        } => {
            let config = load_config(&cli)?;
            logout(&config, &data_dir(&cli)?, *provider, entitlement)
        }
        Command::Subscriptions {
            command: SubscriptionsCommand::AdoptBinary { path },
        } => adopt_binary(&data_dir(&cli)?, path),
        Command::Subscriptions {
            command:
                SubscriptionsCommand::Pool {
                    entitlement,
                    include,
                    exclude,
                },
        } => {
            let config = load_config(&cli)?;
            if !config.accounts.contains_key(entitlement.as_str()) {
                bail!("no [accounts.{entitlement}] table in the gateway configuration");
            }
            let state = inference_gateway::provider::pool_state::PoolState::at(&data_dir(&cli)?);
            if *include || *exclude {
                state.set(entitlement, *include)?;
            }
            println!(
                "{entitlement}: {}",
                if state.excluded(entitlement) {
                    "out of its pool"
                } else {
                    "in its pool"
                }
            );
            Ok(())
        }
        Command::Subscriptions {
            command: SubscriptionsCommand::Usage { entitlement, json },
        } => {
            let config = load_config(&cli)?;
            subscription_usage(&config, &data_dir(&cli)?, entitlement.as_deref(), *json)
        }
        Command::Subscriptions {
            command: SubscriptionsCommand::Verify { entitlement },
        } => {
            let config = load_config(&cli)?;
            if !config.accounts.contains_key(entitlement.as_str()) {
                bail!("no [accounts.{entitlement}] table in the gateway configuration");
            }
            verify_login(&data_dir(&cli)?, entitlement)?;
            println!("{entitlement}: the saved login works");
            Ok(())
        }
        Command::Credentials { command } => {
            let config = load_config(&cli)?;
            let data_dir = data_dir(&cli)?;
            match command {
                CredentialsCommand::List { json } => credentials_list(&config, &data_dir, *json),
                CredentialsCommand::Set {
                    provider,
                    variable,
                    json,
                } => {
                    credentials_set(
                        &config,
                        &data_dir,
                        provider.as_deref(),
                        variable.as_deref(),
                        *json,
                    )?;
                    // A key is only served through an account once any
                    // account is declared, so storing one declares its
                    // provider's -- unless an account already uses it.
                    if let Some(provider) = provider.as_deref() {
                        declare_key_account(&cli, &config, provider)?;
                    }
                    Ok(())
                }
                CredentialsCommand::Remove {
                    provider,
                    variable,
                    json,
                } => credentials_remove(
                    &config,
                    &data_dir,
                    provider.as_deref(),
                    variable.as_deref(),
                    *json,
                ),
            }
        }
        // Reads no configuration: it answers from what a serving process of
        // this same installation wrote down, which is the data directory and
        // nothing else.
        Command::RoutingCost {
            hours,
            json,
            since,
            session,
        } => routing_cost(&data_dir(&cli)?, *hours, *json, *since, session.as_deref()),
    }
}

/// The configuration, with the one line saying where it came from on stderr.
///
/// stderr and never stdout: `serve`'s stdout carries exactly one line and it
/// is the ready line, and a note printed before it would break the contract
/// for every caller that reads one line and stops.
fn load_config(cli: &Cli) -> Result<GatewayConfig> {
    let loaded = config::load(cli.config.as_deref())?;
    eprintln!("{}", loaded.note());
    Ok(loaded.config)
}

/// Where this invocation's configuration lives, whether or not it exists yet.
fn config_path(cli: &Cli) -> Result<PathBuf> {
    cli.config
        .clone()
        .or_else(config::default_config_path)
        .context("could not determine where the gateway configuration lives; pass --config")
}

/// The account a subscription sign-in connects when none is named, declared
/// in the configuration the first time -- so signing in needs no file edit.
fn declare_default_subscription(cli: &Cli, provider: SubscriptionProvider) -> Result<String> {
    let (name, body) = match provider {
        SubscriptionProvider::Openai => (
            "chatgpt-subscription",
            "kind = \"chatgpt\"\nvendor = \"openai\"\nsubscription_broker = \"cliproxyapi\"\n",
        ),
        SubscriptionProvider::Anthropic => (
            "claude-subscription",
            "kind = \"claude\"\nvendor = \"claude\"\nsubscription_broker = \"cliproxyapi\"\n",
        ),
        SubscriptionProvider::Google => (
            "gemini-subscription",
            "kind = \"gemini\"\nvendor = \"google\"\nsubscription_broker = \"cliproxyapi\"\n",
        ),
        SubscriptionProvider::Kimi => (
            "kimi-subscription",
            "kind = \"kimi\"\nvendor = \"moonshot\"\nsubscription_broker = \"cliproxyapi\"\n",
        ),
        SubscriptionProvider::Xai => (
            "grok-subscription",
            "kind = \"xai\"\nvendor = \"xai\"\nsubscription_broker = \"cliproxyapi\"\n",
        ),
        SubscriptionProvider::Devin => (
            "devin-subscription",
            "kind = \"devin\"\nvendor = \"cognition\"\nsubscription_broker = \"cliproxyapi\"\n",
        ),
        SubscriptionProvider::Meta => (
            "muse-subscription",
            "kind = \"meta\"\nvendor = \"meta\"\nsubscription_broker = \"cliproxyapi\"\n",
        ),
    };
    config::declare_table(&config_path(cli)?, &format!("accounts.{name}"), body)?;
    Ok(name.to_owned())
}

/// Declares `[accounts.<provider>]` for a provider whose key was just stored,
/// when no account names that provider yet; a provider that is not known
/// to this gateway declares nothing.
fn declare_key_account(cli: &Cli, config: &GatewayConfig, provider: &str) -> Result<()> {
    let known = config::providers(config).iter().any(|p| p.name == provider);
    let used = config
        .accounts
        .values()
        .any(|entry| entry.provider() == Some(provider));
    if known && !used && !config.accounts.contains_key(provider) {
        let quoted = toml::Value::String(provider.to_owned()).to_string();
        config::declare_table(
            &config_path(cli)?,
            &format!("accounts.{provider}"),
            &format!("provider = {quoted}\n"),
        )?;
    }
    Ok(())
}

/// `providers add`: a custom endpoint and the account that uses it. The key
/// is filed under `<NAME>_API_KEY`, which `credentials set <name>` stores.
fn add_provider(cli: &Cli, name: &str, base_url: &str, protocol: &str, json: bool) -> Result<()> {
    if name.is_empty()
        || !name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
    {
        bail!("a provider name is lower-case letters, digits and `-`");
    }
    if !matches!(
        protocol,
        "openai-chat" | "openai-responses" | "anthropic-messages"
    ) {
        bail!("the protocol is openai-chat, openai-responses or anthropic-messages");
    }
    if !(base_url.starts_with("https://") || base_url.starts_with("http://")) {
        bail!("the base URL starts with https:// or http://");
    }
    let variable = format!("{}_API_KEY", name.to_ascii_uppercase().replace('-', "_"));
    let quoted = |text: &str| toml::Value::String(text.to_owned()).to_string();
    let path = config_path(cli)?;
    let provider = config::declare_table(
        &path,
        &format!("providers.{name}"),
        &format!(
            "base_url = {}\nprotocol = {}\ncredential_env = [{}]\n",
            quoted(base_url),
            quoted(protocol),
            quoted(&variable)
        ),
    )?;
    let account = config::declare_table(
        &path,
        &format!("accounts.{name}"),
        &format!("provider = {}\n", quoted(name)),
    )?;
    if json {
        println!(
            "{}",
            serde_json::json!({"provider": name, "variable": variable, "declared": provider || account})
        );
    } else if provider || account {
        println!(
            "declared {name} ({protocol}, {base_url}); store its key with `credentials set {name}`"
        );
    } else {
        println!("{name} is already declared");
    }
    Ok(())
}

fn data_dir(cli: &Cli) -> Result<PathBuf> {
    match &cli.data_dir {
        Some(dir) => Ok(dir.clone()),
        None => config::default_data_dir()
            .context("could not determine a per-user application-data directory; pass --data-dir"),
    }
}

/// Why [`serve`] stopped. Both arrive on one channel so that neither can be
/// missed while the other is being waited on.
enum Stop {
    StdinEof,
    Signal,
}

/// The ready line's exact shape.
///
/// A struct rather than `serde_json::json!` because struct-field
/// serialization emits keys in declaration order regardless of feature
/// flags, and this line's shape is a contract another program parses.
#[derive(Serialize)]
struct Ready<'a> {
    listening: &'a str,
    token: &'a str,
}

/// Bind, announce, and serve until stdin reaches EOF or a signal arrives.
///
/// `--listen` may name only an **ephemeral loopback** address. The gateway
/// binds `127.0.0.1:0` and the operating system chooses the port; that is
/// what lets several instances coexist, and it is not a parameter the
/// library exposes. A fixed port is therefore refused by name rather than
/// silently ignored — a caller told "listening on 41219" when it asked for
/// 8080 would have been lied to about the one fact it needs.
fn serve(
    listen: &str,
    config: &GatewayConfig,
    data_dir: &Path,
    config_file: Option<PathBuf>,
) -> Result<()> {
    let address: SocketAddr = listen
        .parse()
        .with_context(|| format!("`--listen {listen}` is not a socket address"))?;
    if !address.ip().is_loopback() || address.port() != 0 {
        bail!(
            "`--listen {address}` cannot be honoured: this gateway binds a loopback port the \
             operating system chooses, which is what lets two instances coexist. Pass \
             `127.0.0.1:0`, or omit the flag"
        );
    }

    let secrets = secret_store(data_dir);
    eprintln!("credentials resolve through {}", secrets.describe());
    // Everything a rebuild needs, owned, so the supplier a deferred start
    // keeps can run again on a later request — see `gateway::UpstreamSlot`.
    // **Each build reads the configuration file again**, so an account a
    // sign-in declared, or an endpoint added, serves without a restart; the
    // copy read at start stands in only when there is no file to read.
    let build = {
        let startup = config.clone();
        let file = config_file.clone();
        let data_dir = data_dir.to_path_buf();
        // Which accounts the person took out of their pool, read live per
        // request so a toggle needs no restart.
        let pool_state = std::sync::Arc::new(
            inference_gateway::provider::pool_state::PoolState::at(&data_dir),
        );
        std::sync::Arc::new(move || {
            let current = match &file {
                Some(path) => config::load(Some(path))
                    .map(|loaded| loaded.config)
                    .map_err(|error| format!("{error:#}"))?,
                None => startup.clone(),
            };
            let providers = config::providers(&current);
            pool::pool_from_catalogue(
                &current.accounts,
                &providers,
                &secrets,
                &|entitlement| config::broker_paths(&data_dir, entitlement),
                // No free-tier marking: a standalone gateway is told nothing
                // about who pays, and `Cost::Metered` is that answer's
                // fail-closed default.
                &|_| false,
            )
            .map(|mut built| {
                let state = std::sync::Arc::clone(&pool_state);
                built.upstream = built
                    .upstream
                    .with_exclusion(std::sync::Arc::new(move |account| state.excluded(account)));
                built
            })
            .map_err(|refusal| refusal.to_string())
        })
    };
    let rebuild = std::sync::Arc::clone(&build);
    let gateway = match build() {
        Ok(Pool { upstream, notes }) => {
            for note in notes {
                eprintln!("{note}");
            }
            gateway::start_if_required_with_degrade_sink(
                &[BackendDemand::LocalGateway],
                || Ok(upstream),
                // What this process watches a provider do, written down for
                // its own next run: rate-limit readings, and -- through
                // `GatewayQuotaCache::context_limits` -- the context window a
                // route states when it refuses an over-long request. Without
                // a cache here both are observed and forgotten, and
                // `models --json` could only ever answer with a catalogue's
                // prior.
                Some(inference_gateway::provider::telemetry::GatewayQuotaCache::new(data_dir)),
                None,
                // This binary is the host of last resort. A gateway with a
                // real host emits to it; this one keeps what a turn cost, so
                // `routing-cost --json` can answer the one question a client
                // asks it -- see `turn_cost_sink`.
                Some(turn_cost_sink(data_dir)),
                None,
            )?
            .context("a gateway was required and none was started")?
        }
        // Nothing to forward to yet. Listen anyway: the one flow that stores
        // a credential — the client's own login control — needs the client
        // running, and the client waits for this ready line. Every request
        // is answered `503` with the refusal until a rebuild succeeds, and a
        // credential stored meanwhile is picked up without a restart.
        Err(refusal) => {
            let refusal = refusal.to_string();
            eprintln!("serving nothing yet: {refusal}");
            gateway::start_awaiting_upstream(
                refusal,
                move || {
                    build()
                        .map(|Pool { upstream, notes }| {
                            for note in notes {
                                eprintln!("{note}");
                            }
                            upstream
                        })
                        .map_err(|refusal| refusal.to_string())
                },
                Some(turn_cost_sink(data_dir)),
            )?
        }
    };
    // One reload per change to the file: its modification time is what moved.
    let modified = move || {
        config_file
            .as_deref()
            .and_then(|path| std::fs::metadata(path).ok())
            .and_then(|metadata| metadata.modified().ok())
    };
    let seen = std::sync::Mutex::new(modified());
    gateway.reload_when(
        move || {
            rebuild().map(|Pool { upstream, notes }| {
                for note in notes {
                    eprintln!("{note}");
                }
                upstream
            })
        },
        move || {
            let now = modified();
            let mut last = seen
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let moved = *last != now;
            *last = now;
            moved
        },
    );
    if let Some(provider) = gateway.serving_provider() {
        eprintln!(
            "serving {provider} over {}",
            gateway.served_protocols().join(", ")
        );
    }

    let listening = gateway.base_url();
    let line = serde_json::to_string(&Ready {
        listening: &listening,
        token: gateway.token().expose(),
    })?;
    let mut stdout = std::io::stdout();
    writeln!(stdout, "{line}")?;
    stdout.flush()?;

    let reason = wait_for_shutdown();
    eprintln!(
        "stopping: {}",
        match reason {
            Stop::StdinEof => "stdin reached EOF",
            Stop::Signal => "a termination signal arrived",
        }
    );
    // Explicit, because this is the whole shutdown: it stops the accept
    // loop, joins its thread, releases the port, and drops every backend —
    // which kills and reaps any subscription sidecar and removes its
    // ephemeral serving directory.
    drop(gateway);
    Ok(())
}

/// Block until stdin reaches EOF or a termination signal arrives.
///
/// The stdin read runs on its own thread and reports through a channel, so
/// the signal handler — which runs on `ctrlc`'s thread, not in signal
/// context — can report through the same one. Waiting on stdin directly
/// would make a signal wake nothing, and waiting on a flag would make EOF
/// cost a poll.
///
/// Bytes that arrive on stdin are read and discarded rather than buffered:
/// the channel is EOF, not data, and a caller that piped something in must
/// not be able to grow this process's memory by doing so.
fn wait_for_shutdown() -> Stop {
    let (sender, receiver) = std::sync::mpsc::channel::<Stop>();
    let signal_sender = sender.clone();
    if let Err(error) = ctrlc::set_handler(move || {
        let _ = signal_sender.send(Stop::Signal);
    }) {
        eprintln!(
            "could not install a termination handler ({error}); closing stdin remains the way \
             to stop this gateway"
        );
    }
    std::thread::spawn(move || {
        let mut stdin = std::io::stdin().lock();
        let mut discard = [0_u8; 1024];
        loop {
            match stdin.read(&mut discard) {
                Ok(0) => break,
                Ok(_) => {}
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
                Err(_) => break,
            }
        }
        let _ = sender.send(Stop::StdinEof);
    });
    // A disconnected channel means both reporters are gone, which can only
    // happen once stdin's thread has ended: treat it as the EOF it is.
    receiver.recv().unwrap_or(Stop::StdinEof)
}

/// `models [--json] [--filter TEXT] [--import PATH]` — the published figures
/// this gateway knows for the models it serves.
///
/// The numbers come from [`inference_gateway::models`]: the snapshot baked
/// into this binary, overlaid by whatever the user's own Artificial Analysis
/// key last fetched. `--import` is how that overlay is written, from a
/// catalogue any process holding the key produced, so no key ever reaches
/// this one.
///
/// The JSON document is what a harness reads to tell a model which models it
/// may delegate to and what each is worth: a `version`, the catalogue's
/// `source`, `index_version` and `captured`, and a `models` object keyed by
/// normalised model name.
fn models(data_dir: &Path, json: bool, filter: Option<&str>, import: Option<&Path>) -> Result<()> {
    let mut stdout = std::io::stdout();
    if let Some(path) = import {
        let bytes = if path == Path::new("-") {
            let mut buffer = Vec::new();
            std::io::Read::read_to_end(&mut std::io::stdin(), &mut buffer)?;
            buffer
        } else {
            std::fs::read(path).with_context(|| format!("could not read {}", path.display()))?
        };
        let count = inference_gateway::models::import(data_dir, &bytes)
            .map_err(|e| anyhow::anyhow!("{e}"))?;
        writeln!(
            stdout,
            "imported {count} model(s) to {}",
            inference_gateway::models::overlay_path(data_dir).display()
        )?;
        return Ok(());
    }

    let mut measurements = inference_gateway::models::measurements(data_dir);
    if let Some(text) = filter {
        let needle = inference_gateway::models::normalise(text);
        measurements.models.retain(|id, _| id.contains(&needle));
    }
    if json {
        let document = serde_json::to_string(&serde_json::json!({
            "version": 1,
            "source": measurements.source,
            "index_version": measurements.index_version,
            "captured": measurements.captured,
            "fetched_at": measurements.fetched_at,
            "models": measurements.models,
            // What a route was watched enforcing, beside what a catalogue
            // published. Two blocks rather than one merged figure, so a
            // reader can always tell a measurement from a prior -- which is
            // what lets a context meter say whether its percentage is one.
            "observed": inference_gateway::models::observed(data_dir),
            // What each subscription account's own provider says it is
            // served with -- per plan, so beside the published figures
            // rather than merged into them.
            "served": inference_gateway::models::served(data_dir),
        }))?;
        writeln!(stdout, "{document}")?;
        return Ok(());
    }
    if measurements.models.is_empty() {
        writeln!(stdout, "no model measurements")?;
        return Ok(());
    }
    // Strongest first: the order a person reading this wants, and the order
    // a harness renders its roster in.
    let mut rows: Vec<_> = measurements.models.iter().collect();
    rows.sort_by(|(a, x), (b, y)| {
        y.intelligence
            .partial_cmp(&x.intelligence)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.cmp(b))
    });
    for (id, facts) in rows {
        let figure = |value: Option<f64>| {
            value.map_or_else(|| "-".to_string(), |value| format!("{value:.1}"))
        };
        writeln!(
            stdout,
            "{id}\tintelligence {}\tcoding {}\tusd/task {}",
            figure(facts.intelligence),
            figure(facts.coding),
            facts
                .cost_per_task_usd
                .map_or_else(|| "-".to_string(), |value| format!("{value:.4}")),
        )?;
    }
    Ok(())
}

/// `entitlements [--json] [--refresh]` — what each configured account is.
///
/// The JSON document is the one a host produces, key for key: a `version`,
/// and an `accounts` array whose entries carry `account`, `provider`,
/// `models`, `scope`, `selectable`, `unavailable_reason`, `authenticated`
/// and `connect_with`. Built with `serde_json::json!` for the same reason
/// the host builds it that way — the key order is the macro's, so the two
/// documents are the same bytes for the same catalogue.
///
/// Two fields answer differently here than in a host, and both are
/// deliberate. **`selectable` is always true** and `unavailable_reason` is
/// always null: a host pins a session to one entitlement for the life of a
/// launch, and a standalone gateway has no session to pin. **`scope` is
/// `unknown` until something has read a catalogue** — this process caches
/// what `--refresh` reads and reports `account-declared` or
/// `provider-declared` from that cache, and an empty list with `unknown`
/// claims nothing rather than claiming an account serves no model.
fn entitlements(config: &GatewayConfig, data_dir: &Path, json: bool, refresh: bool) -> Result<()> {
    let cache = ModelCache::at(config::model_cache_dir(data_dir));
    if refresh {
        refresh_catalogues(config, data_dir, &cache);
    }

    let pool_state = inference_gateway::provider::pool_state::PoolState::at(data_dir);
    let mut accounts = Vec::new();
    for (name, entry) in &config.accounts {
        let provider = match entry.subscription_broker() {
            Some(broker) => Some(
                entry
                    .vendor()
                    .map(EntitlementVendor::as_str)
                    .unwrap_or_else(|| broker.as_str())
                    .to_owned(),
            ),
            None => entry.provider().map(str::to_owned),
        };
        let (cached, scope) = if entry.subscription_broker().is_some() {
            (cache.load(name), "account-declared")
        } else if let Some(provider_name) = entry.provider() {
            (cache.load(provider_name), "provider-declared")
        } else {
            (None, "unknown")
        };
        let (mut models, scope) = match cached {
            Some(catalogue) => (
                catalogue
                    .models()
                    .iter()
                    .map(|model| model.id().to_owned())
                    .collect::<Vec<_>>(),
                scope,
            ),
            None => (Vec::new(), "unknown"),
        };
        models.sort();
        models.dedup();
        // Whether this account can be used at all, and if not, which flow
        // would fix it. A subscription with no credential is the row a
        // person most wants to act on.
        let connect_with = entry
            .subscription_broker()
            .and_then(|_| subscription_provider_for(entry.kind(), entry.vendor()))
            .map(|provider| provider.as_str().to_owned());
        let authenticated = connect_with
            .as_ref()
            .map(|_| credential_present(&config::broker_auth_dir(data_dir, name)).unwrap_or(false));
        accounts.push(serde_json::json!({
            "account": name,
            "provider": provider,
            "models": models,
            "scope": scope,
            "selectable": true,
            "unavailable_reason": Option::<String>::None,
            "authenticated": authenticated,
            "connect_with": connect_with,
            "pooled": !pool_state.excluded(name),
        }));
    }

    let mut stdout = std::io::stdout();
    if json {
        let document = serde_json::to_string(&serde_json::json!({
            "version": 1,
            "accounts": accounts,
        }))?;
        writeln!(stdout, "{document}")?;
        return Ok(());
    }
    if accounts.is_empty() {
        writeln!(stdout, "no accounts are configured")?;
        return Ok(());
    }
    for account in &accounts {
        writeln!(
            stdout,
            "{}\t{}\t{} model(s), {}",
            account["account"].as_str().unwrap_or_default(),
            account["provider"].as_str().unwrap_or("(none)"),
            account["models"].as_array().map(Vec::len).unwrap_or(0),
            account["scope"].as_str().unwrap_or_default(),
        )?;
    }
    Ok(())
}

/// Read a model catalogue for every connected subscription account that has
/// none cached.
///
/// **Missing, not stale** — the same rule a host applies: `--refresh` fills
/// a gap, it does not re-fetch what is already known, so asking twice costs
/// one sidecar start rather than two. Sequentially rather than in parallel:
/// a standalone catalogue is a handful of accounts, and starting several
/// CLIProxyAPI processes at once to save a second is not a trade worth the
/// moving parts.
///
/// Every failure is a line on stderr and never an error: one account whose
/// broker will not start must not stop the other three being reported.
fn refresh_catalogues(config: &GatewayConfig, data_dir: &Path, cache: &ModelCache) {
    for (name, entry) in &config.accounts {
        if entry.subscription_broker().is_none() || cache.load(name).is_some() {
            continue;
        }
        let paths = config::broker_paths(data_dir, name);
        if !credential_present(&paths.auth_dir).unwrap_or(false) {
            eprintln!("account `{name}`: not connected, so it has no catalogue to read");
            continue;
        }
        let broker = match RunningSubscriptionBroker::start(&paths, name) {
            Ok(broker) => broker,
            Err(error) => {
                eprintln!("account `{name}`: its broker would not start: {error}");
                continue;
            }
        };
        let base_url = format!("{}/v1", broker.base_url());
        let endpoint = format!("{base_url}/models");
        let document = match broker.model_catalogue_document() {
            Ok(document) => document,
            Err(error) => {
                eprintln!("account `{name}`: its model catalogue did not answer: {error}");
                continue;
            }
        };
        let models: Vec<ModelEntry> = pool::parse_model_catalogue(&document)
            .into_iter()
            .map(ModelEntry::new)
            .collect();
        if models.is_empty() {
            eprintln!("account `{name}`: its model catalogue named no model");
            continue;
        }
        let catalogue = ModelCatalogue::new(name, base_url, endpoint, now_unix_seconds(), models);
        if let Err(error) = cache.store(&catalogue) {
            eprintln!("account `{name}`: its catalogue could not be cached: {error}");
        }
    }
}

/// `subscriptions usage`: every subscription account (or one), read from the
/// provider's usage endpoint with its saved login.
fn subscription_usage(
    config: &GatewayConfig,
    data_dir: &Path,
    only: Option<&str>,
    json: bool,
) -> Result<()> {
    use inference_gateway::provider::subscription_usage;
    let mut usages = Vec::new();
    for (name, entry) in &config.accounts {
        if entry.subscription_broker().is_none() || only.is_some_and(|only| only != name) {
            continue;
        }
        usages.extend(subscription_usage::read_all(
            name,
            &config::broker_auth_dir(data_dir, name),
        ));
    }
    if let Some(only) = only
        && usages.is_empty()
    {
        bail!("no subscription account `{only}` in the gateway configuration");
    }
    if json {
        println!(
            "{}",
            serde_json::to_string(&serde_json::json!({ "schema_version": 1, "accounts": usages }))?
        );
        return Ok(());
    }
    for usage in &usages {
        let who = [usage.plan.as_deref(), usage.email.as_deref()]
            .into_iter()
            .flatten()
            .collect::<Vec<_>>()
            .join(", ");
        println!(
            "{}{}",
            usage.account,
            if who.is_empty() {
                String::new()
            } else {
                format!(" ({who})")
            }
        );
        if let Some(error) = &usage.error {
            println!("  {error}");
        }
        for window in &usage.windows {
            println!(
                "  {:<14} {:>5.1}% used{}",
                window.name,
                window.used_percent,
                window
                    .resets_at
                    .as_deref()
                    .map(|at| format!(", resets {at}"))
                    .unwrap_or_default()
            );
        }
    }
    Ok(())
}

/// Proves a fresh login by using it: the account's broker is started, its
/// catalogue read (and cached, so the models it now serves are listed at
/// once), and one small completion sent to a light model from it.
fn verify_login(data_dir: &Path, entitlement: &str) -> Result<()> {
    let paths = config::broker_paths(data_dir, entitlement);
    let broker = RunningSubscriptionBroker::start(&paths, entitlement)?;
    let document = broker.model_catalogue_document()?;
    let models = pool::parse_model_catalogue(&document);
    let model = probe_model(&models).context("the account's catalogue names no model to try")?;
    broker.verify_credential(&model)?;
    let cache = ModelCache::at(config::model_cache_dir(data_dir));
    let base_url = format!("{}/v1", broker.base_url());
    let endpoint = format!("{base_url}/models");
    let catalogue = ModelCatalogue::new(
        entitlement,
        base_url,
        endpoint,
        now_unix_seconds(),
        models.into_iter().map(ModelEntry::new).collect(),
    );
    if let Err(error) = cache.store(&catalogue) {
        eprintln!("account `{entitlement}`: its catalogue could not be cached: {error}");
    }
    Ok(())
}

/// The model a login is tried against: a light text model when the
/// catalogue has one, never an image or review model.
fn probe_model(models: &[String]) -> Option<String> {
    let text: Vec<&String> = models
        .iter()
        .filter(|m| {
            !["image", "review", "embedding", "spark"]
                .iter()
                .any(|w| m.contains(w))
        })
        .collect();
    ["haiku", "luna", "mini", "flash", "sonnet"]
        .iter()
        .find_map(|light| text.iter().find(|m| m.contains(light)))
        .or_else(|| text.first())
        .map(|m| (*m).clone())
}

/// How `subscriptions connect` was asked to run.
#[derive(Debug, Clone, Copy)]
struct ConnectHow {
    json: bool,
    device_code: bool,
    no_browser: bool,
}

/// `subscriptions connect <provider> --entitlement <name> [--json]
/// [--device-code] [--no-browser]`.
///
/// The broker's own login does the signing in, so the credential is written
/// in the format and through the TLS client the broker serves with. Every
/// line this writes is safe to show and to forward: a sign-in link, a device
/// code, a success or a failure. A line on this process's stdin is a pasted
/// callback address and goes to the broker, which is how a machine with no
/// browser finishes: open the link anywhere, sign in, paste where it landed.
fn connect(
    config: &GatewayConfig,
    data_dir: &Path,
    provider: SubscriptionProvider,
    entitlement: &str,
    how: ConnectHow,
) -> Result<()> {
    use inference_gateway::gateway::subscription_broker::login::{
        self, BrokerLogin, LoginOutput, Method,
    };
    use std::io::BufRead as _;

    broker_account(config, provider, entitlement)?;

    let mut out = std::io::stdout();
    let mut emit = |progress: &flow::Progress| {
        let line = if how.json {
            serde_json::to_string(progress).unwrap_or_else(|_| "{}".to_owned())
        } else {
            match progress {
                flow::Progress::Opened {
                    authorize_url,
                    browser_opened,
                } => format!(
                    "{}\n{authorize_url}\nNo browser on this machine? Open the link on any device, sign in, then paste the address the browser ends on here and press Enter.",
                    if *browser_opened {
                        "Sign in in the browser that just opened, or open this link:"
                    } else {
                        "Open this link to sign in:"
                    }
                ),
                flow::Progress::DeviceCode {
                    verification_url,
                    user_code,
                } => {
                    format!("On any device, open {verification_url} and enter the code {user_code}")
                }
                flow::Progress::Waiting { seconds_remaining } => {
                    format!("waiting for the browser ({seconds_remaining}s left)")
                }
                flow::Progress::Connected { account } => format!(
                    "connected{}",
                    account
                        .as_deref()
                        .map(|account| format!(" as {account}"))
                        .unwrap_or_default()
                ),
                flow::Progress::Failed { reason } => format!("failed: {reason}"),
            }
        };
        let _ = writeln!(out, "{line}");
        let _ = out.flush();
    };
    macro_rules! fail {
        ($reason:expr) => {{
            let reason: String = $reason;
            emit(&flow::Progress::Failed {
                reason: reason.clone(),
            });
            bail!(reason)
        }};
    }

    let browser = !how.no_browser && login::browser_available(|name| std::env::var_os(name));
    let method = if how.device_code
        || (!browser && provider == SubscriptionProvider::Openai && !how.no_browser)
    {
        Method::DeviceCode
    } else {
        Method::Browser
    };
    let Some(flag) = login::login_flag(provider.as_str(), method) else {
        fail!(format!(
            "`{}` has no device-code sign-in; open the link on any device and paste the address it ends on",
            provider.as_str()
        ));
    };
    let paths = config::broker_paths(data_dir, entitlement);
    let mut broker = match BrokerLogin::start(&paths, entitlement, flag) {
        Ok(broker) => broker,
        Err(error) => fail!(format!("{error:#}")),
    };
    if let Some(mut pasted) = broker.take_stdin() {
        std::thread::spawn(move || {
            for line in std::io::stdin().lock().lines().map_while(Result::ok) {
                if writeln!(pasted, "{}", line.trim()).is_err() || pasted.flush().is_err() {
                    break;
                }
            }
        });
    }
    let Some(stdout) = broker.take_stdout() else {
        fail!("the CLIProxyAPI login gave no output to read".to_owned());
    };

    let mut output = LoginOutput::default();
    let mut connected = false;
    let mut account = None;
    for line in std::io::BufReader::new(stdout)
        .lines()
        .map_while(Result::ok)
    {
        let Some(mut progress) = output.read(&line) else {
            continue;
        };
        match &mut progress {
            flow::Progress::Opened {
                authorize_url,
                browser_opened,
            } => *browser_opened = browser && login::open_in_browser(authorize_url),
            flow::Progress::DeviceCode {
                verification_url, ..
            } => {
                if browser {
                    login::open_in_browser(verification_url);
                }
            }
            // Held until the credential has been used once: a saved file
            // is not a working login (2026-09-23, a sign-in reported
            // connected that nobody had checked).
            flow::Progress::Connected { account: saved } => {
                connected = true;
                account = saved.clone();
                continue;
            }
            _ => {}
        }
        emit(&progress);
    }
    let status = broker.wait()?;
    if connected || (status.success() && credential_present(&paths.auth_dir).unwrap_or(false)) {
        return match verify_login(data_dir, entitlement) {
            Ok(()) => {
                emit(&flow::Progress::Connected { account });
                Ok(())
            }
            Err(error) => fail!(format!(
                "the sign-in saved a credential, but using it failed: {error:#}"
            )),
        };
    }
    let reason = output
        .failure()
        .map(str::to_owned)
        .unwrap_or_else(|| format!("the sign-in ended without a credential ({status})"));
    fail!(reason)
}

/// The observation sink a gateway with no other host installs.
///
/// **What crosses the sink is already everything a cost reader needs** —
/// `gateway::session` builds a `NewObservation` carrying the provider, the
/// model, the protocol, the credential label, the purpose, and the provider's
/// own input, output and cached token counts, from figures `gateway::usage`
/// read out of bytes the relay was forwarding anyway. Until now the standalone
/// binary passed `null_sink()` and every one of those figures was computed and
/// dropped, which is why `routing-cost` had nothing to print and why Sterna's
/// `ServedBy` was always unknown.
///
/// **The library still keeps nothing.** This closure is the binary's, on the
/// host's side of the sink exactly as the type's own doc requires; the gateway
/// module neither knows a store exists nor gains a way to reach one. What it
/// writes is the same small per-provider JSON cache the quota and
/// context-limit caches already are, and it is installed beside them so that a
/// gateway told to keep no telemetry keeps no rows either.
///
/// A degrade observation is not a served turn and is dropped here, which is
/// the whole of this sink's filtering.
fn turn_cost_sink(data_dir: &Path) -> gateway::ObservationSink {
    let ledger = inference_gateway::provider::telemetry::TurnCostLedger::new(data_dir);
    Arc::new(move |observation| {
        let gateway::Observation::Routed {
            observation,
            observed_at_unix,
        } = observation
        else {
            return;
        };
        let provider = observation.provider.clone();
        // Absent is never zero: a count this gateway could not read stays
        // unstated all the way to the reader, the same rule the observation
        // itself follows.
        let count = |value: Option<i64>| value.and_then(|value| u64::try_from(value).ok());
        ledger.append(
            &provider,
            inference_gateway::provider::telemetry::TurnCost {
                observed_at_unix,
                model: observation.model.clone(),
                route: observation.route.clone(),
                quota_context: observation.quota_context.clone(),
                purpose: observation.purpose.clone(),
                input_tokens: count(observation.input_tokens),
                output_tokens: count(observation.output_tokens),
                cached_input_tokens: count(observation.cached_input_tokens),
            },
        );
    })
}

/// `routing-cost --json --since <unix>` — the standalone reading of what
/// [`turn_cost_sink`] kept.
///
/// **`--json` is JSON Lines with no wrapper and no summary**, ascending by the
/// second each exchange completed, because the client takes the *last* row in
/// the window as the one closest to the request it is answering for. An empty
/// window prints nothing and exits `0`, so a caller parsing line by line needs
/// no special case for it — and a gateway that never served anything, or was
/// run without telemetry, is exactly that empty window rather than an error.
///
/// `--session` filters nothing here: a standalone gateway is told no session
/// id, so every row it keeps has none, and a filter on one would print an
/// empty window while implying the rows had been examined.
fn routing_cost(
    data_dir: &Path,
    hours: u32,
    json: bool,
    since: Option<i64>,
    session: Option<&str>,
) -> Result<()> {
    let ledger = inference_gateway::provider::telemetry::TurnCostLedger::new(data_dir);
    let since = since.unwrap_or_else(|| {
        let window = i64::from(hours) * 3_600;
        inference_gateway::provider::cache::now_unix_seconds().saturating_sub(window)
    });
    let rows = ledger.since(since);
    if let Some(session) = session {
        eprintln!(
            "routing-cost: this gateway records no session id, so `{session}` selects nothing; \
             printing every row in the window"
        );
    }
    let mut stdout = std::io::stdout();
    if json {
        for (provider, row) in &rows {
            writeln!(stdout, "{}", cost_row_json(provider, row))?;
        }
        return Ok(());
    }
    if rows.is_empty() {
        writeln!(
            stdout,
            "no routing observations in this window: nothing has been served through this gateway \
             since the Unix second {since}"
        )?;
        return Ok(());
    }
    for (provider, row) in &rows {
        let counts = match (row.input_tokens, row.output_tokens) {
            (Some(input), Some(output)) => match row.cached_input_tokens {
                Some(cached) => format!("{input} in ({cached} cached), {output} out"),
                None => format!("{input} in, {output} out"),
            },
            // Unknown, never zero — the provider stated no usage, or its
            // protocol has no spelling this gateway reads.
            _ => "usage unstated".to_string(),
        };
        writeln!(
            stdout,
            "{observed}  {provider}  {model}  {counts}",
            observed = row.observed_at_unix,
            model = row.model
        )?;
    }
    Ok(())
}

/// One row as a client reads it — **the wire shape, and it is a contract**.
///
/// Sterna's `gateway::served_by` parses these keys by name and ignores the rest,
/// so a key renamed here is a figure silently lost there rather than an error
/// anywhere. `cached_input_tokens` is the one that matters most: it is absent
/// from every other path back to a client on an OpenAI-family route, whose
/// body spells it `cached_tokens`.
///
/// A count the provider never stated stays `null` — absent, never zero.
fn cost_row_json(
    provider: &str,
    row: &inference_gateway::provider::telemetry::TurnCost,
) -> serde_json::Value {
    serde_json::json!({
        "provider": provider,
        "model": row.model,
        "route": row.route,
        "quota_context": row.quota_context,
        "purpose": row.purpose,
        "observed_at": row.observed_at_unix,
        "input_tokens": row.input_tokens,
        "output_tokens": row.output_tokens,
        "cached_input_tokens": row.cached_input_tokens,
    })
}

/// The vendor login flow an account's `kind`/`vendor` selects, or `None` for
/// an account no subscription broker can connect.
///
/// One table, read by `entitlements --json`'s `connect_with` and by
/// `connect`'s own validation, so a row the first offers to connect is a row
/// the second accepts.
fn subscription_provider_for(
    kind: Option<EntitlementKind>,
    vendor: Option<EntitlementVendor>,
) -> Option<SubscriptionProvider> {
    match (kind, vendor) {
        (Some(EntitlementKind::Claude), None | Some(EntitlementVendor::Claude))
        | (None, Some(EntitlementVendor::Claude)) => Some(SubscriptionProvider::Anthropic),
        (Some(EntitlementKind::ChatGpt), None | Some(EntitlementVendor::OpenAi))
        | (None, Some(EntitlementVendor::OpenAi)) => Some(SubscriptionProvider::Openai),
        (Some(EntitlementKind::Gemini), None | Some(EntitlementVendor::Google))
        | (None, Some(EntitlementVendor::Google)) => Some(SubscriptionProvider::Google),
        (Some(EntitlementKind::Kimi), None | Some(EntitlementVendor::Moonshot))
        | (None, Some(EntitlementVendor::Moonshot)) => Some(SubscriptionProvider::Kimi),
        (Some(EntitlementKind::Xai), None | Some(EntitlementVendor::Xai))
        | (None, Some(EntitlementVendor::Xai)) => Some(SubscriptionProvider::Xai),
        (Some(EntitlementKind::Devin), None | Some(EntitlementVendor::Cognition))
        | (None, Some(EntitlementVendor::Cognition)) => Some(SubscriptionProvider::Devin),
        (Some(EntitlementKind::Meta), None | Some(EntitlementVendor::Meta))
        | (None, Some(EntitlementVendor::Meta)) => Some(SubscriptionProvider::Meta),
        _ => None,
    }
}

/// Whether `dir` holds at least one regular file, which is the whole of what
/// "connected" can honestly mean.
///
/// Presence, never validity: an expired OAuth token is still a file on disk,
/// so this proves only that a login once happened. Reading the token to say
/// more would put account material back inside this process, which is the
/// one thing the broker design exists to prevent.
///
/// Refuses a symlink or a non-directory rather than following it: the auth
/// directory is private state, and reporting through a symlink would be
/// reporting about a location the user did not choose.
/// The store every command resolves through: the gateway's own credential
/// file first, then the native store, then the process environment.
fn secret_store(data_dir: &Path) -> PreferNativeSecretStore {
    PreferNativeSecretStore::detect_with_file(config::credentials_path(data_dir))
}

/// One row per (provider, variable): where the credential comes from, never
/// what it is. `native_store` is the diagnostic for the one state a user
/// cannot otherwise see — an item that exists and is refused to this build.
fn credentials_list(config: &GatewayConfig, data_dir: &Path, json: bool) -> Result<()> {
    let store = secret_store(data_dir);
    let mut rows = Vec::new();
    for provider in config::providers(config) {
        for var in &provider.credential_env {
            let reference = SecretRef::Environment { var: var.clone() };
            let source = store.source_kind(&reference);
            let native_store = match store.native() {
                Ok(native) => match native.presence(&reference) {
                    Presence::Present => "present",
                    Presence::Absent => "absent",
                    Presence::Refused => "refused",
                },
                Err(_) => "unavailable",
            };
            rows.push((provider.name.clone(), var.clone(), source, native_store));
        }
    }
    let mut stdout = std::io::stdout();
    if json {
        let providers: Vec<serde_json::Value> = rows
            .iter()
            .map(|(provider, variable, source, native_store)| {
                serde_json::json!({
                    "provider": provider,
                    "variable": variable,
                    "source": source.map(SourceKind::as_str),
                    "native_store": native_store,
                })
            })
            .collect();
        writeln!(
            stdout,
            "{}",
            serde_json::json!({ "version": 1, "providers": providers })
        )?;
        return Ok(());
    }
    if rows.is_empty() {
        writeln!(
            stdout,
            "no configured provider declares a credential variable"
        )?;
        return Ok(());
    }
    for (provider, variable, source, native_store) in rows {
        let state = match source {
            Some(kind) => format!("stored in {}", kind.describe()),
            None if native_store == "refused" => {
                "a native-store item exists that this build may not read; store it again".to_owned()
            }
            None => "not set".to_owned(),
        };
        writeln!(stdout, "{provider}\t{variable}\t{state}")?;
    }
    Ok(())
}

/// The variable a provider's key is filed under: the one `--variable`
/// names, or the first the provider declares. A provider that declares
/// none takes no key, and one that is not configured or built in is named
/// as such rather than silently created.
fn credential_variable(
    config: &GatewayConfig,
    provider: Option<&str>,
    variable: Option<&str>,
) -> Result<String> {
    // A bare `--variable` files the key under that name and validates
    // nothing: a host migrating a store keyed by variable name has no
    // provider to name, and the name is what resolution reads anyway.
    let Some(provider) = provider else {
        return variable
            .map(str::to_owned)
            .ok_or_else(|| anyhow::anyhow!("name a provider or `--variable`"));
    };
    let providers = config::providers(config);
    let Some(entry) = providers.iter().find(|entry| entry.name == provider) else {
        bail!(
            "`{provider}` is neither a configured provider nor a built-in template; \
             `credentials list` names them"
        );
    };
    if entry.credential_env.is_empty() {
        bail!("`{provider}` declares no credential variable, so it takes no API key");
    }
    match variable {
        None => Ok(entry.credential_env[0].clone()),
        Some(var) if entry.credential_env.iter().any(|declared| declared == var) => {
            Ok(var.to_owned())
        }
        Some(var) => bail!(
            "`{provider}` reads {}, not `{var}`",
            entry.credential_env.join(" or ")
        ),
    }
}

/// Store the key on stdin under the provider's variable. **Stdin and never
/// an argument**: an argument is in every process listing and every shell
/// history. The value is held for this call and printed by nothing.
fn credentials_set(
    config: &GatewayConfig,
    data_dir: &Path,
    provider: Option<&str>,
    variable: Option<&str>,
    json: bool,
) -> Result<()> {
    let variable = credential_variable(config, provider, variable)?;
    let provider = provider.unwrap_or("-");
    let mut key = String::new();
    std::io::stdin()
        .read_to_string(&mut key)
        .context("reading the key from stdin")?;
    let key = key.trim_end_matches(['\r', '\n']);
    if key.is_empty() {
        bail!(
            "no key arrived on stdin; pipe it in: `printf %s \"$KEY\" | inference-gateway \
             credentials set {provider}`"
        );
    }
    if key.chars().any(char::is_control) {
        bail!("the key contains a line break or a control character; a key is one line");
    }
    let store = FileSecretStore::at(config::credentials_path(data_dir));
    store.store(&variable, key)?;
    let mut stdout = std::io::stdout();
    if json {
        writeln!(
            stdout,
            "{}",
            serde_json::json!({
                "provider": provider,
                "variable": variable,
                "stored_in": store.path().display().to_string(),
            })
        )?;
    } else {
        writeln!(
            stdout,
            "stored {variable} for {provider} in {}",
            store.path().display()
        )?;
    }
    Ok(())
}

fn credentials_remove(
    config: &GatewayConfig,
    data_dir: &Path,
    provider: Option<&str>,
    variable: Option<&str>,
    json: bool,
) -> Result<()> {
    let variable = credential_variable(config, provider, variable)?;
    let provider = provider.unwrap_or("-");
    let store = FileSecretStore::at(config::credentials_path(data_dir));
    let removed = store.remove(&variable)?;
    let mut stdout = std::io::stdout();
    if json {
        writeln!(
            stdout,
            "{}",
            serde_json::json!({ "provider": provider, "variable": variable, "removed": removed })
        )?;
    } else if removed {
        writeln!(stdout, "removed {variable} for {provider}")?;
    } else {
        writeln!(stdout, "nothing was stored for {provider} under {variable}")?;
    }
    Ok(())
}

/// The account a login flow acts on: configured, broker-backed, and of the
/// vendor the flow is for. Writing an Anthropic credential into a ChatGPT
/// account would produce a login that succeeded and a route that never
/// worked, so the mismatch is refused by name — for `connect` and `logout`
/// alike.
fn broker_account<'a>(
    config: &'a GatewayConfig,
    provider: SubscriptionProvider,
    entitlement: &str,
) -> Result<&'a inference_gateway::entitlement::AccountEntry> {
    let Some(entry) = config.accounts.get(entitlement) else {
        bail!("no `[accounts.{entitlement}]` table is configured, so there is nothing to connect");
    };
    if entry.subscription_broker().is_none() {
        bail!("account `{entitlement}` is not backed by a subscription broker, so it has no login");
    }
    if let Some(expected) = subscription_provider_for(entry.kind(), entry.vendor())
        && expected != provider
    {
        bail!(
            "account `{entitlement}` is connected with `{}`, not `{}`",
            expected.as_str(),
            provider.as_str()
        );
    }
    Ok(entry)
}

/// Forget an account's broker login. The auth directory is moved aside and
/// deleted rather than emptied in place, so a broker mid-read never sees a
/// half-removed directory, and it is recreated empty and private so the
/// next `connect` has somewhere to write.
fn logout(
    config: &GatewayConfig,
    data_dir: &Path,
    provider: SubscriptionProvider,
    entitlement: &str,
) -> Result<()> {
    broker_account(config, provider, entitlement)?;
    let paths = config::broker_paths(data_dir, entitlement);
    for dir in [&paths.brokers_dir, &paths.entitlement_dir] {
        refuse_unless_real_directory_or_absent(dir)?;
    }
    let auth = &paths.auth_dir;
    match std::fs::symlink_metadata(auth) {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_dir() => {
            bail!(
                "subscription auth location {} is not a private directory",
                auth.display()
            );
        }
        Ok(_) => {
            let tombstone = auth.with_file_name(format!(
                ".auth-removed-{}-{}",
                std::process::id(),
                now_unix_seconds()
            ));
            std::fs::rename(auth, &tombstone)
                .with_context(|| format!("could not move {} aside", auth.display()))?;
            std::fs::remove_dir_all(&tombstone)
                .with_context(|| format!("could not remove {}", tombstone.display()))?;
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => {
            return Err(error).with_context(|| format!("could not inspect {}", auth.display()));
        }
    }
    private_directory(auth)?;
    println!("{}\t{entitlement}\tabsent", provider.as_str());
    Ok(())
}

/// Copy a CLIProxyAPI executable to `tools/cliproxyapi/sha256-<digest>/`
/// and point the `current` marker at it — the layout
/// `config::cliproxyapi_executable` reads, so the next broker start finds
/// it with no environment variable. Pinned by digest, so two adoptions of
/// one build share one copy and an adoption of another never overwrites it.
fn adopt_binary(data_dir: &Path, source: &Path) -> Result<()> {
    use sha2::Digest as _;
    let metadata = std::fs::metadata(source).with_context(|| {
        format!(
            "CLIProxyAPI executable {} is not readable",
            source.display()
        )
    })?;
    if !metadata.is_file() {
        bail!(
            "CLIProxyAPI executable {} is not a regular file",
            source.display()
        );
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        if metadata.permissions().mode() & 0o111 == 0 {
            bail!(
                "CLIProxyAPI executable {} is not executable",
                source.display()
            );
        }
    }
    let bytes =
        std::fs::read(source).with_context(|| format!("could not read {}", source.display()))?;
    let version = format!("sha256-{}", hex::encode(sha2::Sha256::digest(&bytes)));
    let root = data_dir.join("tools").join("cliproxyapi");
    let version_dir = root.join(&version);
    private_directory(&root)?;
    private_directory(&version_dir)?;
    let destination = version_dir.join(if cfg!(windows) {
        "cliproxyapi.exe"
    } else {
        "cliproxyapi"
    });
    if !destination.exists() {
        let temporary = version_dir.join(format!(".adopt-{}", std::process::id()));
        let placed = (|| -> Result<()> {
            std::fs::write(&temporary, &bytes)?;
            std::fs::set_permissions(&temporary, metadata.permissions())?;
            std::fs::rename(&temporary, &destination)?;
            Ok(())
        })();
        if placed.is_err() {
            let _ = std::fs::remove_file(&temporary);
        }
        placed.with_context(|| format!("could not adopt into {}", version_dir.display()))?;
    }
    let marker = root.join("current");
    let temporary = root.join(format!(".current-{}", std::process::id()));
    std::fs::write(&temporary, version.as_bytes())
        .with_context(|| format!("could not write {}", temporary.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(&temporary, std::fs::Permissions::from_mode(0o600))?;
    }
    std::fs::rename(&temporary, &marker)
        .with_context(|| format!("could not point {} at {version}", marker.display()))?;
    println!("{}", destination.display());
    Ok(())
}

/// Create `path` as a directory only its owner can enter, and refuse a
/// symlink or a file in its place — the same rule the broker applies to
/// every private directory it opens.
fn private_directory(path: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt as _;
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(path)
            .with_context(|| format!("could not create {}", path.display()))?;
    }
    #[cfg(not(unix))]
    std::fs::create_dir_all(path)
        .with_context(|| format!("could not create {}", path.display()))?;
    refuse_unless_real_directory_or_absent(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))
            .with_context(|| format!("could not make {} private", path.display()))?;
    }
    Ok(())
}

fn refuse_unless_real_directory_or_absent(path: &Path) -> Result<()> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_dir() => bail!(
            "subscription broker private directory {} is not a real directory",
            path.display()
        ),
        Ok(_) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error).with_context(|| format!("could not inspect {}", path.display())),
    }
}

fn credential_present(dir: &Path) -> Result<bool> {
    let metadata = match std::fs::symlink_metadata(dir) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error).with_context(|| format!("could not inspect {dir:?}")),
    };
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        bail!("subscription auth location {dir:?} is not a private directory");
    }
    for entry in std::fs::read_dir(dir).with_context(|| format!("could not inspect {dir:?}"))? {
        let entry = entry?;
        if entry.file_type()?.is_file() {
            return Ok(true);
        }
    }
    Ok(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn storing_a_key_declares_its_providers_account_once() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("gateway.toml");
        std::fs::write(&path, "[accounts.work]\nprovider = \"groq\"\n").unwrap();
        let cli = Cli::parse_from([
            "inference-gateway",
            "--config",
            path.to_str().unwrap(),
            "entitlements",
        ]);
        let config = config::load(Some(&path)).unwrap().config;
        declare_key_account(&cli, &config, "typesafe").unwrap();
        declare_key_account(&cli, &config, "groq").unwrap();
        declare_key_account(&cli, &config, "no-such-provider").unwrap();
        let after = config::load(Some(&path)).unwrap().config;
        assert_eq!(after.accounts["typesafe"].provider(), Some("typesafe"));
        assert!(
            !after.accounts.contains_key("groq"),
            "an account already uses groq"
        );
        assert!(!after.accounts.contains_key("no-such-provider"));
    }

    #[test]
    fn signing_in_with_no_account_named_declares_the_providers_default_one() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("gateway.toml");
        let cli = Cli::parse_from([
            "inference-gateway",
            "--config",
            path.to_str().unwrap(),
            "entitlements",
        ]);
        let every = [
            (SubscriptionProvider::Openai, "chatgpt-subscription"),
            (SubscriptionProvider::Anthropic, "claude-subscription"),
            (SubscriptionProvider::Google, "gemini-subscription"),
            (SubscriptionProvider::Kimi, "kimi-subscription"),
            (SubscriptionProvider::Xai, "grok-subscription"),
            (SubscriptionProvider::Devin, "devin-subscription"),
            (SubscriptionProvider::Meta, "muse-subscription"),
        ];
        for (provider, name) in every {
            assert_eq!(declare_default_subscription(&cli, provider).unwrap(), name);
        }
        let config = config::load(Some(&path)).unwrap().config;
        for (provider, name) in every {
            let entry = &config.accounts[name];
            assert!(entry.subscription_broker().is_some(), "{name}");
            // Declared with the kind and vendor its own flow connects.
            assert_eq!(
                subscription_provider_for(entry.kind(), entry.vendor()),
                Some(provider),
                "{name}"
            );
        }
    }

    /// A login is tried against a light text model, never an image or
    /// review model, and any text model when no light one is listed.
    #[test]
    fn a_login_is_tried_against_a_light_text_model() {
        let ids = |list: &[&str]| list.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(
            probe_model(&ids(&[
                "gpt-image-2",
                "codex-auto-review",
                "gpt-6-sol",
                "gpt-6-luna"
            ])),
            Some("gpt-6-luna".into())
        );
        assert_eq!(
            probe_model(&ids(&["claude-opus-5-5", "claude-haiku-4-5-20251001"])),
            Some("claude-haiku-4-5-20251001".into())
        );
        assert_eq!(
            probe_model(&ids(&["gpt-image-2", "big-model"])),
            Some("big-model".into())
        );
        assert_eq!(probe_model(&ids(&["gpt-image-2"])), None);
    }

    /// What a served turn costs reaches disk, including the figure two
    /// dogfooding sessions could never see.
    ///
    /// The sink is where the fix lives: every count below was already computed
    /// by `gateway::usage` and folded into the observation by
    /// `gateway::session`, and `null_sink()` dropped all of it.
    #[test]
    fn a_served_turn_is_kept_with_the_cached_figure_the_provider_stated() {
        use inference_gateway::routing::evidence::NewObservation;

        let dir = tempfile::tempdir().expect("a temporary data directory");
        let sink = turn_cost_sink(dir.path());
        sink(gateway::Observation::Routed {
            observation: Box::new(
                NewObservation::new("chatgpt-subscription".to_owned(), "gpt-5.6-sol".to_owned())
                    .with_route(Some("openai-responses"))
                    .with_quota_context(Some("chatgpt-subscription"))
                    .with_purpose(Some("harness-turn"))
                    .with_tokens(Some(52_000), Some(900), Some(48_000)),
            ),
            observed_at_unix: 1_789_000_000,
        });

        let rows = inference_gateway::provider::telemetry::TurnCostLedger::new(dir.path())
            .since(1_789_000_000);
        assert_eq!(rows.len(), 1, "the sink kept the turn");
        let (provider, row) = &rows[0];
        assert_eq!(provider, "chatgpt-subscription");
        assert_eq!(row.model, "gpt-5.6-sol");
        assert_eq!(row.input_tokens, Some(52_000));
        assert_eq!(row.output_tokens, Some(900));
        assert_eq!(
            row.cached_input_tokens,
            Some(48_000),
            "the cached figure is the one this whole path exists to carry"
        );
    }

    /// The JSON Lines shape Sterna parses, asserted key by key.
    ///
    /// `sterna::gateway::served_by` reads these names off each line and ignores
    /// everything else, so this test is the only thing standing between a
    /// rename here and a figure that quietly stops arriving there.
    #[test]
    fn a_cost_row_carries_every_key_the_client_reads() {
        let row = inference_gateway::provider::telemetry::TurnCost {
            observed_at_unix: 1_789_000_000,
            model: "gpt-5.6-sol".to_owned(),
            route: Some("openai-responses".to_owned()),
            quota_context: Some("chatgpt-subscription".to_owned()),
            purpose: Some("harness-turn".to_owned()),
            input_tokens: Some(52_000),
            output_tokens: Some(900),
            cached_input_tokens: Some(48_000),
        };
        let line = cost_row_json("chatgpt-subscription", &row);
        assert_eq!(line["provider"], "chatgpt-subscription");
        assert_eq!(line["model"], "gpt-5.6-sol");
        assert_eq!(line["route"], "openai-responses");
        assert_eq!(line["quota_context"], "chatgpt-subscription");
        assert_eq!(line["input_tokens"], 52_000);
        assert_eq!(line["output_tokens"], 900);
        assert_eq!(
            line["cached_input_tokens"], 48_000,
            "the cached figure is why this ledger exists; a client that stops seeing it \
             falls back to a body that does not spell it"
        );
    }

    /// An unstated count reaches the client as `null`, never as a zero.
    #[test]
    fn an_unstated_count_is_null_on_the_wire() {
        let row = inference_gateway::provider::telemetry::TurnCost {
            observed_at_unix: 1_789_000_000,
            model: "kimi-k3".to_owned(),
            route: None,
            quota_context: None,
            purpose: None,
            input_tokens: None,
            output_tokens: None,
            cached_input_tokens: None,
        };
        let line = cost_row_json("groq", &row);
        assert!(line["input_tokens"].is_null());
        assert!(line["cached_input_tokens"].is_null());
    }

    /// A degrade is not a served turn, and the ledger is not a log.
    #[test]
    fn an_observation_that_is_not_a_served_turn_keeps_nothing() {
        let dir = tempfile::tempdir().expect("a temporary data directory");
        let sink = turn_cost_sink(dir.path());
        sink(gateway::Observation::Degraded {
            resource: "local-gateway".to_owned(),
            reason: inference_gateway::gateway::DegradeReason::Unreachable,
        });
        assert!(
            inference_gateway::provider::telemetry::TurnCostLedger::new(dir.path())
                .since(0)
                .is_empty()
        );
    }

    /// The ready line's shape, which is the whole interprocess contract:
    /// two keys, in this order, and the address is a loopback URL.
    #[test]
    fn the_ready_line_carries_exactly_the_two_keys_a_caller_reads() {
        let line = serde_json::to_string(&Ready {
            listening: "http://127.0.0.1:41219",
            token: "deadbeef",
        })
        .expect("a two-field struct serializes");
        assert_eq!(
            line,
            r#"{"listening":"http://127.0.0.1:41219","token":"deadbeef"}"#
        );
    }

    /// `--listen` is refused rather than ignored when it names something the
    /// library cannot bind, and the refusal names what was asked for.
    #[test]
    fn a_fixed_listen_port_is_refused_by_name() {
        let error = serve(
            "127.0.0.1:8080",
            &GatewayConfig::default(),
            Path::new("/nonexistent"),
            None,
        )
        .expect_err("a fixed port cannot be honoured");
        let rendered = error.to_string();
        assert!(rendered.contains("127.0.0.1:8080"), "{rendered}");
        assert!(rendered.contains("127.0.0.1:0"), "{rendered}");
    }

    /// A `kind`/`vendor` pair maps to one flow, and an account that states
    /// neither maps to none — so `connect_with` is never guessed.
    #[test]
    fn a_login_flow_is_selected_only_by_what_an_account_states() {
        assert_eq!(
            subscription_provider_for(Some(EntitlementKind::Claude), None),
            Some(SubscriptionProvider::Anthropic)
        );
        assert_eq!(
            subscription_provider_for(None, Some(EntitlementVendor::OpenAi)),
            Some(SubscriptionProvider::Openai)
        );
        assert_eq!(subscription_provider_for(None, None), None);
        assert_eq!(
            subscription_provider_for(Some(EntitlementKind::ApiKey), None),
            None
        );
    }

    /// This binary's own files are covered by the rule `lib.rs`'s header
    /// states — **nothing in this crate may name Glasshouse**.
    ///
    /// `gateway::tests::the_gateway_names_no_glasshouse_path` lists these
    /// three files too and pins its own length; this is the same two needles
    /// checked from the binary's own target, so the rule holds even for a
    /// `cargo test --bin` run that never builds the library's tests.
    #[test]
    fn the_binarys_own_files_name_no_glasshouse_path() {
        const FORBIDDEN: [&str; 2] = ["glasshouse::", "rusqlite"];
        for (name, source) in [
            ("main.rs", include_str!("main.rs")),
            ("pool.rs", include_str!("pool.rs")),
            ("config.rs", include_str!("config.rs")),
        ] {
            let production = source
                .split_once("#[cfg(test)]")
                .map_or(source, |(before, _)| before);
            for forbidden in FORBIDDEN {
                assert!(
                    !production.contains(forbidden),
                    "{name} names `{forbidden}` in production code"
                );
            }
        }
    }
}
