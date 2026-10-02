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
- **Backups** (`x`, or `lazyssh export` / `restore`): snapshot every server, tag, pin, forward, and the launcher setting to a JSON file, and restore it on this or another machine. Restores merge new servers by default or replace the whole profile, and always save the current profile first so they can be undone. See [Backups](#backups).
- **Terminfo install** (`t`, or `lazyssh terminfo <name>`): fixes `clear`, `vim`, `less`, `tmux` breaking after `sudo su - other-user` on a server when using Kitty (or any terminal with its own `TERM`). See [Terminal type on servers](#terminal-type-on-servers-sudo-su---and-kitty).
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
lazyssh import [PATH]          import from ~/.ssh/config, or a LazySSH backup
lazyssh export [PATH|-]        back up everything (default: ~/.config/lazyssh/backups/)
lazyssh restore PATH [--replace]
                               merge a backup, or replace the whole profile
lazyssh terminfo <name> [--user]
                               install this terminal's terminfo on a server
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
t            install this terminal's terminfo on the server
x            backups: create / restore
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

## Backups

A backup is one JSON file holding the whole profile: every server with its tags, pins, jump host, forwards, and connection history, plus the launcher setting. It contains no secrets, only key *paths*, so the key files themselves still need your normal backup.

```bash
lazyssh export                       # ~/.config/lazyssh/backups/lazyssh-backup-<UTC time>.json
lazyssh export ~/sync/lazyssh.json   # anywhere you like
lazyssh export - | gpg -c > lazyssh.json.gpg   # or pipe it

lazyssh restore ~/sync/lazyssh.json            # merge: add servers whose name is new
lazyssh restore ~/sync/lazyssh.json --replace  # make the profile an exact copy
```

- **Merge** (the default) never overwrites: a server whose name already exists is kept as it is and reported.
- **Replace** swaps in the backup wholesale, launcher setting included.
- Before any restore, the current profile is written to `backups/lazyssh-before-restore-<time>.json`, so a wrong restore can be undone by restoring that file.
- A plain copy of an old `servers.json` restores too, and `lazyssh import` accepts backups as well as `~/.ssh/config`.

In the TUI, `x` lists the backups folder newest first: `Enter` on **New backup** creates one, `Enter` on a backup merges it, `R` replaces the profile with it.

## Terminal type on servers (`sudo su -` and Kitty)

Kitty sets `TERM=xterm-kitty`. A server only understands that name if it has the matching *terminfo* entry; without it, `clear`, `vim`, `less`, `htop`, and `tmux` fail or draw garbage (`'xterm-kitty': unknown terminal type`).

Kitty's `ssh` kitten fixes this by copying the entry into the login user's `~/.terminfo` for the session. That is why things work right after you connect, and break the moment you `sudo su - other-user`: the other user's home has no copy.

`t` (or `lazyssh terminfo <name>`) fixes it at the source by installing the entry on the server permanently:

- **All users** (default): compiles it into the system terminfo database with `sudo tic`, so every account (other users, root, tmux sessions, cron jobs) knows your terminal. sudo may ask for your password on the terminal; LazySSH never sees it.
- **Just me** (`--user`): installs into the login user's `~/.terminfo`, for hosts where you have no sudo. This doesn't help after `sudo su -`.

Your `TERM` is never changed or forced; the server simply learns what your terminal is. The entry comes from your local `infocmp`, travels base64-encoded over ssh, and is compiled by the server's own `tic`. It is safe to run again, and works for any terminal (WezTerm, foot, Alacritty, Ghostty…), not just Kitty. The server needs `tic`, which ships in `ncurses-bin` on Debian/Ubuntu and `ncurses` elsewhere.

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
