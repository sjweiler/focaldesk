# Contributing

FocalDesk is currently a solo-developed alpha project. Contributions, bug
reports, and design feedback may be welcome, but there is no formal maintainer
team, release process, or guaranteed review timeline yet.

Participation in project spaces is subject to the
[Code of Conduct](CODE_OF_CONDUCT.md).

## Project Status

FocalDesk is experimental compositor and desktop environment software. Expect
breaking changes, incomplete features, and architectural churn.

Before contributing, read the [README](README.md) for the current project shape,
[building guide](docs/building.md), and [roadmap](ROADMAP.md).

## Issues

Use issues for:

- Reproducible bugs.
- Build or setup failures.
- Clear feature requests.
- Design questions that need a durable record.

When reporting a bug, include:

- What you expected to happen.
- What actually happened.
- Steps to reproduce the problem.
- Your Linux distribution, desktop/session context, GPU driver, and relevant
  Wayland environment details when applicable.
- Logs or terminal output when useful.
- The tested commit and whether the DRM/KMS or nested winit backend was used.

Do not include security-sensitive details in public issues. See
[SECURITY.md](SECURITY.md) for vulnerability reporting guidance.

## Pull Requests

Because the project is early and currently maintained by one person, small,
focused pull requests are much easier to review than broad rewrites.

Good pull requests usually:

- Solve one problem at a time.
- Match the existing Rust style and project structure.
- Include tests when the change touches behavior that can be tested.
- Avoid unrelated formatting, refactors, or dependency changes.
- Explain the user-visible impact or architectural reason for the change.

Large architectural changes should start as an issue or discussion before code is
written.

## Development Checks

Before opening a pull request, run the relevant checks when possible:

```sh
cargo fmt --all -- --check
cargo check --workspace
cargo clippy --workspace --all-targets
cargo test --workspace
./scripts/check-markdown-links.sh
```

If a check cannot be run locally, mention that in the pull request.

## Coding Guidelines

- Prefer simple, explicit Rust over premature abstraction.
- Keep compositor, IPC, permissions, and portal-related changes especially
  conservative.
- Follow the existing crate boundaries unless there is a clear reason to change
  them.
- Treat input handling, process launching, file access, IPC, and automation as
  security-sensitive areas.
- Keep user-facing behavior predictable and recoverable, especially for alpha
  features.
- Update the relevant status, configuration, keybinding, architecture, or
  troubleshooting documentation when behavior changes.

## Commit and Pull Request Notes

Use clear, imperative commit subjects and explain why the change is needed.
Pull requests should identify the affected backend or service, tests performed,
known limitations, and any follow-up work intentionally left out of scope.

Screenshots are helpful for visible UI changes. Remove personal information,
tokens, filenames, model prompts, and other sensitive data before attaching
images or logs.

## Maintainer Notes

At this stage, the project direction may change quickly. A contribution being
technically sound does not guarantee it fits the current roadmap.

Review and merge decisions are ultimately made by the project owner.
