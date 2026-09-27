# Contributing

Thanks for stopping by. Bug reports, ideas and pull requests are all welcome,
and a clear bug report is often worth more than a patch.

## Reporting a bug

Open an issue with the **Bug report** form. The most useful reports have:

- what you ran and what you expected,
- what happened instead (the exact message, or a screenshot of the screen),
- `sterna --version`, your OS, and how you installed (install script, `sterna update`, or from source).

**Never paste an API key, a token or a login file** into an issue, even a
revoked one. If a log contains one, cut it out first.

Security problems do not go in issues: see [SECURITY.md](SECURITY.md).

## Asking a question or suggesting an idea

Questions and open-ended ideas go to
[Discussions](https://github.com/HarzerHeribert/sterna/discussions). A
concrete feature request, one you can say in a sentence, can be an issue with
the **Feature request** form.

## Pull requests

1. For anything bigger than a small fix, open an issue or discussion first so
   we can agree on the shape before you spend the time.
2. Keep one change per pull request, and describe the behaviour it changes.
3. Add or update a test that fails without your change.
4. Before you push, run:

   ```sh
   cargo fmt --all
   cargo clippy --workspace --all-targets
   cargo test -p sterna          # or the crate you changed
   ```

   CI runs the full matrix (macOS, Linux, Windows) on your pull request once a
   maintainer approves the run.

The workspace has two parts: `crates/sterna` (the terminal app) and
`crates/inference-gateway` (the model gateway).
[`docs/architecture.md`](docs/architecture.md) is a one-page map of how they
fit together.

## The terms of a contribution

This repository is **not open source**: its [LICENSE](LICENSE) reserves all
rights to the copyright holder. So that a contribution can be merged at all,
by opening a pull request you confirm that you wrote it (or have the right to
submit it) and you grant HarzerHeribert a perpetual, worldwide, royalty-free,
irrevocable licence to use, modify, distribute and relicense it as part of this
project. You keep the copyright in your own work. The pull request template
has a checkbox for this.

## Conduct

Be kind and assume good faith. The [Code of Conduct](CODE_OF_CONDUCT.md)
applies to issues, discussions and pull requests.
