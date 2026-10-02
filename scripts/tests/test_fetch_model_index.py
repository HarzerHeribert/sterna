#!/usr/bin/env python3
"""`scripts/release/fetch-model-index.py` against a stand-in for Artificial
Analysis's API on 127.0.0.1.

The key travels in the `x-api-key` header and nowhere else -- not the
output, not an error -- each figure lands in the field the bake reads, and an
answer too small to be a catalogue is refused, so a broken API never replaces
the figures already shipped.
"""
from __future__ import annotations

import http.server
import json
import os
import pathlib
import subprocess
import sys
import threading
import unittest

ROOT = pathlib.Path(__file__).resolve().parent.parent.parent
SCRIPT = ROOT / "scripts" / "release" / "fetch-model-index.py"
KEY = "aa-fixture-key-0123456789abcdef"  # glasshouse:not-a-secret


def rows(count):
    return [
        {
            "id": f"id-{n}",
            "slug": f"model-{n}",
            "name": f"Model {n}",
            "model_creator": {"id": "c", "name": "Maker", "slug": "maker"},
            "evaluations": {
                "artificial_analysis_intelligence_index": 40.0 + n / 10,
                "artificial_analysis_coding_index": 30.5,
            },
            "pricing": {"price_1m_input_tokens": 1.25, "price_1m_output_tokens": 10, "price_1m_blended_3_to_1": 3.4},
        }
        for n in range(count)
    ]


def serve(body, status=200):
    seen = []

    class Answer(http.server.BaseHTTPRequestHandler):
        def do_GET(self):
            seen.append({"path": self.path, "key": self.headers.get("x-api-key")})
            payload = json.dumps(body).encode()
            self.send_response(status)
            self.send_header("content-type", "application/json")
            self.send_header("content-length", str(len(payload)))
            self.end_headers()
            self.wfile.write(payload)

        def log_message(self, *args):
            pass

    server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Answer)
    threading.Thread(target=server.serve_forever, daemon=True).start()
    return f"http://127.0.0.1:{server.server_address[1]}/api/v2/data/llms/models", seen, server


def run(url, key=KEY):
    env = dict(os.environ)
    env.pop("ARTIFICIAL_ANALYSIS_API_KEY", None)
    if key is not None:
        env["ARTIFICIAL_ANALYSIS_API_KEY"] = key
    env["ARTIFICIAL_ANALYSIS_API_URL"] = url
    return subprocess.run([sys.executable, str(SCRIPT)], env=env, capture_output=True, text=True, timeout=60)


class FetchModelIndex(unittest.TestCase):
    def test_each_figure_lands_where_the_bake_reads_it_and_the_key_only_in_its_header(self):
        url, seen, server = serve({"status": 200, "data": rows(120)})
        done = run(url)
        server.shutdown()
        self.assertEqual(done.returncode, 0, done.stderr)
        self.assertEqual([s["key"] for s in seen], [KEY])
        catalogue = json.loads(done.stdout)
        first = catalogue["models"]["model-0"]
        self.assertEqual(
            first,
            {"name": "Model 0", "intelligence": 40.0, "coding": 30.5, "input_usd_per_million": 1.25, "output_usd_per_million": 10},
        )
        self.assertEqual(len(catalogue["models"]), 120)
        self.assertIsInstance(catalogue["fetched_at"], int)
        # The shipped version is carried while the API names none.
        shipped = json.loads((ROOT / "crates/inference-gateway/data/model-index.json").read_text())
        self.assertEqual(catalogue["index_version"], shipped["index_version"])
        self.assertNotIn(KEY, done.stdout + done.stderr)

    def test_an_answer_too_small_to_be_a_catalogue_is_refused(self):
        url, _, server = serve({"status": 200, "data": rows(3)})
        done = run(url)
        server.shutdown()
        self.assertNotEqual(done.returncode, 0)
        self.assertEqual(done.stdout, "")
        self.assertIn("only 3 models carry an intelligence index", done.stderr)

    def test_a_refused_key_is_said_plainly_and_never_echoed(self):
        url, _, server = serve({"error": "bad key"}, status=401)
        done = run(url)
        server.shutdown()
        self.assertNotEqual(done.returncode, 0)
        self.assertIn("answered 401", done.stderr)
        self.assertNotIn(KEY, done.stdout + done.stderr)

    def test_no_key_asks_for_one_and_sends_nothing(self):
        url, seen, server = serve({"data": rows(120)})
        done = run(url, key=None)
        server.shutdown()
        self.assertNotEqual(done.returncode, 0)
        self.assertIn("set ARTIFICIAL_ANALYSIS_API_KEY", done.stderr)
        self.assertEqual(seen, [])


if __name__ == "__main__":
    unittest.main()
