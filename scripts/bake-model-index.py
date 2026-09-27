#!/usr/bin/env python3
"""Bake the model-measurement snapshot the gateway ships.

Usage: scripts/bake-model-index.py CATALOGUE.json|- [--limits PATH|URL] [--no-limits]
       (the limits catalogue is fetched unless --limits or --no-limits says otherwise)

Reads an Artificial Analysis catalogue -- {"fetched_at", "index_version",
"models": {slug: facts}}, the shape `inference-gateway models --import`
takes -- from a path or standard input, and
rewrites crates/inference-gateway/data/model-index.json with the fields the
subagent roster and future cost routing need. Run it when a release is cut so
the published figures ship with the binary; a user's own key overlays this
copy at run time (`inference-gateway models --import`).

**Two sources, and the primary always wins.** Artificial Analysis publishes
what a model is worth -- intelligence, coding, agentic, cost -- and publishes
neither of the two *real* limits a harness must never guess: the context
window and the model's own output maximum. Those come from a second catalogue
([`LIMITS`]), and only ever fill a field the primary did not carry. Both are
read here, at bake time, so that an install or an update carries current
figures and nothing in the gateway or in Sterna ever makes a network call to
learn them.
"""

import json
import sys
import urllib.error
import urllib.request
from datetime import date, timezone, datetime
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
OUT = ROOT / "crates" / "inference-gateway" / "data" / "model-index.json"

# LiteLLM's published catalogue: MIT, and this file sits outside the one
# directory of that repository which is licensed otherwise. It carries
# `max_input_tokens` and `max_output_tokens` for several thousand models,
# keyed by the name each provider serves them under.
LIMITS = "https://raw.githubusercontent.com/BerriAI/litellm/main/model_prices_and_context_window.json"
LIMITS_NAME = "LiteLLM model_prices_and_context_window.json (MIT)"
LIMITS_TIMEOUT = 30

# What the roster reads, plus the cost figures routing will read next. An
# absent figure stays absent: the published set is genuinely partial.
KEEP = (
    "name",
    "intelligence",
    "coding",
    "agentic",
    "cost_per_task_usd",
    "input_usd_per_million",
    "output_usd_per_million",
    # The two real limits: a harness cannot choose either and must not guess
    # them. The primary catalogue publishes neither, so they arrive from
    # [`LIMITS`] -- but if the primary ever starts carrying them, it wins.
    "context_window_tokens",
    "max_output_tokens",
)

# A model's own vendor is the only authority for its limits. Every other row
# for the same name is a re-host, and re-hosts cap: measured 2026-09-17,
# `claude-opus-4-5` reads 200000 from Anthropic and 128000 through GitHub
# Copilot, `claude-sonnet-4-6` reads 1000000 from Anthropic and 200000 through
# Snowflake. So candidates are consulted in this order and the first tier that
# holds a figure decides; `""` is a key with no provider prefix at all, which
# is that catalogue's own spelling for the first-party model.
VENDORS = (
    "",
    "openai/",
    "anthropic/",
    "gemini/",
    "azure/",
    "azure_ai/",
    "vertex_ai/",
    "xai/",
    "mistral/",
    "deepseek/",
    "cohere/",
    "groq/",
)

# The primary catalogue measures a model once per reasoning effort and names
# each variant separately; a context window belongs to the model, not to the
# effort it was asked for. These suffixes are therefore stripped -- and only
# these, spelled out rather than pattern-matched, so that `-mini`, `-codex`
# and `-flash` are never mistaken for one.
EFFORTS = (
    "-non-reasoning",
    "-xhigh",
    "-high",
    "-medium",
    "-low",
    "-minimal",
)

# The modes whose rows describe a conversational model. An image or embedding
# row may share a name with one and means something else by "tokens".
CHAT_MODES = (None, "chat", "responses", "completion")


def normalise(name):
    """`routing::analysis::normalise`, which is what our own keys are."""
    return name.strip().lower().replace(".", "-").replace("_", "-")


def source_bytes(argv):
    if not argv:
        sys.exit(
            "bake-model-index: give the Artificial Analysis catalogue as a path, "
            "or - to read it from standard input"
        )
    if argv[0] == "-":
        return sys.stdin.buffer.read()
    return Path(argv[0]).read_bytes()


def limits_bytes(where):
    """The limits catalogue, from a path or a URL.

    A failure here is not a failure of the bake: the primary figures are the
    point of this file and they are already in hand. It says so on stderr and
    returns `None`, and every row then keeps both limits absent -- which is
    the honest answer, and the one `window ?` already renders.
    """
    if where is None:
        return None
    try:
        if str(where).startswith(("http://", "https://")):
            with urllib.request.urlopen(str(where), timeout=LIMITS_TIMEOUT) as answer:
                return answer.read()
        return Path(where).read_bytes()
    except (urllib.error.URLError, OSError, ValueError) as error:
        print(f"limits: unavailable ({error}); baking without them", file=sys.stderr)
        return None


def limits_index(raw):
    """Normalised name -> the rows any provider publishes under it."""
    index = {}
    if raw is None:
        return index
    try:
        catalogue = json.loads(raw)
    except json.JSONDecodeError as error:
        print(f"limits: unreadable ({error}); baking without them", file=sys.stderr)
        return index
    for key, facts in catalogue.items():
        if key == "sample_spec" or not isinstance(facts, dict):
            continue
        if facts.get("mode") not in CHAT_MODES:
            continue
        for candidate in {normalise(key), normalise(key.split("/")[-1])}:
            index.setdefault(candidate, []).append((key, facts))
    return index


def whole(value):
    """A token count as an integer, or `None`.

    The source publishes a few of these as floats (`grok-4-1-fast` carries
    `2000000.0`), and the gateway deserialises them into an integer field:
    one float in the whole file makes serde reject the *catalogue*, not the
    row, so every model silently loses every figure. Measured 2026-09-17.
    """
    if isinstance(value, bool) or not isinstance(value, (int, float)):
        return None
    number = int(value)
    return number if number == value and number > 0 else None


def agreed(rows):
    """One window and one maximum, or `None` where the rows disagree."""
    windows = {w for w in (whole(row.get("max_input_tokens")) for row in rows) if w}
    outputs = {o for o in (whole(row.get("max_output_tokens")) for row in rows) if o}
    if len(windows) > 1 or len(outputs) > 1:
        return None
    if not windows and not outputs:
        return ()
    return (
        windows.pop() if windows else None,
        outputs.pop() if outputs else None,
    )


def limits_for(hits):
    """The vendor's figures, or nothing.

    Returns `(window, maximum, why)`. `why` is the tier that decided, or
    `"conflict"` -- and a conflict is *skipped*, never resolved by preference,
    because attaching one provider's cap to another's model is worse than
    leaving the limit unknown.
    """
    for vendor in VENDORS:
        tier = [
            facts
            for key, facts in hits
            if ("/" not in key if vendor == "" else key.lower().startswith(vendor))
        ]
        if not tier:
            continue
        figures = agreed(tier)
        if figures is None:
            return None, None, "conflict"
        if figures:
            return figures[0], figures[1], f"vendor {vendor or 'unprefixed'}"
    figures = agreed([facts for _, facts in hits])
    if figures:
        return figures[0], figures[1], "unanimous"
    return None, None, "conflict"


def limits_of(slug, index):
    """This model's limits, by exact name and then without its effort word."""
    key = normalise(slug)
    hits = index.get(key)
    how = "exact"
    if not hits:
        base = key
        for effort in EFFORTS:
            if base.endswith(effort):
                base = base[: -len(effort)]
                break
        if base != key:
            hits = index.get(base)
            how = "effort"
    if not hits:
        return None, None, "unmatched"
    window, maximum, why = limits_for(hits)
    if window is None and maximum is None:
        return None, None, why
    return window, maximum, f"{how}, {why}"


def main(argv):
    limits_where = LIMITS
    rest = []
    argv = list(argv)
    while argv:
        word = argv.pop(0)
        if word == "--no-limits":
            limits_where = None
        elif word == "--limits":
            limits_where = argv.pop(0) if argv else None
        else:
            rest.append(word)

    catalogue = json.loads(source_bytes(rest))
    raw = limits_bytes(limits_where)
    index = limits_index(raw)

    models = {}
    filled = {"context_window_tokens": 0, "max_output_tokens": 0}
    reasons = {}
    for slug, facts in sorted(catalogue.get("models", {}).items()):
        kept = {key: facts[key] for key in KEEP if facts.get(key) is not None}
        if index:
            window, maximum, why = limits_of(slug, index)
            reasons[why] = reasons.get(why, 0) + 1
            # The primary wins wherever it carries a figure of its own.
            if window is not None and "context_window_tokens" not in kept:
                kept["context_window_tokens"] = window
                filled["context_window_tokens"] += 1
            if maximum is not None and "max_output_tokens" not in kept:
                kept["max_output_tokens"] = maximum
                filled["max_output_tokens"] += 1
        # A row with no figure at all measures nothing and only costs bytes.
        if len(kept) > 1 or (kept and "name" not in kept):
            models[slug] = kept

    captured = catalogue.get("fetched_at")
    head = {
        "source": "Artificial Analysis (artificialanalysis.ai)",
        "index_version": catalogue.get("index_version"),
        "captured": (
            datetime.fromtimestamp(captured, timezone.utc).date().isoformat()
            if isinstance(captured, (int, float))
            else date.today().isoformat()
        ),
    }
    if index:
        # Provenance, so a reader can tell which source a number came from:
        # everything but the two limits is the primary's.
        head["limits_source"] = LIMITS_NAME
        head["limits_captured"] = date.today().isoformat()
        head["limits_fields"] = ["context_window_tokens", "max_output_tokens"]

    # One line per model: compact enough to ship, granular enough to diff.
    rows = ",\n".join(
        f"  {json.dumps(slug)}: {json.dumps(facts, sort_keys=True, separators=(',', ':'))}"
        for slug, facts in models.items()
    )
    body = ",\n".join(f" {json.dumps(k)}: {json.dumps(v)}" for k, v in head.items())
    OUT.write_text("{\n" + body + ',\n "models": {\n' + rows + "\n }\n}\n")
    # `relative_to` raises for an OUT outside the checkout, which is exactly
    # what a test does when it bakes into a temporary directory.
    shown = OUT.relative_to(ROOT) if OUT.is_relative_to(ROOT) else OUT
    print(f"{shown}: {len(models)} models")
    if index:
        print(
            f"  limits: {filled['context_window_tokens']} window(s), "
            f"{filled['max_output_tokens']} maximum(s) from {LIMITS_NAME}"
        )
        for why, count in sorted(reasons.items(), key=lambda row: -row[1]):
            print(f"    {why}: {count}")


if __name__ == "__main__":
    main(sys.argv[1:])
