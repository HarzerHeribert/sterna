# tui-kit: drive the real Sterna TUI

Keys, mouse (real SGR mouse reports), screen text and colour screenshots of the
actual binary in a pseudo-terminal, against a free fake model. Use it to check a
TUI change the way a person would meet it.

```sh
python3 -m venv /tmp/tui-venv && /tmp/tui-venv/bin/pip install pyte
cargo build -p sterna
/tmp/tui-venv/bin/python - <<'PY'
import sys; sys.path.insert(0, 'scripts/tui-kit')
from tui import Tui
t = Tui(fake=True, cols=140, rows=42)
try:
    t.wait('What should we build', 15)
    t.type('/theme'); t.key('enter'); t.settle()
    print(t.screen())
    x, y = t.find('Parrots'); t.click(x, y)
    t.png('theme-sheet')          # needs Chrome/Chromium; screen() works without
finally:
    t.close()
PY
```

- `fake=True` (default): `sterna session --model fixture-model` against a local fake
  Anthropic endpoint. Prompts are free: a turn gets one small cell, then a plain
  answer. `t.model.script = [...]` scripts the replies (a cell that writes a file
  brings up an approval; a cell calling `ask` brings up the ask sheet).
- `fake=False`: plain `sterna` with the real gateway and accounts. Pickers and
  catalogue only: never send a prompt, never submit a sign-in or key form.
- Settings are isolated per session (`XDG_CONFIG_HOME` in a temp dir).
- Browsers are blocked: `open`/`xdg-open` are shims on `PATH` that only log.
- `close()` kills only its own process. Never `pkill`/`killall` anything.
- Knobs: `Tui(cols=80, rows=24)`, `Tui(colorterm=None)` (no true colour),
  `t.png(name, light=True)` (a light terminal), `t.drag()`, `t.scroll()`, `t.hover()`.
