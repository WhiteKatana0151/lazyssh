# lazyssh

A tiny Rust TUI for remembering SSH targets so you do not have to keep opening your SSH config just to remember which box is which.

It stores only connection metadata and SSH key paths. It does not store passwords, private key contents, passphrases, or API keys.

## Features

- Centered dashboard TUI with a large hollow neon LAZYSSH wordmark (green-to-cyan gradient outline) and tagline on wide terminals.
- Bordered command bar footer with key badges, and a status line for feedback.
- Blinking add-form cursor without busy-looping.
- Responsive layout: full wordmark and server card on wide terminals, compact single-column fallback on narrow terminals.
- List saved SSH servers by name in a centered card with airy rows, a full-width selection bar, and a status dot per row.
- Fixed-height, scrollable server card with overflow markers and a position counter.
- Vim-style `/` search that live-filters by name, host, or description.
- Add, edit, and delete servers from the TUI.
- Connect through native OpenSSH or Kitty's SSH kitten, selected safely and configured from the TUI.
- Persist entries at:

```text
~/.config/lazyssh/servers.json
```

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

## Keys

Main screen:

```text
j / Down     move down
k / Up       move up
/            search by name, host, or description (live filter)
a            add server
e            edit selected server
d            delete selected server
b            bootstrap a new server (install your public key)
s            SSH launcher settings
Enter        connect to selected server
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

Example saved entry:

```json
{
  "servers": [
    {
      "name": "node-2",
      "description": "Docker host for self-hosted services",
      "host": "node-2.ts.net",
      "username": "sam",
      "identity_file": "/home/viper/.ssh/id_ed25519"
    }
  ],
  "launcher": "auto"
}
```

Before connecting, manually authorize the matching public key on the remote server, usually by adding it to:

```text
~/.ssh/authorized_keys
```
