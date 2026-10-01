# Measurements

What has been measured, when, and on what. Runs from before the rename used
the program under its former name, Pane; the commits named are in this
repository's history. Tasks are the [ruler](ruler.md)'s unless a section
names SWE-bench. **Read the limits at the end before acting on a number.**

## Against the Codex CLI (2026-09-30)

Sterna v0.1.0-pre.24 as released against Codex CLI 0.155.1, both on GPT-6 Sol
through the ChatGPT subscription. What the run could print was written down
before it ran.

**Tasks.** The first 30 tasks that grade correctly on macOS from a seeded
random order (seed 20260930) of the 306 django and sympy tasks in SWE-bench
Verified: 16 of the first 46 drawn were dropped because the untouched or the
gold-patched tree did not grade as expected here, before any agent ran. The
draw is 11 tasks of under 15 minutes, 15 of up to an hour and 4 of up to four
hours, close to the benchmark's own mix.

**Arms**, run side by side on each task so both meet the same provider:
Sterna at its default effort (GPT is asked for `low`), Sterna at `medium`,
and Codex at its default `medium`. One attempt each; the five tasks where the
arms disagreed got two more attempts in every arm, and a task's result is
its share of resolved attempts. Every attempt ran fenced from the rest of the
disk; neither agent searched the web. Ratios are Sterna's total over Codex's
across the 30 tasks, with a 90 % interval from resampling tasks.

| | Sterna, default (`low`) | Sterna, `medium` | Codex (`medium`) |
|---|---|---|---|
| resolved, of 30 | 24.7 | 24.0 | 25.0 |
| model requests | 0.79× (0.71–0.89) | 0.78× (0.73–0.85) | 15.5 per task |
| fresh input tokens | 1.29× (1.15–1.43) | 1.45× (1.29–1.62) | |
| output tokens | 1.02× (0.93–1.13) | 1.31× (1.21–1.40) | |
| cost | 1.00× (0.90–1.11) | 1.14× (1.04–1.24) | |
| cost per resolved task | $0.21 | $0.25 | $0.21 |
| time | 1.23× (0.92–1.67) | 1.48× (1.21–1.88) | |

What that says:

- **The same results.** No difference in what was resolved (exact McNemar
  p = 1.0 for either Sterna arm against Codex).
- **About a fifth fewer model requests**, at both efforts: the one clear
  difference. Codex 0.155.1 writes programs too: its only tool, `exec`, runs a
  JavaScript program that calls the shell, and it averaged 1.7 calls per
  program, 30 % of programs with more than one. Sterna's cells averaged 2.2 to
  2.4 calls, two-thirds with more than one.
- **Each request carries more**: more new input (mostly whole definitions
  read with `context`), and at `medium` more output. At the default effort
  the two even out, and the cost is the same.
- **Not faster.** At the default effort the time is within noise, and 1.02×
  with the two attempts described next left out; at `medium` Sterna was
  slower.
- **`medium` bought nothing** for Sterna: the same results at 13 % more cost
  and 20 % more time, so GPT keeps `low` as its default.
- Two Sterna attempts on sympy 19040 hung on a command that never ended and
  were stopped at the 40-minute cap: a foreground command had no bound in
  pre.24, and the cell's clock does not run while it waits. Both count as
  unresolved above.

Five tasks from the earlier hand-picked set also ran once in every arm
outside the sample: Sterna at `medium` resolved 3, the other two arms 2 each.

**Fresh input, re-run on 2026-10-01** (build `5897083d`, the default arm
again on the same 30 tasks, attempts as before). Sterna had sent more
uncached input than Codex, and three changes went at it: sessions share four
prompt-cache keys, so a new session's first request lands where the system
prompt is cached; an excerpt the conversation already carries line for line
is a pointer; and a result no longer repeats the cell's description or the
declared keys of a typed handle.

| against | resolved | fresh input | cost | requests |
|---|---|---|---|---|
| the same arm on 2026-09-30 | 25.0 to 24.7 | 0.92× (0.83–1.03) | 0.97× (0.87–1.07) | 1.00× |
| Codex, 2026-09-30 | 25.0 to 25.0 | 1.19× (1.06–1.32) | 0.97× (0.86–1.07) | 0.79× (0.72–0.87) |

The first request of a session went from wholly uncached (8.2–8.6K fresh
tokens every time) to a median of 94 % cached, in all 42 first requests the
gateway's ledger recorded during the run. The
pointers replaced 217 excerpts. The total fell less than that promised,
because this run's model asked for a tenth more contexts: what a run chooses
to read moves fresh input by as much as these changes do. What is left of
the gap is by design -- Sterna shows whole definitions and their
neighbours, about 1.8 times the source Codex reads, and the neighbours are
how it found the sibling code path on django 15957.

**Reading by condition, 2026-10-01** (build `a703587a`, the same 30 tasks
and attempts). The model was given two ways to read less: a search option
that printed each match with N lines either side, numbered so an `edit`
could bind to them, and `context` modes (`precise`: the definition alone,
neighbours as their first line; `generous`: a file whole up to 24 KB). It
took both -- the window option on 234 of 267 searches, `precise` on 226 of
318 contexts -- and `context`'s share of result text fell from 69 % to 56 %.
Against the previous build it resolved 24.0 to 25.0, with 1.15× the fresh
input (1.04–1.27), 1.08× the requests and 1.10× the cost (1.01–1.21); the
gateway's ledger, counted over the run's own window, agrees (1.11×). The
windows served as a preview: half were followed within two cells by a
`context` of the same file, so they added to the read instead of replacing
it. The window option was removed; the modes were kept for a run of their
own.

**`context` modes alone, 2026-10-01** (build `1d36e714`). The model chose
`precise` on 146 of 347 contexts with no nudge to, and made more context
calls instead (347 against 325): the text sent back was unchanged, 3.46M
characters against 3.44M, and it resolved 24.3 to 25.0. The modes were
removed; `context` decides what it returns, the same way every time.

**What these comparisons can and cannot say.** The last three each ran one
arm and set it against an earlier run, and the provider's cache served a
different share of the input at different times of day: across the four
runs fresh input moved −8 %, +11 % and +25 % while the result text the
model read moved −3 %, −6 % and 0 %. Their fresh-input and cost figures
mix the change with the day's cache; the result text and the behaviour
counts are what they show. A comparison runs its control arm side by
side in the same run, as the 2026-09-30 one did.

## Against the Codex CLI (2026-09-29, superseded by the section above)

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

## Against the Codex CLI (2026-09-24, superseded)

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
  runs. A difference under ~40 % at n = 3 is direction, not a result. The
  2026-09-30 run compares arms task by task over 30 tasks instead, and
  says only what its intervals carry.
- **Two repositories, one model** in the 2026-09-30 run: django and sympy,
  the tasks among them that grade on macOS, and GPT-6 Sol.
- **One repository, few models** in the earlier runs. A weaker model, an unfamiliar repository
  or tasks the model fails can change every row.
- Attempts that hit a subscription's weekly limit are recorded as errored,
  never as failures.
