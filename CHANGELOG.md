# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.2.2] - 2026-03-01

### Added

- Add `--force-index` option for `rename --copy` to force composer index rebuild (WI-2026-03-01-001)

### Fixed

- Fix `rename --copy` to preserve full chat history visibility by synchronizing `composer.composerData` and normalizing workspace references in the copied `workspaceStorage` DB (WI-2026-03-01-001)
- Extend `rename` copy flow to copy the full `workspaceStorage/<hash>/` directory, update both `storage.json` and `globalStorage/state.vscdb`, and clear stale Electron cache directories to prevent UI desync (WI-2026-03-01-001)

## [0.2.1] - 2026-01-28

### Added

- `--split` flag for exporting sessions to separate files (WI-2026-01-28-004)
- `--exclude-blank` flag to filter empty sessions (WI-2026-01-28-004)

### Fixed

- Strip Windows extended-length path prefix in export-chat (WI-2026-01-28-003)
- Case-insensitive URI comparison for Windows paths (WI-2026-01-28-003)
- `count_chat_sessions` now counts Composer sessions (consistent with export) (WI-2026-01-28-004)
- Strip Windows path prefix in stats command (WI-2026-01-28-005)

## [0.2.0] - 2026-01-28

### Added

- Export chat from remote sessions by path matching (WI-2026-01-28-001)
- `--workspace-id` flag for direct workspace lookup (WI-2026-01-28-001)

### Changed

- Rename `--with-hash` to `--with-id` for consistency (WI-2026-01-28-002)

## [0.1.1] - 2026-01-23

### Fixed

- list command runs without error for multi-root workspaces (WI-2026-01-23-001)
- Workspace hash matches Cursor's stored hash on Windows (WI-2026-01-23-002)

## [0.1.0] - 2026-01-19

### Added

- Create RFC-0000 meta-RFC defining project vision (WI-2026-01-19-001)
- Define scope and goals of cursor-helper CLI (WI-2026-01-19-001)
- Document implemented and planned commands (WI-2026-01-19-001)
- Implement cursor-helper list command (WI-2026-01-19-002)
- Implement cursor-helper stats command (WI-2026-01-19-002)
- Implement cursor-helper export-chat command (WI-2026-01-19-002)
- Implement cursor-helper clean command (WI-2026-01-19-002)
- Implement cursor-helper backup command (WI-2026-01-19-002)
- Implement cursor-helper restore command (WI-2026-01-19-002)
- Implement cursor-helper clone command (WI-2026-01-19-002)
- List command supports sorting options (--sort) (WI-2026-01-19-002)
- List command supports filtering options (--filter) (WI-2026-01-19-002)

### Fixed

- Chat count column displays actual counts instead of dashes (WI-2026-01-19-002)
- Pre-publish issues fixed (WI-2026-01-19-003)
