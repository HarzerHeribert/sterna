"""Drive the real Sterna TUI in a pseudo-terminal: keys, mouse, screen text, colour PNGs.

    import sys; sys.path.insert(0, KIT)
    from tui import Tui
    t = Tui(fake=True)               # isolated settings, scratch project, free fake model
    t.wait('What next')              # wait until text shows
    t.type('/theme'); t.key('enter')
    print(t.screen())                # plain text of the screen
    x, y = t.find('Parrots')         # 1-based column/row of the first match
    t.click(x, y)                    # a real SGR mouse press + release there
    t.png('theme-sheet')             # colour screenshot -> OUT/theme-sheet.png (read it with the Read tool)
    t.close()

Modes:
  fake=True   `sterna session --model fixture-model` against a local fake Anthropic endpoint:
              prompts are free; the first request of a turn answers with a small cell, the
              next with plain text.
  fake=False  plain `sterna` in the scratch project with the REAL gateway and accounts.
              Use it for pickers/catalogue only. NEVER send a prompt in this mode.
Settings are always isolated (XDG_CONFIG_HOME in a temp dir): nothing touches the user's config.
Browsers are blocked: `open`/`xdg-open` are logging shims, so a sign-in can never open a page.
"""
import os, pty, fcntl, termios, struct, select, threading, time, json, tempfile, subprocess, signal, re, html, shutil
import pyte
from fake_model import FakeModel

KIT = os.path.dirname(os.path.abspath(__file__))
OUT = os.environ.get('TUI_OUT', os.path.join(KIT, 'shots'))
REPO = os.path.dirname(os.path.dirname(KIT))
CHROME = next((c for c in ('/Applications/Google Chrome.app/Contents/MacOS/Google Chrome',
                           shutil.which('google-chrome') or '', shutil.which('chromium') or '',
                           shutil.which('chromium-browser') or '') if c and os.path.exists(c)), None)
KEYS = {
    'enter': '\r', 'esc': '\x1b', 'tab': '\t', 'backtab': '\x1b[Z', 'space': ' ', 'backspace': '\x7f',
    'up': '\x1b[A', 'down': '\x1b[B', 'right': '\x1b[C', 'left': '\x1b[D',
    'home': '\x1b[H', 'end': '\x1b[F', 'pgup': '\x1b[5~', 'pgdn': '\x1b[6~', 'delete': '\x1b[3~',
    'f1': '\x1bOP', 'f2': '\x1bOQ', 'f3': '\x1bOR', 'f4': '\x1bOS', 'f5': '\x1b[15~', 'f6': '\x1b[17~',
    'f7': '\x1b[18~', 'f8': '\x1b[19~', 'f9': '\x1b[20~', 'f10': '\x1b[21~', 'f11': '\x1b[23~', 'f12': '\x1b[24~',
    'shift-up': '\x1b[1;2A', 'shift-down': '\x1b[1;2B', 'alt-enter': '\x1b\r',
}


def _scratch_project():
    root = tempfile.mkdtemp(prefix='tui-project-')
    os.makedirs(os.path.join(root, 'src'))
    with open(os.path.join(root, 'README.md'), 'w') as f:
        f.write('# demo\n\nA scratch project for driving the TUI.\n')
    with open(os.path.join(root, 'src', 'main.py'), 'w') as f:
        f.write('def add(a, b):\n    return a + b\n\nprint(add(2, 3))\n')
    subprocess.run(['git', 'init', '-q'], cwd=root)
    subprocess.run(['git', '-c', 'user.email=t@t', '-c', 'user.name=t', 'add', '.'], cwd=root)
    subprocess.run(['git', '-c', 'user.email=t@t', '-c', 'user.name=t', 'commit', '-qm', 'init'], cwd=root)
    return root


class Tui:
    def __init__(self, fake=True, cols=140, rows=45, binary=None, cwd=None, colorterm='truecolor',
                 env=None, args=None):
        self.cols, self.rows = cols, rows
        self.binary = binary or os.environ.get('TUI_BINARY') or next((b for b in (os.path.join(REPO, 'target', 'debug', 'sterna'),) if os.path.exists(b)), None) or shutil.which('sterna')
        self.cwd = cwd or _scratch_project()
        self.home = tempfile.mkdtemp(prefix='tui-config-')
        self.model = FakeModel() if fake else None
        e = dict(os.environ)
        for k in ('ANTHROPIC_API_KEY', 'ANTHROPIC_AUTH_TOKEN', 'ANTHROPIC_BASE_URL'):
            e.pop(k, None)
        e.update({'XDG_CONFIG_HOME': self.home, 'TERM': 'xterm-256color'})
        # Never let an audited session open a browser (sign-in flows): `open`/`xdg-open` are shims
        # that only log the URL to KIT/blocked-open.log. Pane, the gateway and the broker inherit PATH.
        e['PATH'] = os.path.join(KIT, 'shim') + os.pathsep + e.get('PATH', '')
        e['BROWSER'] = os.path.join(KIT, 'shim', 'open')
        e.setdefault('TUI_KIT_LOG', os.path.join(OUT, 'blocked-open.log'))
        if colorterm:
            e['COLORTERM'] = colorterm
        else:
            e.pop('COLORTERM', None)
        if fake:
            e['ANTHROPIC_BASE_URL'] = self.model.url
            argv = [self.binary, 'session', '--root', self.cwd, '--model', 'fixture-model',
                    '--gateway', os.path.join(self.cwd, 'no-gateway')]
            e['INFERENCE_GATEWAY_BIN'] = os.path.join(self.cwd, 'no-gateway')
        else:
            argv = [self.binary]
        argv += list(args or [])
        e.update(env or {})
        self.screen_ = pyte.Screen(cols, rows)
        self.stream = pyte.ByteStream(self.screen_)
        self.lock = threading.Lock()
        pid, fd = pty.fork()
        if pid == 0:
            os.chdir(self.cwd)
            os.execve(self.binary, argv, e)
        self.pid, self.fd = pid, fd
        fcntl.ioctl(fd, termios.TIOCSWINSZ, struct.pack('HHHH', rows, cols, 0, 0))
        self.alive = True
        self.raw = bytearray()
        threading.Thread(target=self._pump, daemon=True).start()
        time.sleep(0.3)

    def _pump(self):
        while self.alive:
            try:
                r, _, _ = select.select([self.fd], [], [], 0.05)
                if r:
                    data = os.read(self.fd, 65536)
                    if not data:
                        break
                    with self.lock:
                        self.raw += data
                        self.stream.feed(data)
            except OSError:
                break
        self.alive = False

    # ---- input -----------------------------------------------------------------------------
    def send(self, text):
        os.write(self.fd, text.encode() if isinstance(text, str) else text)
        time.sleep(0.12)

    def type(self, text, delay=0.02):
        for ch in text:
            os.write(self.fd, ch.encode())
            time.sleep(delay)
        time.sleep(0.12)

    def key(self, name, times=1):
        """'enter', 'esc', 'tab', 'up', 'f4', 'ctrl-b', 'shift-up', … (see KEYS)."""
        for _ in range(times):
            if name.startswith('ctrl-') and len(name) == 6:
                self.send(chr(ord(name[5].lower()) & 31))
            else:
                self.send(KEYS[name])

    def paste(self, text):
        self.send('\x1b[200~' + text + '\x1b[201~')

    def click(self, x, y, button='left'):
        """x, y are 1-based screen column/row (what find() returns)."""
        b = {'left': 0, 'middle': 1, 'right': 2}[button]
        self.send(f'\x1b[<{b};{x};{y}M')
        self.send(f'\x1b[<{b};{x};{y}m')

    def drag(self, x1, y1, x2, y2):
        self.send(f'\x1b[<0;{x1};{y1}M')
        steps = max(abs(x2 - x1), abs(y2 - y1), 1)
        for i in range(1, steps + 1):
            x = x1 + (x2 - x1) * i // steps
            y = y1 + (y2 - y1) * i // steps
            self.send(f'\x1b[<32;{x};{y}M')
        self.send(f'\x1b[<0;{x2};{y2}m')

    def scroll(self, x, y, up=True, times=1):
        for _ in range(times):
            self.send(f'\x1b[<{64 if up else 65};{x};{y}M')

    def hover(self, x, y):
        self.send(f'\x1b[<35;{x};{y}M')

    # ---- output ----------------------------------------------------------------------------
    def screen(self):
        with self.lock:
            return '\n'.join(line.rstrip() for line in self.screen_.display)

    def find(self, text, nth=0):
        """1-based (x, y) of the nth occurrence of text, or None."""
        with self.lock:
            hits = []
            for y, line in enumerate(self.screen_.display):
                for m in re.finditer(re.escape(text), line):
                    hits.append((m.start() + 1, y + 1))
        return hits[nth] if len(hits) > nth else None

    def wait(self, text=None, timeout=8.0, gone=None):
        """Wait until `text` appears (or `gone` disappears). Returns True/False."""
        end = time.time() + timeout
        while time.time() < end:
            s = self.screen()
            if (text is None or text in s) and (gone is None or gone not in s):
                return True
            time.sleep(0.1)
        return False

    def settle(self, seconds=0.6):
        time.sleep(seconds)

    def png(self, name, light=False):
        """Colour screenshot of the screen as OUT/<name>.png; returns the path."""
        os.makedirs(OUT, exist_ok=True)
        if not CHROME:
            raise RuntimeError('png() needs Chrome or Chromium; use screen() for text')
        fg0, bg0 = ('#1B2229', '#F4F6F8') if light else ('#D6DEE5', '#0A0E12')
        names = {'black': '#000000', 'red': '#CD3131', 'green': '#0DBC79', 'brown': '#E5E510', 'yellow': '#E5E510',
                 'blue': '#2472C8', 'magenta': '#BC3FBC', 'cyan': '#11A8CD', 'white': '#E5E5E5',
                 'brightblack': '#666666', 'brightred': '#F14C4C', 'brightgreen': '#23D18B', 'brightyellow': '#F5F543',
                 'brightblue': '#3B8EEA', 'brightmagenta': '#D670D6', 'brightcyan': '#29B8DB', 'brightwhite': '#FFFFFF'}
        def col(c, default):
            if c == 'default':
                return default
            if re.fullmatch(r'[0-9a-fA-F]{6}', c or ''):
                return '#' + c
            return names.get(c, default)
        rows = []
        with self.lock:
            for y in range(self.rows):
                line = self.screen_.buffer[y]
                spans = []
                for x in range(self.cols):
                    ch = line[x]
                    fg, bg = col(ch.fg, fg0), col(ch.bg, bg0)
                    if ch.reverse:
                        fg, bg = bg, fg
                    style = f'color:{fg};background:{bg};' + ('font-weight:700;' if ch.bold else '') + \
                            ('text-decoration:underline;' if ch.underscore else '') + ('font-style:italic;' if ch.italics else '')
                    spans.append(f'<span style="{style}">{html.escape(ch.data or " ")}</span>')
                rows.append(''.join(spans))
        page = ('<html><head><style>body{margin:0;background:%s}pre{margin:0;font:14px/1.0 Menlo,monospace;}'
                'pre span{display:inline-block;width:8.43px;height:17px;line-height:17px;overflow:hidden;vertical-align:top}</style>'
                '</head><body><pre>%s</pre></body></html>') % (bg0, '\n'.join(rows))
        hp = os.path.join(OUT, name + '.html'); pp = os.path.join(OUT, name + '.png')
        open(hp, 'w').write(page)
        w, h = int(self.cols * 8.43) + 4, self.rows * 17 + 4
        subprocess.run([CHROME, '--headless=new', '--disable-gpu', '--hide-scrollbars', '--force-device-scale-factor=1',
                        f'--screenshot={pp}', f'--window-size={w},{h}', 'file://' + hp],
                       stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, timeout=60)
        return pp

    def close(self):
        if self.alive:
            try:
                os.kill(self.pid, signal.SIGTERM)
                time.sleep(0.4)
                os.kill(self.pid, signal.SIGKILL)
            except ProcessLookupError:
                pass
        self.alive = False
        if self.model:
            self.model.stop()
        try:
            os.waitpid(self.pid, os.WNOHANG)
        except ChildProcessError:
            pass
