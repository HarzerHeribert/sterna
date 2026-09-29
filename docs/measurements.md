# Measurements

What has been measured, when, and on what. Every run below used the
program under its former name, Pane; the commits named are in this
repository's history. Tasks are the [ruler](ruler.md)'s. **Read the limits
at the end before acting on a number.**

## Against the Codex CLI (2026-09-29)

Same model (GPT-6 Sol) for both, three attempts per task. Sterna ran with its
helper models and Jev off (`[helpers] enabled = false`, `[decisions] mode =
"off"`) at its default effort, which sends `low` for an OpenAI model; Codex
0.155.1 ran at `medium`. Cost is API-equivalent at $2 fresh input, $0.20
cached input and $10 output per million tokens; both actually ran on the
ChatGPT subscription.

**Four small tasks** (the [ruler](ruler.md)'s F1, X1, I1, E1; commit
`26130fb3`): every attempt of both passed.

| | Sterna, helpers off | Codex | Sterna, shipped defaults |
|---|---|---|---|
| passed | 12/12 | 12/12 | 12/12 |
| time, sum of per-task means | 452 s | 489 s | 521 s |
| tokens (uncached) | 6.44M (780k) | 9.31M (699k) | — |
| requests | 122 | 188 | — |
| cost | $2.97 | $3.45 | $5.85 |

The shipped defaults cost most because of the helper models; one of those
attempts also hit a paging bug fixed in `a542867e`.

**Six hard SWE-bench Verified tasks** (django 14631, 15629, 15957, 16263;
sympy 14248, 16597; build `16fbebee`), graded on the tasks' own
FAIL_TO_PASS and PASS_TO_PASS tests:

| | Sterna, helpers off | Codex |
|---|---|---|
| resolved | 7/18 | 9/18 |
| time per attempt | 187 s | 230 s |
| requests per attempt | 18.2 | 27.5 |
| cost per attempt | $0.32 | $0.38 |

Both failed every attempt of 16263, 14248 and 16597; Sterna lost one attempt
each of 15629 and 15957, one to a test it skipped after watching it fail and
one to a query that duplicated rows on many-to-many relations. At `medium`
effort Sterna resolved 5 of 9 on three of these tasks, at 304 s and $0.57 an
attempt. Neither agent looked up the upstream fix in any run.

**What a model can find on this machine.** DeepSeek V4.1 Flash under Sterna
resolved 17 and 18 of 18 until the fix was out of its reach: it read later
commits from the repository's history, other copies of the project on disk,
the pull request on GitHub, and notes in temporary folders. Each attempt now
gets a clone ending at the task's base commit and runs under a seatbelt
profile that refuses the home folder, temporary folders and the network
outside its own tree. Sealed that way it resolved 7 of 18, at 770 s and
$0.13 (DeepSeek's peak rates) an attempt.

## Against the Codex CLI (2026-09-24, superseded by the section above)

Four tasks, same model (GPT-6 Sol), three attempts each, Sterna at its
shipped defaults of commit `6fc97dc7`. Time is launch to exit including the
task's own test run; tokens are everything the gateway served for the
attempt (main model, helpers, Jev), cached included, uncached in brackets;
Codex tokens come from its own session logs. Codex ran in its
workspace-write sandbox, Sterna with full access.

| task | Sterna | Codex |
|---|---|---|
| F1 — fix a failing test | 3/3 · 134 s · 452k (49k) | 3/3 · 77 s · 563k (43k) |
| X1 — explore and explain | 3/3 · 71 s · 312k (45k) · 8.0 of 8 facts | 3/3 · 73 s · 398k (68k) · 7.3 of 8 |
| I1 — implement a small feature | 3/3 · 53 s · 124k (23k) | 3/3 · 55 s · 239k (25k) |
| E1 — rename across files | 3/3 · 215 s · 1.09M (97k) | 3/3 · 164 s · 1.21M (76k) |
| **all twelve attempts** | **12/12 · 1.98M tokens (214k uncached)** | **12/12 · 2.41M (212k)** |

Same results, 18 % fewer tokens overall and as many uncached ones. Codex
finished 1.28× faster (473 s against 369 s for the four tasks). Split from
the attempts' events: model time was 975 s for Sterna and 976 s for Codex,
in 128 requests against 161; the difference is commands, 422 s against
118 s, and 200 s of that is the repository's own gate script, which its
`CLAUDE.md` asks for before an edit is reported — Sterna ran it in 4 of 6
edit attempts, Codex in 1. Without it Codex is 1.10× faster (406 s against
369 s); the rest is Sterna's extra formatting and test runs on F1 and E1.
On X1 and I1 Sterna was as fast or faster.

81–91 % of each task's input came from the provider's prompt cache.

Handing any command still running at 10 s to a background job was built and
measured: E1's command wait fell from 134 to 65 s but model time rose from
133 to 179 s and cells from 53 to 72, because the model spent turns
collecting jobs. It was reverted (`a8a29c96`); a cell can still start a
background job itself with `bg.run`.

Earlier, for the record: Claude Code beat Pane in both comparisons of
2026-09-06 and 2026-09-07. Before 2026-09-24 the GPT prompt cache was broken
in Pane, so every earlier GPT token figure overstates its cost.

## Four request-savers, A/B (2026-09-25, GPT-6 Sol)

A = `776dd89e`; B = `d8e40360`, which adds a memo for an identical check,
edits bound to lines already seen, failure locations attached, and an
edit-rhythm paragraph asking for every file of a change in one cell.
Median time to answer and requests per attempt; every attempt passed.

| task | n per arm | A | B |
|---|---|---|---|
| F1 | 3 | 109 s · 7.0 | 110 s · 7.3 |
| X1 | 8 | 86 s · 8.5 | 71 s · 6.9 |
| I1 | 8 | 67 s · 6.2 | 46 s · 5.0 |
| E1 | 3 | 272 s · 20.0 | 259 s · 20.0 |

On X1 and I1 none of the three mechanisms fired, so their fewer requests
are the reworded paragraph. I1's time is the one clear gain.

## Jev, offline (2026-09-23)

Sterna's own questions, two runs each, labelled cases in
`crates/sterna/examples/decision_eval_cases/`. The latencies are the
classifier's own, measured offline; in a live session, through the gateway,
an answer takes about two seconds.

| question | cases | result | latency |
|---|---|---|---|
| kind (explore, fix, implement, question, run) | 48 requests | 88 % right, 100 % stable across runs; at confidence ≥ 0.7, 97 % right on 79 % of cases | ~300 ms median |
| intent (read-only, modify, run, other) | the same 48 | 96 % | same request |
| field shape (log, listing, source, prose, data) | 29 real texts | 97 %; `log` never wrongly claimed | ~310 ms |
| enough (is a return enough to act on?) | 24 returns | median confidence 0.72 when enough, 0.11 when not | ~330 ms |
| complexity (needs exploration?) | 35 cases | 23/35 with or without the session's context; leans to "needs exploration" | same request |

Kind, intent and field shape are what Jev is used for; whether a request
needs exploration is decided from facts the session already has.

## The scout, offline (2026-09-23)

Does it name the files the acting model went on to read (read in at least 3
of 6 baseline attempts)?

| variant | X1 recall | X2 recall | time |
|---|---|---|---|
| tool-loop scout | 0.62 | 0.17–0.33 | ~60 s |
| one answer over the file listing (Opus 5.5) | 0.88 | 0.17 | ~8 s |
| one answer over the file listing (GPT-6 Luna) | 0.75–0.88 | 0.00 | 10–16 s |

Where file names reveal the files (X1) the one-shot dissection matches or
beats the loop at about a fifteenth of the tokens; where they do not (X2),
no variant finds them.

## Where the main model's context goes

Weighted by how often each part is resent, on this repository: the system
prompt and project instructions about 60 %, tool results 25–30 % (search
hits and listings first, then file text, then command output), the cells'
own source the rest. A helper can save at most the tool-result share.

## Limits

- **n is small** (3–8 per cell) and the spread is wide: main-model tokens
  on X1 had a standard deviation of about 35 % of the mean across nine
  runs. A difference under ~40 % at n = 3 is direction, not a result.
- **One repository, few models.** A weaker model, an unfamiliar repository
  or tasks the model fails can change every row.
- Attempts that hit a subscription's weekly limit are recorded as errored,
  never as failures.
