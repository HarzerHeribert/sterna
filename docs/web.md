# The web tools

`web.fetch` and `web.search` are host functions: the socket is opened by
Sterna's own process, on the host side of the V8 boundary, never by a shell
or a tool. **They grant no network to commands** — shells stay without a
network on every platform ([sandbox](sandbox.md)). Code:
`crates/sterna/src/web.rs`, `src/web/search.rs`.

## Configuration

On by default. A fetch reaches the allowed hosts at once and asks before
any other:

- **The allowed hosts** are one list for commands and the web: the package
  registries and source hosts of the ecosystems switched on, your own
  `sandbox.hosts`, and each `--allow-host`
  ([sandbox](sandbox.md#allowed-hosts)).
- **Any other host asks**, the way leaving the sandbox does: *Reach a new
  host*, with *Allow host for this session*, *Always allow host* (saved to
  the global `sandbox.hosts`), *Allow once* for this page only, or a
  refusal. At Full access nothing asks. With nobody at the terminal
  (`sterna -p`, a subagent) the fetch is refused and says so.
- A redirect to a host that is not allowed is not followed: the refusal
  names the URL, and fetching it asks.

```toml
[web]
enabled = true                                 # the default; false turns both tools off
deny_domains = ["private.docs.example.org"]    # never reached, whatever is allowed
search_provider = "searxng"                    # or "brave"
search_endpoint = "https://search.example.org/search"
# search_key_var = "BRAVE_API_KEY"             # a variable NAME, for a keyed provider
max_response_bytes = 1048576
timeout_seconds = 20
```

- `*.example.org` matches subdomains only; a bare name matches exactly.
- HTTPS only, unless `allow_http = true`.
- Search needs a provider you configure: a SearXNG-compatible JSON
  endpoint, or Brave with its key. The key is read by name from the
  environment, then from the gateway's credential file — never written in a
  project file. With no provider, `web.search` refuses and says so.
- A destination you configured yourself (the search endpoint, a remote MCP
  server) is reached because you configured it; the deny list still wins.
- An older `web.allow_domains` in your global settings moves into
  `sandbox.hosts` once, with a notice; a project's is removed, because a
  project cannot allow hosts.

`web` is declared to the model while it is on. The declaration says how a
fetch reaches a host, not which hosts: the list grows as you allow them,
and a list in the prompt would throw its cache away each time.

## In a cell

```typescript
const found = web.search("ratatui scroll offset");
const page  = web.fetch(found.results[0].url);
return {source: page.citation, head: page.content.slice(0, 1000)};
```

`fetch` returns bounded UTF-8 text, HTML, JSON or XML with its URL, a
citation and `untrusted_content: true`. HTML comes back as source: no
browser, no script execution. Web content is data and never becomes an
instruction.

## What the broker checks

Every request and every redirect (at most five) against the deny list and
the allowed hosts; the resolved address against private, loopback and
link-local ranges, at connect time; the content type and size. Ambient HTTP proxies and
credentials are not used. Cancelling stops waiting and stops further
redirects; a request already sent may finish within its timeout.
