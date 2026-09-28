"""A local, free stand-in for the Anthropic Messages endpoint.

A turn's first request is answered with a small cell (a fenced `sterna` program that lists the
project and reads the README); once the conversation holds an assistant reply, the answer is
plain text, which ends the turn. Every request is recorded in `self.requests`.
Set `script` to a list of replies to answer requests in order instead.
"""
import json, threading
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

CELL = ('```sterna\nconst files = await bash({command: "ls -la && git log --oneline -3"});\n'
        'const readme = await read({path: "README.md"});\n'
        'return {files: files.stdout, readme: readme.excerpt({lines: 5})};\n```')
ANSWER = ('The project has a README and `src/main.py`, which defines `add` and prints `add(2, 3)`. '
          'Last commit: init. Nothing else has changed.')


class FakeModel:
    def __init__(self, script=None):
        self.requests = []
        self.script = list(script or [])
        model = self

        class H(BaseHTTPRequestHandler):
            def log_message(self, *a):
                pass

            def do_GET(self):
                self.send_response(404); self.end_headers()

            def do_POST(self):
                n = int(self.headers.get('Content-Length') or 0)
                try:
                    body = json.loads(self.rfile.read(n) or b'{}')
                except Exception:
                    body = {}
                model.requests.append(body)
                text = model.reply(body)
                if body.get('stream'):
                    events = [
                        {"type": "message_start", "message": {"role": "assistant", "usage": {"input_tokens": 12}}},
                        {"type": "content_block_start", "index": 0, "content_block": {"type": "text", "text": ""}},
                        {"type": "content_block_delta", "index": 0, "delta": {"type": "text_delta", "text": text}},
                        {"type": "content_block_stop", "index": 0},
                        {"type": "message_delta", "delta": {"stop_reason": "end_turn"}, "usage": {"output_tokens": 40}},
                        {"type": "message_stop"},
                    ]
                    data = ''.join(f"event: {e['type']}\ndata: {json.dumps(e)}\n\n" for e in events).encode()
                    self.send_response(200)
                    self.send_header('Content-Type', 'text/event-stream')
                else:
                    data = json.dumps({"id": "msg_fake", "type": "message", "role": "assistant",
                                       "content": [{"type": "text", "text": text}], "stop_reason": "end_turn",
                                       "usage": {"input_tokens": 12, "output_tokens": 40}}).encode()
                    self.send_response(200)
                    self.send_header('Content-Type', 'application/json')
                self.send_header('Content-Length', str(len(data)))
                self.send_header('Connection', 'close')
                self.end_headers()
                self.wfile.write(data)

        self.server = ThreadingHTTPServer(('127.0.0.1', 0), H)
        self.url = f'http://127.0.0.1:{self.server.server_address[1]}'
        threading.Thread(target=self.server.serve_forever, daemon=True).start()

    def reply(self, body):
        if self.script:
            return self.script.pop(0)
        msgs = body.get('messages') or []
        if any(m.get('role') == 'assistant' for m in msgs):
            return ANSWER
        return CELL

    def stop(self):
        self.server.shutdown()
