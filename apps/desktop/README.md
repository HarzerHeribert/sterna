# Sterna desktop

The desktop app: a window around the Sterna engine. It holds no agent
logic. It starts the user's host (`sterna host --background`), keeps the
host's list of every folder and session, and attaches to each running
session's port, as `docs/engine.md` describes. Every word about a cell's
state comes from the engine's `reading`.

```
ui/            the window: vanilla JS and CSS, built by vite into dist/
  src/bridge/  the one seam to the machine: tauri.js in the app, socket.js in a browser
src-tauri/     the Tauri v2 shell: starts the host, carries lines to and from loopback ports,
               the folder chooser, opening files and links, asking before quitting,
               opening a newer release once it is in place
dev/           development only: a mock host (mock-host.mjs, mock-sterna.mjs), the bridge a
               browser talks to (bridge.mjs), a scripted model endpoint (provider.mjs)
e2e/           Playwright checks of the UI in Chromium, one bridge per check
scripts/       sidecars.sh (copies the engine beside the app), shots.mjs, icon.mjs
```

## Build the app

Needs Node 22, Rust from rustup (the repository's `rust-toolchain.toml`
applies), and on Linux the WebKitGTK 4.1 packages listed in
`.github/workflows/desktop.yml`.

```sh
cargo build --release -p sterna -p inference-gateway   # the engine, from the repository root
cd apps/desktop
npm install
bash scripts/sidecars.sh            # target/release (or --profile debug) → src-tauri/binaries/sterna-desktop-{engine,gateway}-<triple>
npm run tauri build                  # dist/, then src-tauri/target/release/bundle/{macos,dmg}/…
```

On macOS, `CI=true npm run tauri build` skips the Finder window layout the
dmg script otherwise drives with AppleScript (which needs permission to
control Finder). The `.app`'s `Contents/MacOS` holds `Sterna` and, beside it,
the engine as `sterna-desktop-engine` and the gateway as
`sterna-desktop-gateway` (namespaced, so a Linux package never installs a
generic `/usr/bin/sterna`). The app runs the installed command-line
`sterna` (`$STERNA_HOME/current/bin/sterna`, else `~/.local/lib/sterna/…`)
unless its own engine is newer, and never runs a host from a disk image or
an AppImage mount: it copies its engine out first.

In a debug build `STERNA_BIN` points the app at another `sterna` (a `.mjs`
path runs with Node), for example `dev/mock-sterna.mjs`; a release build
ignores it.

The window keeps no settings of its own on disk: the theme, the motion and
how a cell being written is shown are the person's settings (`ui.theme`,
`ui.motion`, `ui.stream`, the same the terminal reads), and everything else
is kept by the host's `set_preferences`.

Newer releases come through the engine, as the terminal's do: when the
window opens it sends the host's `update` check (docs/engine.md), and when
a newer release is out a card at the foot of the sessions list offers
Update. The host places the release beside the running one; Restart
leaves every session running, as Keep running does, and opens the new app
from where the person opens it (`src-tauri/src/restart.rs`). With
`STERNA_DISABLE_AUTOUPDATE` set nothing is said until Check now, in
Settings, Updates; a copy the updater does not move says why there, with
the releases page.

## Type

The window draws with three bundled fonts, so every platform lays out the
same widths: Inter for the interface (400, 500, 600), Barlow Condensed for
the folder name and the opening, IBM Plex Mono for code. Each is under the
SIL Open Font License 1.1 and comes from its `@fontsource` package; the
system's own font stands behind Inter only as a fallback. Counts and times
that sit in columns use tabular figures.

## Run it in a browser

```sh
npm run dev                              # http://127.0.0.1:5199/
npm run bridge:mock -- --scenario busy   # a mock host: four folders, four running sessions
```

The bridge prints the page to open (`"open"` in its ready line): it takes
a WebSocket only from the dev server's page, and only with the secret in
that address, and it connects only to ports the host announced.
`--scenario empty` is a first run; `--pace 0.5` plays the mock's turns
faster; `--releases newer` has its release check find a newer release
(also `current`, `manual`, `unmoved`, `unreachable`, `fails`). Against the real engine: `node dev/bridge.mjs --sterna
../../target/debug/sterna`. Add `&dev` to the address for a development
switcher (light, dark, a scripted task, the overview, quit); a built app
never has it, nor the checks' `window.__sterna` handle.

## Test

```sh
npm test            # unit tests (node --test), then the Playwright checks
npm run test:unit
npm run test:e2e
npm run shots       # shots/<state>-<light|dark>-<1480|980>.png, release-card the sidebar's card
```

The checks use Chromium from Playwright's own cache. They run the real
`sterna host` (`STERNA_E2E_BIN`, else `target/debug/sterna`) in a world of
their own (`e2e/world.mjs`): scratch data and settings folders,
`dev/provider.mjs` as the model, a fake gateway script that records what it
is handed, and no credential, the way the engine's own checks do. Without
that binary they fail and say what to build. Three specs use
the mock host: `e2e/busy.spec.mjs`, for several sessions held at once in
states the real engine passes through in a moment (a cell being written, a
cell running) and the one row grid they are drawn in;
`e2e/window.spec.mjs`, for the window's shape at any width (the side
panels hidden, or folded below 860 and 1100 px without the person's choice
changing, long names and words, clocks that tick without redrawing what is
around them) and a button pressed twice; and
`e2e/versions.spec.mjs`, for plan goal 17 in the window: a newer release
offered, moved to, failing and tried again, and Restart, which the browser
bridge only counts. A copy that does not move itself is checked on the real
engine (`e2e/sheets.spec.mjs`); the move itself, from release archives, by
the engine's `crates/sterna/tests/release_channel.rs` and
`desktop_release.rs`.
