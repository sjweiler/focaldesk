# Security Policy

FlowState is alpha compositor and desktop environment software. Security-sensitive
behavior may include process launching, IPC, permissions, portals, input handling,
display output, file access, and automation features.

## Supported Versions

Security fixes are handled on the current default branch. FlowState does not
currently maintain separate supported release branches.

## Reporting a Vulnerability

Please do not report security vulnerabilities in public issues, discussions, or
pull requests.

If GitHub private vulnerability reporting is enabled for this repository, use
that channel. Otherwise, contact a maintainer privately. If you do not have a
private contact path, open a public issue asking for a security contact without
including exploit details, crash dumps, logs, repro steps, or affected code paths.

Include as much of the following as you can safely share:

- Affected component or crate.
- A short description of the vulnerability and impact.
- Reproduction steps or proof-of-concept details.
- Relevant platform details, including Linux distribution, compositor/session
  environment, GPU driver, and Wayland-related versions when applicable.
- Whether the issue is already public or known to be exploited.

## Response Expectations

Maintainers will acknowledge valid reports when possible, investigate the issue,
and coordinate a fix before public disclosure. Because FlowState is early alpha,
response times and release processes may vary.

## Disclosure

Please allow maintainers reasonable time to investigate and prepare a fix before
publishing details. Once a fix is available, public disclosure should include
enough information for users and contributors to understand the risk and upgrade
or patch appropriately.
