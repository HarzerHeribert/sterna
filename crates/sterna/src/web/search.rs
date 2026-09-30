//! `web.search`: one abstraction, two providers, the key never in a file
//! the model can read.
//!
//! **The invariant: a search answers with bounded excerpts that carry their
//! source URLs, every hit re-validated against the deny list and the
//! private-address rule (a hit from a host nobody allowed stays, and
//! fetching it asks), and the provider's key reaches only the request
//! header** — from the process environment first and the gateway's
//! credential file second (map 2657),
//! never `.sterna/config.toml`, argv, the rollout or a refusal. `brave` is the
//! keyed provider built first (plain HTTPS, one header); `searxng` stays as
//! the keyless one, a self-hosted endpoint. A provider the configuration
//! names but cannot ask — no endpoint, no key variable — is refused at
//! construction or, for a key that resolves to nothing, at the query, by the
//! variable's name.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::Deserialize;

use super::{SearchHit, SearchResult, WebBroker, WebConfig, encode};
use crate::tools::invoke::CancellationToken;

/// Brave's web search endpoint; the query goes in `q`, the key in
/// `X-Subscription-Token`.
pub const BRAVE_ENDPOINT: &str = "https://api.search.brave.com/res/v1/web/search";

/// The header a Brave key travels in.
pub const BRAVE_KEY_HEADER: &str = "X-Subscription-Token";

/// The variable a Brave key is read from when `[web] search_key_var` names
/// none.
pub const BRAVE_DEFAULT_KEY_VAR: &str = "BRAVE_SEARCH_API_KEY";

/// Hits per query, at most.
pub const MAX_HITS: usize = 20;

/// Bytes one hit's snippet may carry, so one verbose provider cannot fill a
/// cell.
pub const MAX_SNIPPET_BYTES: usize = 2048;

/// Which provider a configuration names, with what it needs to be asked.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SearchProvider {
    /// A SearXNG-compatible JSON endpoint; no credential.
    Searxng { endpoint: String },
    /// Brave Search, keyed; `key_var` is the variable's name.
    Brave { key_var: String },
}

impl SearchProvider {
    /// The provider `config` names, `None` when it names none, an error when
    /// it names one it cannot ask (an unknown name; `searxng` with no
    /// endpoint). Read at construction, so a session refuses to start on a
    /// configuration whose search would refuse every query.
    pub fn from_config(config: &WebConfig) -> Result<Option<Self>, String> {
        let provider = config
            .search_provider
            .as_deref()
            .map(str::trim)
            .filter(|name| !name.is_empty());
        match provider {
            None => Ok(config
                .search_endpoint
                .clone()
                .map(|endpoint| Self::Searxng { endpoint })),
            Some("searxng") => config
                .search_endpoint
                .clone()
                .map(|endpoint| Some(Self::Searxng { endpoint }))
                .ok_or_else(|| {
                    "web.search_provider = \"searxng\" needs web.search_endpoint".to_string()
                }),
            Some("brave") => {
                let key_var = config
                    .search_key_var
                    .as_deref()
                    .map(str::trim)
                    .filter(|name| !name.is_empty())
                    .unwrap_or(BRAVE_DEFAULT_KEY_VAR);
                if !key_var
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
                {
                    return Err(format!(
                        "web.search_key_var must be a variable name, not `{key_var}`"
                    ));
                }
                Ok(Some(Self::Brave {
                    key_var: key_var.to_string(),
                }))
            }
            Some(other) => Err(format!(
                "web.search_provider `{other}` is not a provider; `brave` or `searxng`"
            )),
        }
    }

    /// The provider's name as the rollout line carries it.
    pub fn name(&self) -> &'static str {
        match self {
            Self::Searxng { .. } => "searxng",
            Self::Brave { .. } => "brave",
        }
    }
}

/// One query against the configured provider.
pub(super) fn run(
    broker: &WebBroker,
    query: &str,
    token: &CancellationToken,
) -> Result<SearchResult, String> {
    let provider = SearchProvider::from_config(broker.config())?.ok_or(
        "web.search refused: no search provider is configured; set [web] search_provider and its key",
    )?;
    let raw = match &provider {
        SearchProvider::Searxng { endpoint } => {
            let sep = if endpoint.contains('?') { '&' } else { '?' };
            let url = format!("{endpoint}{sep}q={}&format=json", encode(query));
            let response = broker.get_endpoint(&url, &BTreeMap::new(), token)?;
            parse_searxng(&response.body)?
        }
        SearchProvider::Brave { key_var } => {
            let key = resolve_key(key_var).ok_or_else(|| {
                format!(
                    "web.search refused: no key for `brave`; set the variable `{key_var}` in the \
                     environment or store it with `inference-gateway credentials set --variable \
                     {key_var}`"
                )
            })?;
            let url = format!("{BRAVE_ENDPOINT}?q={}&count={MAX_HITS}", encode(query));
            let mut headers = BTreeMap::new();
            headers.insert(BRAVE_KEY_HEADER.to_string(), key);
            let response = broker.get_endpoint(&url, &headers, token)?;
            parse_brave(&response.body)?
        }
    };
    let results: Vec<SearchHit> = raw
        .into_iter()
        .filter(|hit| broker.validate_url(&hit.url).is_ok())
        .take(MAX_HITS)
        .map(|mut hit| {
            hit.snippet = bounded(&hit.snippet);
            hit
        })
        .collect();
    let citations = results.iter().map(|hit| hit.url.clone()).collect();
    Ok(SearchResult {
        query: query.into(),
        provider: provider.name().into(),
        results,
        citations,
        untrusted_content: true,
    })
}

/// SearXNG's JSON: `{results: [{title, url, content}]}`.
pub fn parse_searxng(body: &[u8]) -> Result<Vec<SearchHit>, String> {
    #[derive(Deserialize)]
    struct Payload {
        results: Vec<SearchHit>,
    }
    serde_json::from_slice::<Payload>(body)
        .map(|payload| payload.results)
        .map_err(|_| "search endpoint must return JSON {results:[{title,url,content}]}".into())
}

/// Brave's JSON: `{web: {results: [{title, url, description}]}}`. A hit
/// with no URL is not a source and is dropped here, before the domain
/// policy sees it.
pub fn parse_brave(body: &[u8]) -> Result<Vec<SearchHit>, String> {
    #[derive(Deserialize)]
    struct Payload {
        #[serde(default)]
        web: Web,
    }
    #[derive(Deserialize, Default)]
    struct Web {
        #[serde(default)]
        results: Vec<Hit>,
    }
    #[derive(Deserialize)]
    struct Hit {
        #[serde(default)]
        title: String,
        #[serde(default)]
        url: String,
        #[serde(default)]
        description: String,
    }
    let payload: Payload = serde_json::from_slice(body).map_err(|_| {
        "brave search must return JSON {web:{results:[{title,url,description}]}}".to_string()
    })?;
    Ok(payload
        .web
        .results
        .into_iter()
        .filter(|hit| !hit.url.is_empty())
        .map(|hit| SearchHit {
            title: hit.title,
            url: hit.url,
            snippet: hit.description,
        })
        .collect())
}

/// A snippet cut at a character boundary to [`MAX_SNIPPET_BYTES`].
fn bounded(snippet: &str) -> String {
    if snippet.len() <= MAX_SNIPPET_BYTES {
        return snippet.to_string();
    }
    let mut end = MAX_SNIPPET_BYTES;
    while !snippet.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &snippet[..end])
}

/// The key named by `var`: the process environment first — the path Sterna's
/// own model credential takes — and the gateway's credential file second.
/// `None` when neither has it; the caller refuses by the variable's name.
pub fn resolve_key(var: &str) -> Option<String> {
    resolve_key_from(var, gateway_credentials_path().as_deref())
}

/// [`resolve_key`] with the credential file named, so the order — the
/// environment first, the file second — is testable without the process-wide
/// data directory.
pub fn resolve_key_from(var: &str, credentials_file: Option<&Path>) -> Option<String> {
    if let Ok(value) = std::env::var(var)
        && !value.is_empty()
    {
        return Some(value);
    }
    credentials_file.and_then(|path| credentials_file_value(path, var))
}

/// `<the gateway's data dir>/credentials.toml`, resolved the way the gateway
/// resolves it: `INFERENCE_GATEWAY_DATA_DIR`, else the per-user application
/// data directory for `inference-gateway`. Sterna never links the gateway
/// crate; it reads the one flat file it writes.
pub fn gateway_credentials_path() -> Option<PathBuf> {
    let data_dir = match std::env::var_os("INFERENCE_GATEWAY_DATA_DIR") {
        Some(path) if !path.is_empty() => PathBuf::from(path),
        _ => directories::ProjectDirs::from("", "", "inference-gateway")?
            .data_dir()
            .to_path_buf(),
    };
    Some(data_dir.join("credentials.toml"))
}

/// The value stored under `var` in the gateway's credential file — flat
/// TOML, variable name to value, mode 0600 — or `None` for a file that is
/// absent, unreadable, unparseable or has no such name. Read on every
/// resolution and never cached: the file is a few lines long, and a store
/// answers for now.
pub fn credentials_file_value(path: &Path, var: &str) -> Option<String> {
    let text = std::fs::read_to_string(path).ok()?;
    let table: toml::Table = text.parse().ok()?;
    table
        .get(var)
        .and_then(toml::Value::as_str)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}
