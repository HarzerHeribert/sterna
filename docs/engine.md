# The engine and its clients

A Sterna session is an **engine**: the model loop, its cells, approvals,
settings and records. Everything a person sees or does reaches it through
one seam, the **session port**. The terminal is its first client; the
desktop app and the tests are others. Code: `crates/sterna/src/engine/`.

```
terminal ─┐                       ┌─ session process (folder A) ── gateway
desktop ──┼── session port(s) ────┤
tests   ──┘                       └─ session process (folder B) ──┘
             host port ── sterna host (the list, one gateway, usage)
```

- **One session, one process.** `sterna` in a terminal runs its session in
  its own process and serves its port beside the terminal. The desktop's
  sessions are started by the host, each as its own process
  (`sterna session --serve`). Closing a terminal ends its session; the
  terminal never starts a host.
- **The host** (`sterna host`) keeps the one list of folders and sessions,
  starts sessions, owns one gateway they share, adds up their usage, and
  stays running when a client quits and asks to keep its sessions.
- **Decided once.** How a session reads -- a cell's state, the line under
  its card, the answer's facts -- is decided in the engine and sent as
  words (`reading`); a client draws them and decides none of them.

## Transport

Loopback TCP, newline-delimited JSON, UTF-8, one object per line. The same
on macOS, Linux and Windows.

The first line a client sends is its hello; nothing else is read before it:

```json
{"hello":{"token":"<hex>","protocol":1,"client":"desktop"}}
```

The answer is one line, `{"welcome":{"protocol":1,"session":"<id>"}}` on a
session port or `{"welcome":{"protocol":1,"host":"<version>"}}` on the host
port, or `{"refused":{"reason":"…"}}` and the connection closes. A wrong
token, a missing hello or another protocol number is refused.

`client` is a name the client chooses; it is what `settled.by` reports.
The terminal's name is `terminal`.

## Where to find them

The user's data folder holds the engine's state: `$XDG_DATA_HOME/sterna`
when that variable is set, else `~/Library/Application Support/sterna`
(macOS), `~/.local/share/sterna` (Linux), `%LOCALAPPDATA%\sterna\data`
(Windows). A session whose user settings were moved with `XDG_CONFIG_HOME`
(a test's, a portable install's) keeps its data beside them, in
`$XDG_CONFIG_HOME/sterna/data`, and never lists itself among the person's
own sessions. Every file here is the user's own; tokens are never readable
by a confined tool.

| file | what it is |
|---|---|
| `host.json` | `{"listening":"127.0.0.1:<port>","token":…,"pid":…,"version":…,"protocol":1}` while a host runs |
| `host.lock` | held by the running host; a second host refuses to start |
| `sessions.json` | the list: every folder and session, with when each was last used |
| `live/<id>.json` | a running session's `{"id","root","listening","token","pid","started"}` |

`sterna host` prints the same `host.json` object as its one ready line on
stdout, the way the gateway does, and runs until told to stop.
`sterna host --background` starts one detached if none runs, prints its
ready line and returns.

## The session port

### Commands

A command is `{"do":"<name>", …}`. A command the session cannot take is
answered, to that client only, with
`{"kind":"refused","to":"<name>","reason":"…"}`.

| command | fields | does |
|---|---|---|
| `attach` | `from` (optional seq) | a `snapshot`, then every event after it; with `from`, every event from that seq instead, then live |
| `submit` | `text`, `images` (paths, optional) | a message; held in the session's queue while a turn runs |
| `take_back` | | the newest queued message comes off the queue |
| `stop` | `by` (optional: `you` or `interrupt`) | stop after this cell; `interrupt` is Ctrl-C, and two within two seconds end the session |
| `cancel` | | cancel the call in flight |
| `answer` | `prompt`, `answer` | answers a prompt (below); the first answer wins |
| `control` | `line` | a slash command, as typed in the composer (`/model x`, `/effort high`) |
| `set_level` | `level`, `save` | the sandbox level: `ask`, `sandboxed` or `full` |
| `forget` | `id` | forget an answer remembered for the session |
| `rollback` | | undo the newest cell that changed files, without a preview |
| `end` | | end the session |

An `answer` is one of `{"approval":"allow_once" | "allow_for_session" |
"deny" | "deny_once" | "cancel" | "allow_host_session" |
"allow_host_always"}`, `{"redirect":"<words>"}`, `{"choice":"<text>"}`
(a question), `{"form":["<value>", …]}` or `{"dismiss":null}`.

### Events

Every event is one line: `{"seq":<n>,"at":<unix ms>,"kind":"<kind>", …}`.
`seq` counts up from 1 in a session and never repeats, and `at` is the
session process's own clock. A session keeps every event for as long as it
runs, so a client that attaches with `from: 1` gets exactly the events a
client attached from the start was sent, and no snapshot. A client ignores
a kind it does not know.

**Some lines are for one client only** and carry `seq: 0`: the
`snapshot`, a `refused`, and what the session builds in answer to one
client's input -- a `panel`, a form's `prompt`, a sign-in's progress. They
are never replayed.

| kind | fields | |
|---|---|---|
| `snapshot` | `state` | everything a client needs to draw the session now |
| `transcript` | `conversation`, `notebook`, `served`, `activity`, `reading` | the record changed; `activity` is `null` when a command was answered and no turn ran |
| `activity` | `activity`, `since` | `idle`, `starting`, `thinking`, `streaming`, `executing`, `searching`, `waiting`, `compacting`, `awaiting_you`, `complete`, `failed`, `stopped` (by a person), `interrupted` (by Ctrl-C); `since` is when it began (unix ms). A turn ends with `complete`, `failed`, `stopped` or `interrupted` |
| `delta` | `text` | prose as it arrives |
| `tool_delta` | `text` | the cell being written, as it arrives |
| `reasoning` | `text` | readable reasoning as it arrives |
| `prompt` | `prompt` | an approval, a question or a form waits for an answer |
| `settled` | `id`, `by`, `answer` | a prompt was answered, by client name or `"session"` |
| `queue` | `items` | the queued messages, oldest first, sent on every change (an empty queue too) |
| `notice` | `text` | a notice |
| `panel` | `panel` | a sheet the session built |
| `facts` | `facts` | `{"model","effort","level","root","subagents"}` changed |
| `usage` | `usage` | tokens used so far |
| `ended` | `reason` | the session ended |

A `prompt` is `{"id":<n>,"type":"approval"|"question"|"form", …}`. An
approval carries `tool`, `label`, `target`, `confirmation`, `reason`,
`hosts` and `leaves_sandbox`; a question `question` and `choices`; a form
its fields, never their values.

The snapshot's `state` holds `session`, `seq` (the last event it
includes), `facts`, `conversation`, `notebook`, `served`, `activity`,
`since`, `streaming`, `queue`, `prompts`, `notes`, `memory`, `hosts`,
`suggestions`, `reading` and `usage`. `notebook.cells[i]` is cell `i + 1`'s
view: `description`, `changes` (a unified diff), `stdout`, `output`,
`returned`, `execution`, `error`, `call_count`, `asked`, `rolled_back`. A
turn's answer joins `conversation` as an assistant message.

`reading` is the words a client shows, decided once:
`{"cells":[{"cell":<n>,"state":"EXECUTED","mark":"✓","tone":"success","line":"✓ executed · 1 file changed","parts":[…],"facts":…}],"answer":{"cell":<n>,"facts":"1 file · +1 −0 · 1 call","mark":"✓","failed":false}}`.
Cells count from 1; `answer` is the newest answer's, `null` until there is
one, and `complete.` or `failed.` when its cell changed nothing and made no
call. A cell still running has no reading: it is drawn from `activity`.

### Prompts, once

A prompt is answered once. The first `answer` settles it: every client is
sent `settled` with the answer and who gave it, once. An answer to a prompt
that is settled or unknown is `refused` (`to: "answer"`) to that client and
changes nothing. A session with a client attached -- a terminal or a port
-- is attended: an approval or a question waits for a client rather than
being refused as nobody's to answer.

### Two sessions in one folder

Each session's diff and rollback are its own. A `rollback` that would undo
a change another session made to a file since is refused: the session says
so as a `notice` to every client -- or `refused` (`to: "rollback"`) to the
sender -- naming the file by its path in the folder and the other session
by its id, and nothing changes.

## The host port

The host answers each command line with one line: `{"ok":{…}}` or
`{"error":"…"}`. `watch` answers `{"ok":{}}` and then sends the list each
time it changes.

| command | fields | answers |
|---|---|---|
| `list` | | `{"folders":[{"root","last_used","sessions":[{"id","title","last_used","live"}]}]}`, folders and sessions newest first |
| `start` | `root`, `task` (optional), `model` (optional) | `{"id","listening","token"}` |
| `locate` | `id` | `{"id","listening","token"}` for a running session |
| `stop` | `id` | ends a running session |
| `usage` | | `{"sessions":[{"id","input_tokens","output_tokens"}],"total":{"input_tokens","output_tokens"}}` |
| `quit` | `keep` | the client is leaving: `keep: false` ends the sessions the host started, `keep: true` leaves them running |
| `watch` | | the list, again on every change |
| `shutdown` | | the host ends its sessions and exits |

`live` is `null` for a session that is not running, else
`{"state":"idle"|"thinking"|"writing"|"running"|"waiting","since"}`.

- **One id everywhere.** `start`'s `id`, the welcome's `session`,
  `live/<id>.json`, the list, `<root>/.sterna/sessions/<id>.jsonl`,
  `sterna --sessions` and `--resume` name a session by the same id, whether
  a terminal or the host started it.
- **The list.** Every session writes its own entry, whether or not a host
  runs, under `sessions.lock`. A session's `last_used` (Unix ms) moves when
  a person sends it a message, never on attach, end, shutdown or restart; a
  folder's is its newest session's. Ties are ordered by id, then root. A
  root is the folder resolved, as the operating system spells it.
- **A session the host starts stays running** until it is told to `end`,
  the host is told to `stop` it, a client quits with `keep: false`, the
  host shuts down, or the host dies; finishing its task does not end it.
  `quit` with `keep: false` ends every session this host started. The
  host's own connections to its sessions call themselves `host`.
- **Usage** covers the sessions this host started: every request each
  made, as the provider counted it. It may trail a session's own `usage`.
A session started in a terminal appears in the list like any other; the
terminal's own `/resume` and `sterna --resume` still offer only the
sessions of the folder they run in.
