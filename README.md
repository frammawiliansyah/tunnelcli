# tunnelcli

[![License: MIT](https://img.shields.io/badge/License-MIT-yellow.svg)](LICENSE)

A tiny, zero-dependency CLI for one-shot SSH local port forwarding, built as a
lighter alternative to editing `~/.ssh/config` by hand or wiring up a
GUI/daemon tunnel manager for a single "forward this port, please" need. The
project is called `tunnelcli`; the command it installs is `tunnel`.

```
$ tunnel myserver 5173
tunnel: 5173 (local) -> myserver:5173 (remote) -- Ctrl+C untuk berhenti
```

## Why

Editors and IDEs with SSH remote development (e.g. Zed) often require you to
declare every forwarded port ahead of time in a settings file and restart the
whole application to pick up a new one. That's fine for one or two ports you
know about up front, but painful once you're regularly spinning up new
services on a remote box and just want `localhost:PORT` to work immediately.

`tunnel` is a single command that:

- Forwards **local:PORT → host:PORT** over SSH, using host aliases you already
  have in `~/.ssh/config` - no separate config file to maintain.
- **Refuses to start** if the local port is already taken, and tells you
  exactly what's holding it (PID, process name, full command) instead of
  failing with an opaque bind error.
- Comes with `tunnel -kill PORT` to free up a local port without hunting for
  the PID yourself first.
- Collapses repeated identical SSH error lines (e.g. a flood of `channel N:
  open failed: connect failed: Connection refused` while the remote service
  isn't up yet) into a single line instead of spamming your terminal.
- Has **zero external dependencies** - it's a thin wrapper around `ssh`,
  `lsof`, `ps`, and `kill`, so there's nothing to audit beyond the ~250 lines
  in `src/main.rs`.

## Non-goals

- **No cross-port mapping.** `tunnel host 3000` always maps local `3000` to
  remote `3000`. This is deliberate, not a missing feature - it keeps the
  command trivially predictable (`tunnel <host> <port>`, nothing more to get
  wrong) and matches how most dev workflows already think about ports (the
  service listens on 3000 everywhere, dev machine included).
- **No daemon / background service management.** `tunnel` holds the
  foreground for as long as the tunnel is open, same as running `ssh`
  directly. If you want many tunnels running unattended, run several
  instances (in separate terminal tabs, a multiplexer, etc.) rather than
  expecting this tool to supervise them for you.
- **No bundled SSH config management.** `tunnel` reads your existing
  `~/.ssh/config` to sanity-check the host alias you pass; it never writes to
  it.

## Requirements

- `ssh`, `lsof`, `ps`, `kill` on `PATH`.
- Rust toolchain to build from source (no published binary yet).

## OS support

| OS | Status |
| --- | --- |
| macOS | **Tested.** This is the primary development environment. |
| Linux | **Likely works, not verified.** Same external tools (`ssh`, `lsof`, `kill`) and the same `ps -o command=` invocation are supported by both BSD (macOS) and GNU (Linux) `ps`/`lsof`, and the signal handling described below is standard POSIX. Nothing in the code is macOS-specific, but it hasn't actually been run on Linux yet. |
| Windows (native, cmd/PowerShell) | **Not supported.** The signal-forwarding logic (see [How it works](#how-it-works)) links directly against the POSIX `signal(2)`/`kill(2)` C functions and hardcodes `SIGINT = 2` / `SIGTERM = 15`, which don't exist the same way on native Windows. It also shells out to `lsof` and reads `~/.ssh/config` via the `HOME` environment variable, none of which are native Windows concepts. |
| WSL / Git Bash / Cygwin | Should work the same as Linux/macOS, since these provide a real POSIX-like environment (WSL in particular *is* Linux) - not independently verified either. |

In short: this is a Unix tool. If you're on Windows, run it inside WSL rather
than expecting it to work outside of a POSIX environment.

## Installation

```bash
git clone git@github.com:<owner>/tunnelcli.git
cd tunnelcli
cargo build --release
cp target/release/tunnel ~/.local/bin/tunnel   # or anywhere on your PATH
```

## Usage

### Forward a port

```bash
tunnel <ssh-host-alias> <port>
```

`<ssh-host-alias>` must be a `Host` entry from `~/.ssh/config` (or anything
`ssh` itself can resolve - `tunnel` only prints a warning, never blocks, if it
can't find the alias in your config, since the file may rely on `Include` or
pattern matching it doesn't parse).

```bash
$ tunnel myserver 5173
tunnel: 5173 (local) -> myserver:5173 (remote) -- Ctrl+C untuk berhenti
```

Press `Ctrl+C` to stop. If the local port is already in use, `tunnel` exits
immediately with a report instead of starting anything:

```bash
$ tunnel myserver 8901
error: port 8901 sudah dipakai di local, tunnel dibatalkan.

  PID     : 12846
  Process : Python
  User    : someuser
  Command : /opt/homebrew/.../Python -m http.server 8901

Kill manual kalau memang mau lanjut:
  kill 12846
```

### Free up a local port

```bash
$ tunnel -kill 8901
killing PID 12846 (Python, user someuser) - /opt/homebrew/.../Python -m http.server 8901 ... ok
```

Kills every process found listening on that port locally (there's usually
just one, but a port can have more than one listener in rare setups).

## How it works

`tunnel host port` runs, in order:

1. **Host sanity check** - reads `~/.ssh/config`, looks for a `Host` line
   listing the given alias, and prints a non-fatal warning if it's not found
   directly (it might still resolve via `Include` or a wildcard pattern
   `tunnel` doesn't parse).
2. **Port availability check** - runs `lsof -nP -iTCP:<port> -sTCP:LISTEN` and
   bails out *before* touching `ssh` at all if anything is already listening,
   reporting the PID/process/command via `ps -p <pid> -o command=`.
3. **Spawn `ssh -N -L port:localhost:port host`** as a supervised child
   process (not `exec()`'d into), piping its stderr through a small dedup
   filter that collapses consecutive identical lines.
4. **Signal handling** - `SIGINT`/`SIGTERM` are caught explicitly and
   forwarded to the `ssh` child before `tunnel` exits. This matters because a
   real terminal `Ctrl+C` signals the whole foreground process group (which
   would clean up the child anyway), but a plain `kill <tunnel-pid>` only
   signals `tunnel` itself - without explicit forwarding, `ssh` would be
   orphaned and keep holding the port.

`tunnel -kill port` skips steps 1/3/4 entirely: it just runs the same `lsof`
lookup and sends `kill <pid>` to everything it finds.

## Known limitations

- **`~/.ssh/config` parsing is intentionally simple.** It only matches
  literal `Host` tokens on a single line; it does not follow `Include`
  directives or evaluate wildcard patterns. This only affects the advisory
  warning, never whether the tunnel actually works (that's entirely up to
  `ssh` itself).
- **No final "repeated N times" tally on Ctrl+C/kill.** The dedup summary for
  a run of identical stderr lines is only flushed when a *different* line
  appears or the process exits on its own; the signal handler prioritizes
  killing the `ssh` child immediately over flushing pending output. In
  practice this means you'll see a repeated error line once and then
  silence, without ever finding out exactly how many times it repeated if you
  stop the tunnel yourself.
- **Same-port-only by design** - see [Non-goals](#non-goals) if you need
  cross-port mapping; this tool intentionally doesn't support it.

## Contributing

This started as a personal tool, so it's intentionally minimal. Issues and
PRs are welcome - please keep changes to the "does one thing well" spirit of
the tool (see [Non-goals](#non-goals)) rather than growing it into a general
SSH tunnel manager.

## License

[MIT](LICENSE)
