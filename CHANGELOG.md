# Changelog

Notable user-visible changes will be recorded in this file. FocalDesk is alpha
software and does not yet promise a stable API or configuration format.

The format is based on Keep a Changelog, and future releases should use semantic
version numbers where practical.

## Unreleased

### Added

- Project status matrix and annotated architecture documentation.
- Build, configuration, keybinding, troubleshooting, and roadmap documentation.
- Contribution and issue-reporting templates.

### Changed

- Clarified experimental and planned feature claims, including subsurface
  damage tracking, HDR, capture, AI, and automation.
- Provisioned the pinned native Vosk library in CI so workspace tests can link
  voice-enabled applications on Ubuntu runners.
- Moved desktop-service IPC sockets into a private per-user runtime directory,
  restricted socket permissions, authenticated peer users and processes,
  enforced endpoint-specific caller policies, added versioned message
  envelopes, and bounded request sizes.

## Historical tags

The repository contains the earlier `v0.1-gdm-session` tag. It predates this
changelog; consult the tagged source for its exact contents.
