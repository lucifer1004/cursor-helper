# cursor-helper

<p align="center">
  <a href="https://crates.io/crates/cursor-helper"><img src="https://img.shields.io/crates/v/cursor-helper.svg" alt="crates.io"></a>
  <a href="https://github.com/lucifer1004/cursor-helper/actions/workflows/ci.yml"><img src="https://github.com/lucifer1004/cursor-helper/actions/workflows/ci.yml/badge.svg" alt="CI"></a>
  <a href="https://opensource.org/licenses/MIT"><img src="https://img.shields.io/badge/License-MIT-yellow.svg" alt="License: MIT"></a>
  <a href="https://github.com/govctl-org/govctl"><img src="https://img.shields.io/badge/governed%20by-govctl-6366F1" alt="governed by govctl"></a>
</p>

**Stop losing your Cursor chat history.** This CLI fixes the things Cursor doesn't expose in the UI.

## The Problem

When you rename or move a project folder, Cursor loses all your chat history. Weeks of conversations, context, and problem-solving — gone.

## The Solution

```bash
cursor-helper rename /old/project /new/project
```

Your chat history, workspace settings, and MCP cache stay intact.

## Installation

```bash
# From crates.io (recommended)
cargo install cursor-helper

# Or from source
git clone https://github.com/lucifer1004/cursor-helper
cd cursor-helper
cargo install --path .
```

Requires Rust 1.70+. Works on macOS, Linux, and Windows.

Pre-built binaries for major platforms are available on the [Releases](https://github.com/lucifer1004/cursor-helper/releases) page.

## Key Commands

### `rename` — Move Projects Without Losing History

```bash
# Move/rename a project
cursor-helper rename /path/to/old-project /path/to/new-project

# Copy instead of move
cursor-helper rename --copy /path/to/project /path/to/project-copy

# Preview changes first
cursor-helper rename -n /path/to/old /path/to/new
```

### `export-chat` — Export Everything Cursor Hides

Cursor's built-in export omits thinking blocks and tool calls. This doesn't.

```bash
# Full export with thinking, tools, and token counts
cursor-helper export-chat /path/to/project -v

# Just the conversations
cursor-helper export-chat /path/to/project

# Export to JSON
cursor-helper export-chat /path/to/project --format json -o export.json

# Remote sessions (SSH, tunnels, WSL, dev containers)
cursor-helper export-chat /home/user/project        # By remote path
cursor-helper export-chat --workspace-id abc123def  # By workspace ID (from 'list')
```

| Flag                 | What it adds                                   |
| -------------------- | ---------------------------------------------- |
| `--with-thinking`    | AI reasoning/thinking blocks with duration     |
| `--with-tools`       | Tool calls (file reads, edits, shell commands) |
| `--with-stats`       | Model name and token counts                    |
| `-v`                 | All of the above                               |
| `--include-archived` | Include archived sessions                      |
| `--workspace-id`     | Export by workspace ID (for remote sessions)   |

### `list` — See All Your Projects

```bash
# List all projects (most recent first)
cursor-helper list

# Show workspace IDs (for use with export-chat --workspace-id)
cursor-helper list --with-id

# Sort by chat count
cursor-helper list --sort chats --limit 10

# Filter by type
cursor-helper list --filter remote   # SSH/tunnel projects
cursor-helper list --filter local    # Local projects
```

### `clean` — Reclaim Disk Space

Remove workspace data for deleted projects.

```bash
cursor-helper clean --dry-run  # Preview
cursor-helper clean --yes      # Delete without confirmation
```

## Other Commands

| Command   | Description                                       |
| --------- | ------------------------------------------------- |
| `stats`   | Show chat count and storage size for a project    |
| `backup`  | Create a portable backup of project metadata      |
| `restore` | Restore metadata to a new location                |
| `clone`   | Duplicate a project with independent chat history |

## How It Works

Cursor stores metadata in platform-specific locations:

| Platform | Location                                                      |
| -------- | ------------------------------------------------------------- |
| macOS    | `~/Library/Application Support/Cursor/User/workspaceStorage/` |
| Linux    | `~/.config/Cursor/User/workspaceStorage/`                     |
| Windows  | `%APPDATA%\Cursor\User\workspaceStorage\`                     |

Each project has a unique ID derived from its path. When you rename a folder, the ID changes and Cursor can't find the old data. This tool updates the necessary references.

**Note:** Linux support is experimental. Workspace hash computation depends on filesystem birthtime support, which varies by filesystem and kernel version.

## Disclaimer

This tool is **not affiliated with or endorsed by Anysphere, Inc.** (Cursor).

It reads your local data files for personal backup and portability. It does not access Cursor's servers, APIs, or source code. See [DISCLAIMER.md](DISCLAIMER.md) for details.

## License

MIT — see [LICENSE](LICENSE).

## Related

- [cursor-chat-export](https://github.com/somogyijanos/cursor-chat-export)
- [cursor-view](https://github.com/saharmor/cursor-view)
