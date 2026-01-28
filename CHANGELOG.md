# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Fixed

- Strip Windows extended-length path prefix in export-chat (WI-2026-01-28-003)
- Case-insensitive URI comparison for Windows paths (WI-2026-01-28-003)

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
