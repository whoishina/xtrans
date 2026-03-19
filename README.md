# xtrans

SSH wrapper that forwards clipboard images from your local machine to a remote server. Built for using [Claude Code](https://docs.anthropic.com/en/docs/claude-code) over SSH — paste screenshots directly into a remote CLI session.

## The Problem

When running Claude Code on a remote server via SSH, pressing Ctrl+V pastes from the **remote** clipboard, which doesn't have your local screenshots. There's no built-in way to send images from your local machine into the remote terminal session.

## How It Works

```
xtrans ssh user@host
```

This wraps `ssh` transparently. Everything works exactly like a normal SSH session, except:

- **Ctrl+V with an image** in your clipboard: the image is uploaded to `/tmp/` on the remote via the existing SSH connection, and the file path is typed into the terminal. Claude Code automatically recognizes file paths as attachments.
- **Ctrl+V with text**: standard bracketed paste (same as normal terminal paste).
- **Ctrl+V with empty clipboard**: the raw Ctrl+V byte is forwarded as usual.

All other input, output, terminal resizing, and SSH features work unchanged.

## Installation

### From source

Requires [Rust](https://rustup.rs/) (edition 2024).

```bash
git clone https://github.com/user/xtrans.git
cd xtrans
cargo build --release
# Binary at target/release/xtrans
```

**Linux** additionally requires `libutil-dev` (or equivalent) for PTY support:

```bash
# Debian/Ubuntu
sudo apt install libutil-linux-dev

# Fedora/RHEL
sudo dnf install util-linux-devel
```

**macOS** and **Windows** need no extra system libraries.

### Cross-compilation

```bash
# Linux target from macOS
rustup target add x86_64-unknown-linux-gnu
cargo build --release --target x86_64-unknown-linux-gnu

# Windows target
rustup target add x86_64-pc-windows-msvc
cargo build --release --target x86_64-pc-windows-msvc
```

## Usage

```bash
# Basic
xtrans ssh user@10.0.0.1

# With SSH options (all options are passed through)
xtrans ssh -p 2222 -i ~/.ssh/id_rsa user@host

# With verbose SSH output
xtrans ssh -v user@host

# Using SSH config alias
xtrans ssh myserver
```

Inside the session, press **Ctrl+V** to paste. If your clipboard has a screenshot, it's uploaded and the path appears in the terminal. Claude Code picks it up as an image attachment.

> **Note:** On macOS/Linux, Ctrl+V (byte `0x16`) is distinct from Cmd+V/middle-click paste. Your terminal emulator handles Cmd+V; xtrans intercepts the raw Ctrl+V keystroke.

## Architecture

```
Local machine                          Remote server
┌──────────────────┐                   ┌──────────────┐
│  Terminal (iTerm, │                   │              │
│  Terminal.app,    │    SSH + PTY      │  bash/zsh    │
│  Windows Terminal)├──────────────────►│  claude      │
│                   │                   │              │
│  xtrans           │                   │  /tmp/*.png  │
│  ├─ raw mode      │  ControlMaster   │  (uploaded   │
│  ├─ PTY proxy ────┼──(multiplexed)──►│   images)    │
│  ├─ Ctrl+V detect │                   │              │
│  └─ clipboard read│                   │              │
└──────────────────┘                   └──────────────┘
```

### Platform-specific implementation

| | macOS / Linux | Windows |
|---|---|---|
| SSH stdin | PTY pair (`openpty`) | Piped stdin |
| Raw mode | POSIX termios (`cfmakeraw`) | Win32 Console API (`SetConsoleMode`) |
| Terminal size | `ioctl(TIOCGWINSZ)` passed to PTY | SSH reads from console stdout |
| Resize handling | SIGWINCH signal handler | SSH handles via Console API |
| Native library | `libc` | `windows-sys` |

### How image upload works

1. Ctrl+V detected in the input byte stream (byte `0x16`)
2. Local clipboard is read via native APIs (NSPasteboard / Win32 / X11)
3. Image data is encoded as PNG
4. A new SSH channel is opened through the existing connection using [ControlMaster multiplexing](https://man.openbsd.org/ssh_config#ControlMaster) — no extra authentication required
5. PNG data is piped to `cat > /tmp/xtrans-<timestamp>.png` on the remote
6. The remote file path is typed into the SSH stdin

## Dependencies

| Crate | Purpose |
|---|---|
| [clap](https://crates.io/crates/clap) | CLI argument parsing |
| [arboard](https://crates.io/crates/arboard) | Cross-platform clipboard access (text + image) |
| [image](https://crates.io/crates/image) | PNG encoding |
| [libc](https://crates.io/crates/libc) | POSIX syscalls — PTY, termios, signals (Unix only) |
| [windows-sys](https://crates.io/crates/windows-sys) | Win32 Console API (Windows only) |

## Limitations

- **Image cleanup:** Uploaded images persist in `/tmp/` on the remote server. Clean up manually or with a cron job.
- **Windows Ctrl+V:** Some Windows terminal emulators consume Ctrl+V for their own paste handling before the application sees it. Windows Terminal can be configured to change this binding.
- **ControlMaster:** SSH multiplexing is required for fast image uploads. If unavailable, the upload falls back to a new SSH connection (requires re-authentication unless key-based auth is configured).
- **SSH client:** Requires OpenSSH. Other SSH clients (PuTTY, etc.) are not supported.

## License

MIT
