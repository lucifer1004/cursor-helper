# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.1.0] - 2026-01-19

### Added

- `rename` command: Rename/move projects while preserving chat history
- `list` command: List all Cursor projects with sorting and filtering
- `stats` command: Show usage statistics for a project
- `export-chat` command: Export chat history to Markdown or JSON
  - Includes thinking blocks, tool calls, and token counts
  - `--include-archived` flag for archived sessions
- `clean` command: Remove orphaned workspace storage
- `backup` command: Backup Cursor metadata for a project
- `restore` command: Restore metadata from a backup
- `clone` command: Clone a project with full chat history

### Notes

- Linux support is experimental; workspace hash computation may not match Cursor's
  internal hash on some filesystems. The tool will search for existing workspaces
  as a fallback.
