# The ruler

`sterna ruler run` measures harnesses on the same tasks, so a change to
Sterna can be judged by outcome and cost rather than by preference. Code:
`crates/sterna/src/ruler/`.

## Tasks

A task is real work from this repository's history (`ruler/tasks.rs`): the
parent commit the harness starts from, a statement written from the commit's
subject — never from its diff — and the commit's own test command, which
the ruler runs. Tasks come in three tiers (Leaf `L1`–`L4`, Standard
`S1`–`S4`, Heavy `H1`–`H4`) plus task families added later: explore
(`X1`, `X2`, scored by facts found), fix (`F1`), implement (`I1`, `I2`),
write (`W1`) and edit across files (`E1`).

Each attempt gets a fresh `git worktree` at the task's parent commit,
removed afterwards, so no attempt sees another's tree.

## The command

    sterna ruler run \
      --task X1 --task F1 \
      --harness sterna --harness codex \
      --parent-model <model> \
      --repeat 3 \
      --out ruler/<date>/

- `--task all` runs every task; `--tier leaf|standard|heavy` runs one tier.
- `--harness` rows: `sterna`, `claude-code`, `codex`.
- **`--repeat` is 3 by default and at least 3.** One attempt of an agent
  task measures the sample, not the harness.
- `--parent-model` is required for a Sterna row. It is written into the
  attempt's own `.sterna/config.toml`, the file a person's session reads,
  not passed as a flag.
- Two expansions split the Sterna row into arms: one per interface
  (`cells`, `hybrid`, `tools`), or one per decision mode (`off`, `shadow`,
  `on`, with `--decisions-model`). One expansion at a time; `sterna ruler
  run --help` names the flags.
- `--meter <path to inference-gateway>` is where token figures and turns
  come from: after each attempt the ruler reads `inference-gateway
  routing-cost --json` for that attempt's time window, one attempt at a
  time. Without it, tokens and turns are absent. `--gateway <url>` points
  every row at the same running gateway.
- `--out` writes one JSON line per attempt: task, harness, commit, attempt,
  outcome, token figures (with `--meter`), wall clock, turns and the test
  command's exit status, so runs can be diffed later.

It prints one row per task and harness and one block per tier. The ruler
never prints tokens per turn, and a missing measurement is shown as absent,
never as zero.

## Three ways a measurement can lie

- **Different prompts.** The request body is byte-identical with and
  without the gateway ([model contract](model-contract.md#7-the-gateway-hop-changes-nothing-in-the-prompt)),
  so a comparison through the gateway compares harnesses, not prompts.
- **Too few attempts.** Hence the minimum of three, and results reported as
  direction, not proof.
- **A task the harness has seen.** Statements come from commit subjects,
  and each attempt starts from the parent tree.

Results so far: [measurements](measurements.md).
