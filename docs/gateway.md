# The inference gateway

`crates/inference-gateway` — a library and the `inference-gateway` binary.
Sterna starts it per session; it can also run on its own for any client that
speaks one of the wire formats below. See [architecture](architecture.md)
for where it sits.

## What it owns

Providers and their built-in templates (43 today, pinned by
`tests/provider_discovery.rs`), API keys, subscription sign-in, several
accounts pooled per model, same-model failover, protocol translation and
usage accounting. It chooses the provider, account and entitlement for a
request. It never changes the requested model or effort unless the
caller's fallback policy allows it, and it keeps no project, session or
harness state.

## Serving

```sh
inference-gateway serve [--listen 127.0.0.1:0] [--config <path>] [--data-dir <path>]
```

- Prints exactly one line to stdout when ready, then nothing more on stdout:
  `{"listening":"http://127.0.0.1:<port>","token":"<bearer>"}`. Every
  diagnostic goes to stderr.
- Only an ephemeral loopback port can be bound; a fixed port is refused by
  name.
- Serves until **stdin reaches EOF** or SIGTERM, SIGINT or SIGHUP arrives.
  stdin is the shutdown channel because a parent that dies takes its child's
  stdin with it, so a gateway cannot outlive the session that started it.
- Routes: `/v1/messages` (Anthropic Messages, the one Sterna uses),
  `/v1/chat/completions` and `/v1/responses` (OpenAI), `/v1/models`, and
  `/v1/systemone` (the typed classifier questions Jev answers, routed to a
  TypeSafe account).

## Configuration

`gateway.toml` in the platform configuration directory
(`~/.config/inference-gateway/` on Linux, `~/Library/Application
Support/inference-gateway/` on macOS), or the file `--config` or
`INFERENCE_GATEWAY_CONFIG` names. A missing file is an empty catalogue, not
an error.

```toml
[accounts.work]                           # one entitlement
kind = "api-key"                          # or claude, chatgpt, gemini, kimi, xai, devin, meta
provider = "my-endpoint"
credential = { env = "MY_ENDPOINT_KEY" }  # a reference: an env var NAME or an OS-credential entry
models = ["m1", "m2"]                     # optional; unset serves whatever model is asked

[providers.my-endpoint]                   # a destination; a built-in template's name overrides it
base_url = "https://llm.example.com/v1"
protocol = "openai-chat"   # anthropic-messages (default), openai-chat, openai-responses
credential_env = ["MY_ENDPOINT_KEY"]   # variable NAMES, never values
```

Neither table may hold a secret. An account's credential is a reference,
resolved at the moment of use: an environment variable name, or an entry in
the OS credential store (Keychain on macOS, Secret Service on Linux). Keys
stored with `credentials set` or `/key` go to the gateway's own
`credentials.toml` (mode `0600`) in its data directory, filed under the
variable name, because a Keychain item written by one ad-hoc-signed build is
refused to the next. A bare string where a reference belongs is refused
without echoing what was written.

## Commands

| command | what it does |
|---|---|
| `entitlements [--json] [--refresh]` | each configured account and what it can serve |
| `models [--json] [--filter TEXT] [--import PATH]` | published measurements for the models this gateway serves |
| `subscriptions connect <provider> [--entitlement NAME] [--device-code] [--no-browser]` | sign in with the provider's own flow |
| `subscriptions logout <provider> --entitlement NAME` | forget one account's login |
| `subscriptions usage [--entitlement NAME] [--json]` | each subscription's plan, limit windows and reset times |
| `subscriptions pool --entitlement NAME --include\|--exclude` | take an account into or out of its model's pool |
| `subscriptions verify --entitlement NAME` | start the broker, read the catalogue, send one small completion |
| `subscriptions adopt-binary <path>` | adopt a CLIProxyAPI executable, pinned by its digest |
| `providers add <name> --base-url URL [--protocol P]` | declare a custom endpoint and an account that uses it |
| `credentials list [--json]` | where each provider's key comes from — names only, never a value |
| `credentials set <provider> [--variable VAR]` | store a key read from **stdin**, never from argv |
| `credentials remove <provider>` | remove a stored key |
| `routing-cost [--hours N] [--json --since UNIX --session ID]` | what routing has consumed, as JSON Lines |

Inside Sterna the same things are `/login`, `/key`, `/models` and `/status`.

## Subscriptions

Subscription sign-in runs through CLIProxyAPI, which the gateway manages as
a child process. Each release pins one CLIProxyAPI version and a SHA-256
per platform in `release/cliproxyapi.toml`; the installer and `sterna
update` refuse an asset whose digest differs. A daily workflow
(`.github/workflows/broker-bump.yml`) pins new upstream releases.
Which subscriptions exist, and the terms risk of each: [subscriptions](subscriptions.md).
