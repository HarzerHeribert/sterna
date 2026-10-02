#!/usr/bin/env python3
"""Fetch Artificial Analysis's current model figures, for the bake.

Usage: scripts/release/fetch-model-index.py > catalogue.json
       (the key is read from ARTIFICIAL_ANALYSIS_API_KEY in the environment:
       never an argument, never printed)

Writes the catalogue `scripts/bake-model-index.py` reads -- {"fetched_at",
"index_version", "models": {slug: facts}} -- from Artificial Analysis's
`GET /api/v2/data/llms/models`. Their terms ask that a key be used
server-side and its answers cached: the bump workflow runs this once per
release and bakes the answer into the gateway, so no install ever needs a
key or asks the network for a model's figures.

A broken answer is refused rather than baked: too few models carrying an
intelligence index means the API changed or failed, and the figures already
shipped are better than a catalogue that sorts nothing.
"""

import json
import os
import sys
import time
import urllib.error
import urllib.request
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent.parent
SHIPPED = ROOT / "crates" / "inference-gateway" / "data" / "model-index.json"
API = "https://artificialanalysis.ai/api/v2/data/llms/models"
TIMEOUT = 60
# The shipped snapshot holds several hundred measured models; an answer with
# fewer than this many is not a catalogue.
FEWEST = 100

# Our field: where Artificial Analysis publishes it. A figure the answer does
# not carry stays absent.
FIELDS = {
    "intelligence": ("evaluations", "artificial_analysis_intelligence_index"),
    "coding": ("evaluations", "artificial_analysis_coding_index"),
    "agentic": ("evaluations", "artificial_analysis_agentic_index"),
    "input_usd_per_million": ("pricing", "price_1m_input_tokens"),
    "output_usd_per_million": ("pricing", "price_1m_output_tokens"),
}


def figure(value):
    """A number, or `None`: a flag or a string is not a measurement."""
    if isinstance(value, bool) or not isinstance(value, (int, float)):
        return None
    return value


def catalogue(body, shipped_version):
    """The bake's input, from the API's answer."""
    models = {}
    for row in body.get("data") or []:
        slug = row.get("slug")
        if not isinstance(slug, str) or not slug:
            continue
        facts = {"name": row.get("name") or slug}
        for field, (group, name) in FIELDS.items():
            value = figure((row.get(group) or {}).get(name))
            if value is not None:
                facts[field] = value
        models[slug] = facts
    measured = sum(1 for facts in models.values() if "intelligence" in facts)
    if measured < FEWEST:
        raise ValueError(
            f"only {measured} models carry an intelligence index (at least {FEWEST} expected); "
            "the figures already shipped are kept"
        )
    # The free API names no index version; the shipped one is carried until
    # it does, so the gateway still says which scale the figures are on.
    version = figure(body.get("index_version")) or shipped_version
    return {"fetched_at": int(time.time()), "index_version": version, "models": models}


def shipped_version():
    try:
        return json.loads(SHIPPED.read_text()).get("index_version")
    except (OSError, ValueError):
        return None


def main():
    key = os.environ.get("ARTIFICIAL_ANALYSIS_API_KEY", "").strip()
    if not key:
        sys.exit("fetch-model-index: set ARTIFICIAL_ANALYSIS_API_KEY in the environment")
    request = urllib.request.Request(
        os.environ.get("ARTIFICIAL_ANALYSIS_API_URL") or API,
        headers={"x-api-key": key, "accept": "application/json", "user-agent": "sterna-release"},
    )
    try:
        with urllib.request.urlopen(request, timeout=TIMEOUT) as answer:
            body = json.load(answer)
    except urllib.error.HTTPError as error:
        sys.exit(f"fetch-model-index: Artificial Analysis answered {error.code} {error.reason}")
    except (urllib.error.URLError, OSError, ValueError) as error:
        sys.exit(f"fetch-model-index: the answer could not be read ({type(error).__name__})")
    try:
        result = catalogue(body, shipped_version())
    except ValueError as error:
        sys.exit(f"fetch-model-index: {error}")
    json.dump(result, sys.stdout, indent=1, sort_keys=True)
    sys.stdout.write("\n")
    measured = sum(1 for facts in result["models"].values() if "intelligence" in facts)
    print(f"fetch-model-index: {len(result['models'])} models, {measured} with an intelligence index", file=sys.stderr)


if __name__ == "__main__":
    main()
