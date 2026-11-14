# Notch

A tiny always-on-top notch for Windows 10/11 that shows your AI coding agent sessions
live. It sits flush against a screen edge like the MacBook notch, opens when you hover
it, tells you when a session finishes or needs input, shows your usage limits, and has
two one-click toggles: Claude Code Remote Control and Keep Awake (lid closed).

Native Rust, Direct2D/DirectWrite, one exe (about 0.8 MB), no installer, no runtime.

> Status: work in progress. See "What is and is not verified" at the end.

## Run it

```
cargo build --release
target\release\notch.exe
```

On first start it asks whether to install the Claude Code hooks (see below).

- **Hover** the notch to open it; move away to close it. **Click** it to pin it open.
- **Drag** it to any screen edge: top, bottom, left or right, anywhere along the edge.
  It attaches when its centre comes within 56 px of an edge. On the bottom edge it sits
  on top of the taskbar and opens upward. Dropped away from the edges it floats as a pill.
  The position is remembered per monitor setup.
- **Ctrl+/** hides and shows it. Change the keys in Settings (click the row, press the
  new combination; Backspace clears it, Esc cancels).
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

## Claude Code hooks

Hooks are what make sessions update live (permission prompts, finished turns).

Install: answer Yes on first start, or Settings > "Claude Code hooks", or
`notch.exe --install-hooks`.

What it does to `~/.claude/settings.json`:

- Saves a copy first as `settings.json.notch-backup` (only the first time, so the backup
  is always the file as it was before Notch).
- Appends one entry per event after your existing hooks. Nothing of yours is changed,
  removed or reordered. Events: SessionStart, UserPromptSubmit, PreToolUse, PostToolUse,
  PostToolUseFailure, PermissionRequest, Notification, Stop, StopFailure, SubagentStop,
  PreCompact, SessionEnd.
- Each entry runs `notch.exe --hook` in exec form with `"async": true`, so Claude Code
  never waits for it. The helper reads the hook JSON from stdin, forwards it over
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

## Usage limits

- Source A: `GET https://api.anthropic.com/api/oauth/usage` with the OAuth token Claude
  Code keeps in `~/.claude/.credentials.json`. The file is only read, the token is sent
  only to api.anthropic.com and is never logged or shown. The endpoint is undocumented.
  If the token has expired (Claude Code refreshes it when it runs), is rejected, or the
  endpoint rate-limits, Notch falls back to source B.
- Source B: a local estimate from the transcripts, like ccusage: tokens in the current
  5-hour block and in the last 7 days (input + output + cache writes). It shows tokens,
  not a percentage, unless you set `estimate_limit_5h_tokens` /
  `estimate_limit_week_tokens` under `[usage]`. The first scan reads a week of
  transcripts once; after that only appended bytes are read (cache:
  `%LOCALAPPDATA%\Notch\usage-cache.tsv`).
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

## Keep Awake

Sets "when I close the lid: do nothing" on battery and mains for the active power plan and
holds the system and display awake. The previous lid values are saved to
`%APPDATA%\Notch\power-restore.toml` first and restored when you switch it off, when
Notch quits, or on the next start after a crash.

The laptop's own screen still turns off when the lid is closed. This keeps the PC and any
external monitor running.

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

Measured or tested on the development machine (Windows 11, 150% scaling):

- Idle, no sessions, 60 s bench: 0.002% average CPU, 4.95 MB private working set
  (24.4 MB working set including shared DLL pages). Measured before the font, logos and
  usage code were added; with those, about 10 MB private / 32 MB working set.
- Hook install then uninstall on a copy of a real `settings.json`: byte-identical, and an
  existing PreToolUse hook stayed first.
- Simulated hook events through the pipe: permission prompt turns the session amber and
  opens the notch, Stop turns it green.
- Restart recovers running sessions from transcripts.
- 23 unit tests (`cargo test`).

Not yet verified or not built:

- T3 Code and Codex adapters are stubs.
- Keep Awake and Remote Control start/stop have not been exercised end to end (they
  change system state); only the state detection has been run.
- Dragging between edges, the hotkey, toasts on screen, click-to-focus, multi-monitor
  and mixed-DPI behaviour have not been tested by hand.
- Hook latency has not been measured properly.
- With sessions pulsing, redraws cost about 1 to 2% of the machine; this has not been
  tuned, and the 5-session acceptance bench has not been run on the current build.
- Fullscreen auto-hide reacts to foreground changes; a window that goes fullscreen
  without a foreground change (F11) is noticed only at the next change.
