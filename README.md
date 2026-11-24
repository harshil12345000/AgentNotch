# Notch

A tiny always-on-top notch for Windows 10/11 that shows your AI coding agent sessions
live. It sits flush against a screen edge like the MacBook notch, opens when you hover
it, tells you when a session finishes or needs input, shows your usage limits, and has
two one-click toggles: Claude Code Remote Control and Keep Awake (lid closed).

Native Rust, Direct2D/DirectWrite, one exe (about 1 MB), no installer, no runtime.

> See "What is and is not verified" at the end for what has been measured and what has not.

## Run it

```
cargo build --release
target\release\notch.exe
```

The exe is self-contained (static C runtime, fonts, logos and icon inside).

### Install as an app

```
target\release\notch.exe --install
```

This copies the exe to `%LOCALAPPDATA%\Programs\Notch\notch.exe`, adds a **Notch**
shortcut to the Desktop and the Start Menu, points any installed hooks and the
Start-with-Windows entry at that copy, and starts it. Nothing needs administrator rights.
Run it again after a rebuild to update the installed copy.

Taskbar: Windows does not let a program pin itself. Right-click the Notch shortcut and
choose **Pin to taskbar**. Notch has no taskbar window of its own; clicking the pinned
icon (or the desktop shortcut) while it is running shows the notch and opens it.

`notch.exe --uninstall` removes the shortcuts and the Start-with-Windows entry.

On first start it asks whether to install the Claude Code hooks (see below).

- **Hover** the notch to open it; move away to close it. Clicking the header closes it
  at once. (With "Expand on hover" off in Settings, a click opens it and the next click
  closes it.)
- **Drag** it to any screen edge: top, bottom, left or right, anywhere along the edge.
  It attaches when its centre comes within 56 px of an edge. On the bottom edge it sits
  on top of the taskbar and opens upward. Dropped away from the edges it floats as a pill.
  The position is remembered per monitor setup.
- **Ctrl+\\** hides and shows it. Change the keys in Settings (click the row, press the
  new combination; Backspace clears it, Esc cancels).
- Blue dots (working) are steady. Amber (needs input), green (finished) and red (error)
  pulse, because those want you.
- Every Claude Code session that is open is listed, working or idle, for as long as its
  process lives. Other agents' finished sessions leave after 2 minutes.
- Click a session to go to it. A session inside the Claude desktop app opens in the app
  at exactly that session; a terminal session has its window raised.
- When a session asks for permission, its row shows **Approve** and **Deny**. The toast
  has the same two buttons. See "Approve / deny" below for when this is offered.
- Clicking a toast takes you to that session.
- Every subagent running inside a session is listed under its row, with its description
  and what it is doing. They never change the session's own state: a session whose turn
  has ended still shows as finished or waiting while a background subagent keeps working.
- A turn that ends with a question for you shows as **needs input** (amber) with the
  question; a turn that just ends shows as **done** (green).
- On the left or right edge the collapsed tab shows three dots and then an arrow when
  there are more sessions; the open panel is narrower and taller there, with a scrolling
  list (mouse wheel).
- Hover a session row and press the **x** to dismiss it. Only running sessions are
  recovered at startup; finished ones leave after 2 minutes (Settings: "Keep finished
  sessions").
- Click a session to bring its terminal or editor window to the front.
- Tray icon: Show/Hide, Clear finished sessions, Settings, Start with Windows, Quit.

Config: `%APPDATA%\Notch\config.toml`. Logs: `%LOCALAPPDATA%\Notch\logs` (two files of
2 MB at most). Nothing leaves the machine except the usage API call.

## Command line

```
notch.exe                       start
notch.exe --status              what Notch detects (lid action, keep-awake, Remote Control, hooks)
notch.exe --quit                ask the running notch to exit (restores power settings)
notch.exe --install-hooks       add the Claude Code hooks      [--settings <file>] [--wsl <distro>]
notch.exe --uninstall-hooks     remove them                    [--settings <file>] [--wsl <distro>]
notch.exe --hooks-status
notch.exe --bench [secs]        log working set and CPU every second, then exit  [--expanded] [--demo]
notch.exe --snapshot f.bmp      render the UI with demo data  [--expanded] [--settings] [--edge top|bottom|left|right|free] [--at 0..1]
```

## How Claude Code sessions are tracked

Three sources, most trusted first:

1. **Claude Code's registry of open sessions**, `~/.claude/sessions/<pid>.json`. Claude
   Code keeps one file per running session (terminal and desktop app) with its process
   id, title, folder and a live status it rewrites on every change: `busy`, `idle`, or
   `waiting` with what it is waiting for. Notch watches the folder. This decides which
   sessions exist, their titles, and when one is waiting on you. It needs no hooks.
2. **Hooks** (below): the current step, permission requests you can answer, the question
   a turn ended with.
3. **Transcripts**, `~/.claude/projects/<project>/<session>.jsonl` and
   `<session>/subagents/agent-*.jsonl` + `.meta.json`: the current step for sessions
   without hooks, the end of a turn (a second witness if a hook is missed), and every
   running subagent (the meta file says what it is and whether it has finished).

All three file layouts are internal to Claude Code and can change with an update. Each
source is read defensively and independently, so if one stops making sense the others
carry on. Checked against Claude Code 2.1.319 and 2.1.325.

## Approve / deny

With the hooks installed, the `PermissionRequest` hook is the one hook that waits: its
helper holds the request open, Notch shows Approve / Deny, and your click is passed back
to Claude Code as the hook's decision. Deny rules in your Claude settings still win over
an Approve.

- Offered for sessions hosted by the Claude desktop app or an SDK app. There the hook
  runs alongside the host's own prompt and whichever is answered first applies, so you
  can still answer in Claude as usual.
- Not offered for terminal sessions by default. There it is not documented whether Claude
  Code shows its own prompt while a hook is still deciding, so Notch lets the hook go at
  once and Claude Code asks as usual. Set `approve_in_terminal = true` in the config to
  try it.
- If you answer in Claude instead, or the turn moves on, Notch releases the hook and the
  buttons disappear.
- Settings > "Approve / deny from the notch" switches the whole feature off (the hook
  then returns immediately).
- From a script: `notch.exe --decide <session-id> allow|deny`.

## Claude Code hooks

Hooks are what make sessions update live (permission prompts, finished turns).

Install: answer Yes on first start, or Settings > "Claude Code hooks", or
`notch.exe --install-hooks`.

What it does to `~/.claude/settings.json`:

- Saves a copy first as `settings.json.notch-backup` (only the first time, so the backup
  is always the file as it was before Notch).
- Appends one entry per event after your existing hooks. Nothing of yours is changed,
  removed or reordered. Events: SessionStart, UserPromptSubmit, PreToolUse, PostToolUse,
  PostToolUseFailure, PermissionRequest, Notification, Stop, StopFailure, SubagentStart,
  SubagentStop, PreCompact, SessionEnd.
- Each entry runs `notch.exe --hook` in exec form with `"async": true`, so Claude Code
  never waits for it. The one exception is PermissionRequest, which is synchronous so it
  can return your Approve / Deny (see above); it returns at once whenever Notch is not
  running or does not offer the buttons. The helper reads the hook JSON from stdin, forwards it over
  `\\.\pipe\notch` and exits 0. If Notch is not running it exits 0 at once.

Uninstall: Settings, or `notch.exe --uninstall-hooks`. If you changed nothing else in the
meantime the file is restored byte for byte from the backup; otherwise only Notch's
entries are removed and your other edits stay.

The hooks store the path of the exe. If you move `notch.exe`, install again.

Without hooks, Notch still follows sessions by watching
`~/.claude/projects/<project>/<session>.jsonl` (it reads only appended bytes). That is
slower to notice things and cannot see permission prompts.

## WSL

- Hooks: start the distro, then `notch.exe --install-hooks --wsl <distro>`. This edits
  `~/.claude/settings.json` inside the distro. The hook runs the Windows exe through WSL
  interop (`/mnt/c/.../notch.exe --hook --wsl <distro>`), which reaches the named pipe
  directly. It needs interop enabled (the default).
- Notch never boots a distro: it only looks at distros that are already running.
- Transcripts: set `[wsl] enabled = true` in the config to read running sessions from
  `\\wsl$\<distro>\home\<user>\.claude\projects` at startup. WSL file systems do not send
  change notifications, so after startup WSL sessions update through hooks only.
- TCP fallback: `[wsl] tcp_fallback = true` makes Notch also listen on 127.0.0.1 on a
  random port. Port and a token are written to `%LOCALAPPDATA%\Notch\ipc` (line 1 port,
  line 2 token). A hook can then send
  `{"v":1,"agent":"claude","tok":"<token>","wsl":"<distro>","p":<hook json>}` to that
  port. This only works when WSL uses mirrored networking; with the default NAT mode
  127.0.0.1 inside WSL is not the Windows host, so use the interop hook above.

## Codex

- Hooks (best): `notch.exe --install-hooks --codex` adds entries to `~/.codex/hooks.json`
  next to any hooks already there (same merge, backup and byte-exact uninstall as for
  Claude Code). Codex then asks you to review and trust the new hooks (`/hooks`) before
  it runs them. Hooks are the only way Notch can see a Codex approval request.
  Remove with `notch.exe --uninstall-hooks --codex`.
- Rollout files under `~/.codex/sessions` are tailed as a fallback (turn started, tool
  call, turn finished). Approvals are not written there, and Codex builds that keep
  sessions in a database write no rollout files at all, so this fallback can see nothing.
- Notch does not touch Codex's `notify` setting. If you want to chain it yourself, the
  receiver is `notch.exe --codex-notify <json>` (turn finished only).

## T3 Code

T3 Code has no hooks or notify command. Notch reads its state database,
`~/.t3/userdata/statev2.sqlite`, read-only through the SQLite that ships with Windows
(`winsqlite3.dll`), each time the file changes. Each thread's latest run becomes a session:
running, waiting for approval or input, finished, failed.

- Individual tool calls inside a run are not shown, only the run's state.
- The table names were checked against a real database (November 2026). If a T3 Code
  update changes them, the query fails and the adapter goes quiet; `notch.exe --status`
  says whether the database is readable.
- A Claude session that T3 Code starts with your normal Claude settings also fires the
  Claude Code hooks. When T3's run record names that session, Notch folds the two rows
  into the T3 one. This folding is implemented from the database layout and has not been
  seen working on a live T3 run.

## Usage limits

- Source A: `GET https://api.anthropic.com/api/oauth/usage` with the OAuth token Claude
  Code keeps in `~/.claude/.credentials.json`. Notch only reads that file; the token is
  sent only to api.anthropic.com and is never logged or shown. The endpoint is
  undocumented and can change.
- That token goes stale when Claude Code is not used from a terminal (for example when
  you only use the desktop app). Notch never refreshes or writes it. Instead it runs
  `claude auth status` hidden, at most once every 20 minutes, which makes Claude Code
  renew its own file.
- Source B (only when A fails): a local estimate from the transcripts, like ccusage:
  tokens in the current 5-hour block and the last 7 days. Token counts are not shown.
  If you set `estimate_limit_5h_tokens` / `estimate_limit_week_tokens` under `[usage]`
  they are turned into an approximate percentage (marked `~`); otherwise the panel says
  the limits are unavailable and why. The first scan reads a week of transcripts once;
  after that only appended bytes are read (cache: `%LOCALAPPDATA%\Notch\usage-cache.tsv`).
- The last real numbers are saved (`%LOCALAPPDATA%\Notch\usage-last.tsv`) and shown at
  once after a restart. Real percentages are never replaced by an estimate or an error;
  when they are more than 7 minutes old the panel says "as of ... ago".
- Polled every 5 minutes and when you open the panel (at most once a minute). No polling
  while the PC is locked or idle.

## Remote Control

The button opens a terminal tab in a project folder and runs `claude remote-control`.
Folder: the selected session's, else the default folder from Settings, else the last
project an agent worked in. Terminal: Windows Terminal, PowerShell or WSL (Settings).
Click again to stop it.

It shows **On - already running** when Remote Control is active without Notch having
started it: a Claude process started with `remote-control` / `--rc`, or a running session
whose transcript carries a Remote Control marker (for example the Claude desktop app's
"Connect new sessions to Remote Control"). Notch cannot stop those.

## Travel mode

One button (in the panel, and in the tray menu) for working away from your desk:

- switches **Keep Awake** on,
- starts **Remote Control** if it is not already running,
- covers the screen with a reminder, **Turn On Mobile Hotspot**, until you dismiss it
  (click anywhere, or Esc / Enter / Space). Its button opens Windows' hotspot settings.

Clicking it again switches travel mode off: Keep Awake goes off and a Remote Control that
Notch started is stopped. Both toggles still work on their own. Notch cannot switch the
hotspot on for you; the reminder is only a reminder.

## Keep Awake

On: sets "when I close the lid: do nothing" for both plugged in and on battery (active
power plan) and holds the system and display awake. The previous values are saved to
`%APPDATA%\Notch\power-restore.toml` first.

Off (clicking the toggle): both sides leave "do nothing". Each goes back to its saved
value; a side that was already "do nothing" before Notch touched it goes to sleep, the
Windows default.

Once switched on it stays on: the choice is remembered (`keep_awake` in the config), the
lid setting is left in place when Notch quits, and it is applied again at every start.
While Notch is not running the lid setting still holds; only the extra "do not idle-sleep,
keep the display on" hold needs Notch running. Switch it off with the same toggle, which
puts the saved values back as described above.

The laptop's own screen still turns off when the lid is closed. This keeps the PC and any
external monitor running. (The panel no longer prints this note.)

This is not the same as the Claude desktop app's "Keep computer awake while Claude
works", which only stops idle sleep and still sleeps on lid close.

## Bench

```
notch.exe --bench            60 s, idle
notch.exe --bench 60 --demo  five demo sessions
notch.exe --bench 60 --expanded
```

One line per second and a summary go to `%LOCALAPPDATA%\Notch\logs\bench.log` (and stdout
if redirected). `cpu` is the share of the whole machine (what Task Manager shows), `cpu1`
the share of one core, `frames` / `render` the redraws in that second and their total time.

Test instance: set `NOTCH_INSTANCE=test` to run a second, isolated notch (own pipe, lock
and window title) next to the real one. Hook helpers started with the same variable talk
to that instance. This is how the stress bench below was run.

## Adding an adapter

1. Add `src/adapters/<name>.rs` with a type that implements `AgentAdapter`
   (`src/adapters/mod.rs`): `id`, `agent`, `enabled`, and either or both of
   - `on_push(payload, pids, wsl)`: translate one pushed JSON payload into `AgentEvent`s,
   - `start(cfg)`: spawn a thread that watches files and calls `app::post(UiMsg::Events(..))`
     (`watch::watch_dir` and `watch::Tailer` do the file work).
2. Add it to `REGISTRY` in `src/adapters/mod.rs` (replace its stub in `stubs.rs` if it has one).
3. If the agent can call a program on events, point it at
   `notch.exe --hook --agent <id>` (JSON on stdin).

Every event carries `session_id`, `agent`, `project_dir`, optional `title` and `pids`, and
one of: `SessionStarted`, `Working{step}`, `NeedsInput{reason}`, `Done`, `Error`,
`SessionEnded` (plus `Info` for title-only updates).

## Credits

- DM Sans (SIL Open Font License 1.1), embedded: `assets/DMSans-OFL.txt`.
- Agent logos from thesvg.org (`npx @thesvg/cli add ...`), in `assets/logos`. The marks
  belong to their owners and are used only to identify each agent.

## What is and is not verified

Measured on the development machine (Windows 11, 12 logical cores, 150% scaling).
`cpu` is the share of the whole machine, as Task Manager shows it.

| Case | CPU avg | CPU peak | Working set | Private working set |
|---|---|---|---|---|
| Idle, no sessions, 60 s (first milestone build) | 0.002% | 0.13% | 24.4 MB | 4.95 MB |
| 5 demo sessions, 3 dots pulsing, collapsed, 20 s | 0.22% | 0.78% | 28 MB peak, 12 MB after trim | 6.9 MB peak |
| 5 demo sessions, 3 dots pulsing, expanded, 20 s | 0.38% | 0.95% | 32 MB peak, 13 MB after trim | 10.1 MB peak |
| 5 sessions, about 80 hook events per second for 30 s, expanded | 1.85% | 4.23% | 41.1 MB peak | 14.0 MB peak |
| Same, collapsed | 1.68% | 4.51% | 40.0 MB peak | 13.5 MB peak |

The idle row predates the font, logos, usage and adapter code and has not been re-run on
the final build. About 12 seconds after start Notch hands unused startup memory back.

Hook helper (`notch.exe --hook`), timed from a Python harness whose own cost to start any
process is about 34 ms median on this machine; the figures are what the harness saw:

- island running: median 9.9 ms, p95 13.5 ms, max 24.8 ms (small payload, n=40);
  median 10.6 ms with a 200 KB payload
- island not running: median 9.4 ms, exit code 0

Also checked:

- Hook install then uninstall on copies of a real `~/.claude/settings.json` and a real
  `~/.codex/hooks.json`: byte-identical afterwards, existing hooks stayed first.
- Simulated hook events through the pipe: a permission prompt turns the session amber and
  opens the notch, Stop turns it green.
- Toast-click route in an isolated instance: `--activate` reached the running notch and it
  raised a window for the session. A second launch made the running one open.
- `--install` on this machine: exe copied, Desktop and Start Menu shortcuts written,
  Claude Code hooks re-pointed (existing hooks untouched), notch restarted from the
  installed copy.
- Restart recovers running sessions from transcripts; titles match the Claude app's.
- Usage percentages from the real endpoint after an expired sign-in was renewed.
- T3 Code's real database is readable (no active runs at the time).
- Live on this machine: both open desktop-app sessions listed with their real titles, and
  a session's two running subagents listed under it.
- 42 unit tests (`cargo test`), including: subagent events never change the main
  session's state, a turn ending with a question is "needs input", and a turn end read
  from the transcript rescues a missed Stop hook.

Known limits on live accuracy:

- Sessions that were already running when the hooks were installed do not have them until
  they are restarted. They are still listed, with the right state, from the registry and
  transcripts, but cannot be approved from the notch.
- The registry says `busy` both while the main turn runs and while only a background
  subagent runs. Notch tells these apart with the turn-end record (hook or transcript),
  so it depends on at least one of those being seen.
- After you approve in Claude (not in the notch), the row turns back to working when
  Claude Code updates its status, normally at once.

Not verified:

- A real toast has not been clicked.
- Approve / Deny has been tested end to end against a simulated blocking hook (the hook
  received exactly the allow / deny JSON Claude Code documents) but not yet on a real
  permission prompt: that needs a session started after this build's hooks were installed.
- Jump to session: Windows accepts the `claude://` link Notch opens; that the app then
  switches to a *different* session has not been watched.

- Travel mode: the reminder screen and the button have been rendered and captured, but
  the button has not been pressed (it changes power settings and opens a terminal).
- Keep Awake and Remote Control start/stop have not been pressed (they change system
  state). Only the state detection has run.
- Dragging between edges, the hotkey, toasts on screen, click-to-focus, multi-monitor and
  mixed-DPI behaviour have not been tested by hand.
- The T3 Code adapter has not seen a live run, and the Codex adapter has not seen a live
  Codex session (payloads follow the docs and unit tests).
- WSL hooks and transcript backfill are untested (no distro was running).
- Fullscreen auto-hide reacts to foreground changes; a window that goes fullscreen
  without a foreground change (F11) is noticed only at the next change.
- Gemini CLI, Cursor agent and OpenCode are stubs.
