# lazyssh

A tiny Rust TUI for remembering SSH targets so you do not have to keep opening your SSH config just to remember which box is which.

It stores only connection metadata and SSH key paths. It does not store passwords, private key contents, passphrases, or API keys.

## Features

- Centered dashboard TUI with a large hollow neon LAZYSSH wordmark (green-to-cyan gradient outline) and tagline on wide terminals.
- Bordered command bar footer with key badges, and a status line for feedback.
- Blinking add-form cursor without busy-looping.
- Responsive layout: full wordmark and server card on wide terminals, compact single-column fallback on narrow terminals.
- List saved SSH servers by name in a centered card with airy rows, a full-width selection bar, and a status dot per row.
- Add a server from the TUI.
- Delete a server.
- Connect to a selected server by launching `ssh`.
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
a            add server
e            edit selected server (coming soon)
d            delete selected server
Enter        connect to selected server
q / Esc      quit
```

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
  ]
}
```

Before connecting, manually authorize the matching public key on the remote server, usually by adding it to:

```text
~/.ssh/authorized_keys
```
