# lazyssh

A tiny Rust TUI for remembering SSH targets so you do not have to keep opening your SSH config just to remember which box is which.

It stores only connection metadata and SSH key paths. It does not store passwords, private key contents, passphrases, or API keys.

## Features

- Centered dashboard TUI with a large hollow neon LAZYSSH wordmark (green-to-cyan gradient outline) and tagline on wide terminals.
- Responsive layout: full wordmark and server card on wide terminals, compact single-column fallback on narrow terminals.
- Fixed-height, scrollable server card with overflow markers, a position counter, and a description inspector showing `user@host:port`, jump host, and tags.
- **Reachability dots**: each server's SSH port is probed in the background (TCP only, 1.5 s timeout, never blocks the UI). Green = port open, orange = unreachable, hollow = checking, blue ◆ = behind a jump host (not probed directly). `r` re-checks.
- **Tags and pins**: tag servers (`homelab`, `prod`, …) and search with `/#prod`; pin favourites with `p` so they stay above the recency-ranked rest (★ icon).
- Vim-style `/` search: every word must match name, host, description, or a tag; `#tag` matches tags by prefix, a bare `#` shows tagged servers.
- **Jump hosts**: a `Jump host` field that takes either the name of another saved server (resolved at connect time, chains like `edge → bastion → db` included, loops rejected) or a literal `user@host:port`.
- **Import from `~/.ssh/config`** (`i`, or `lazyssh import`): concrete `Host` aliases with their HostName, User, Port, IdentityFile, and ProxyJump. Wildcard and `Match` blocks are skipped; existing names are never overwritten. The file is only ever read.
- **Duplicate** (`D`) a server into a prefilled add form; **copy** (`y`) its exact ssh command line to the clipboard (`wl-copy`, `xclip`/`xsel`, `clip.exe`, else OSC 52 through the terminal).
- **Open as SSH, SFTP, or Mosh** (`o`). Mosh tunnels its bootstrap through ssh with the same key, port, jump host, and options.
- **Saved port forwards** (`f`): store `L8080:localhost:80`, `R9000:localhost:9000`, or `D1080` per server and start/stop them from the TUI. They run as background `ssh -N` processes, are shown as `⇄n` on the row, and are all stopped when LazySSH exits. While forwards are running, connecting to a server returns you to LazySSH afterwards instead of replacing it.
- **Command-line mode** for scripts and muscle memory (see below).
- `?` opens a full key reference.
- Connect through native OpenSSH or Kitty's SSH kitten, selected safely and configured from the TUI.
- Add, edit, delete, and bootstrap (install your public key on) servers from the TUI.
- Persist entries at:

```text
~/.config/lazyssh/servers.json
```

Older config files load unchanged; new fields are only written when set.

## Build

```bash
cargo build --release
```

The binary will be at:

```text
target/release/lazyssh
```

Optional local install:

```bash
cargo install --path .
```

## Run

```bash
cargo run
# or, after install:
lazyssh
```

## Command line

```text
lazyssh                        open the TUI
lazyssh <name> [--sftp|--mosh] connect by name (exact, else unique prefix)
lazyssh connect <name> [...]   same, for servers named like a subcommand
lazyssh ls [--tag <tag>]       tab-separated: name, user@host:port, tags
lazyssh cmd <name>             print the ssh command line for a server
lazyssh import [PATH]          import new hosts from ~/.ssh/config (or PATH)
lazyssh --help | --version
```

`lazyssh ls` keeps the TUI's pinned-then-recent order, so it pairs well with fzf:

```bash
lazyssh "$(lazyssh ls | fzf | cut -f1)"
```

## Keys

Main screen (press `?` in the app for the same list):

```text
j / Down     move down
k / Up       move up
/            search: words, #tag, # = any tag (live filter)
Enter        SSH to selected server
o            open as SSH / SFTP / Mosh
f            start/stop the server's saved port forwards
y            copy the ssh command line
a            add server
e            edit selected server
D            duplicate selected server
d            delete selected server
p            pin / unpin
i            import from ~/.ssh/config
b            bootstrap a new server (install your public key)
r            re-check reachability
s            SSH launcher settings
?            key reference
Esc          clear the active filter, or quit when none is active
q            quit
```

Search mode (after `/`):

```text
type         filter the list as you type
Backspace    erase
Enter        keep the filter and return to the list
Esc          clear the filter and return to the list
```

The server card keeps a fixed height of up to 10 rows; longer lists scroll,
with `▲ n more` / `▼ n more` markers and a `current/total` counter.

Import dialog: `Space` toggles a host, `A` toggles all, `Enter` imports, `Esc` cancels. Hosts whose name is already saved are shown greyed out and skipped.

Port forwards dialog: `j`/`k` move, `Enter` starts or stops the highlighted forward, `Esc` closes. Forwards run with `BatchMode=yes` (no password prompts — use a key or agent) and `ExitOnForwardFailure=yes`, so a busy local port is reported in the status bar instead of failing silently.

SSH launcher settings:

```text
j / Down         next launcher
k / Up           previous launcher
Enter            save selection
Esc              cancel without saving
```

## SSH launcher modes

Open the settings dialog with `s`. LazySSH persists the selected mode alongside the server list, so no manual configuration-file editing is required.

- **Auto** (default): when `TERM=xterm-kitty` and `kitten` is available, LazySSH launches `kitten ssh`. If only the `kitty` executable is available, it uses `kitty +kitten ssh`. When `TERM` is unavailable, a Kitty window ID can provide the same signal. An explicit non-Kitty `TERM`—including `tmux-*`—safely falls back to native `ssh`; users with correctly configured multiplexer passthrough can force Kitty mode.
- **OpenSSH**: always launch native `ssh`.
- **Kitty**: always use Kitty's SSH kitten. If neither `kitten` nor `kitty` is available, LazySSH reports the configuration error instead of silently falling back.

Kitty advertises `TERM=xterm-kitty`. Servers without that terminfo entry can make commands such as `clear`, `vim`, `less`, or `tmux` misbehave. Kitty's SSH kitten transfers the required terminal information for the session, avoiding manual terminfo installation across every remote server. LazySSH never rewrites `TERM`, and SSH key bootstrap continues to use native OpenSSH.

Add server popup:

```text
Enter / Tab      next field
Shift+Tab        previous field
Ctrl+s           save
Esc              cancel
```

## Server fields

- Name: short display name, e.g. `node-2`.
- Description: what this server is for.
- Host / IP: SSH hostname or address.
- Username: optional. If blank, SSH uses your local username or SSH config.
- SSH key path: optional. If set, lazyssh runs `ssh -i <key> <target>`.
- Jump host: optional. A saved server's name, or a literal `[user@]host[:port]`; passed as `ssh -J`.
- Extra ssh args: optional, split on whitespace and passed to `ssh` verbatim.
- Tags: optional, space or comma separated.
- Forwards: optional, comma separated `L…`, `R…`, or `D…` specs.

Example saved entry:

```json
{
  "servers": [
    {
      "name": "node-2",
      "description": "Docker host for self-hosted services",
      "host": "node-2.ts.net",
      "username": "sam",
      "identity_file": "/home/viper/.ssh/id_ed25519",
      "tags": ["homelab", "docker"],
      "pinned": true,
      "forwards": ["L3000:localhost:3000"]
    },
    {
      "name": "db",
      "description": "Postgres, only reachable from node-2",
      "host": "10.0.0.5",
      "username": "postgres",
      "identity_file": null,
      "jump_host": "node-2"
    }
  ],
  "launcher": "auto"
}
```

Before connecting, manually authorize the matching public key on the remote server, usually by adding it to:

```text
~/.ssh/authorized_keys
```

## Releases

Pushing a tag that matches the `Cargo.toml` version (for example `v0.2.0`) runs `.github/workflows/release.yml`: tests, Linux + Windows builds, packaging, `SHA256SUMS`, and a GitHub release with generated notes. Every push and pull request runs fmt, clippy, tests, and a Windows build through `.github/workflows/ci.yml`.
