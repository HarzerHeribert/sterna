# The web tools

`web.fetch` and `web.search` are host functions: the socket is opened by
Sterna's own process, on the host side of the V8 boundary, never by a shell
or a tool. **They grant no network to commands** — shells stay without a
network on every platform ([sandbox](sandbox.md)). Code:
`crates/sterna/src/web.rs`, `src/web/search.rs`.

## Configuration

Off until you turn it on, and a fetch reaches only domains you allowed:

```toml
[web]
enabled = true
allow_domains = ["docs.rs", "*.python.org"]   # empty refuses every fetch
deny_domains = ["private.docs.example.org"]    # deny wins
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
  project file. With no provider, `web.search` is not offered.
- A destination you configured yourself (the search endpoint, a remote MCP
  server) is reached because you configured it; the deny list still wins.

`web` is declared to the model only when `[web]` is configured, together
with the domains it may name.

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

Every request and every redirect (at most five) against the domain policy;
the resolved address against private, loopback and link-local ranges, at
connect time; the content type and size. Ambient HTTP proxies and
credentials are not used. Cancelling stops waiting and stops further
redirects; a request already sent may finish within its timeout.
