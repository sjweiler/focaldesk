# Security Policy

FocalDesk is alpha compositor and desktop environment software. Security-sensitive
behavior may include process launching, IPC, permissions, portals, input handling,
display output, file access, and automation features.

## Supported Versions

Security fixes are handled on the current default branch. FocalDesk does not
currently maintain separate supported release branches.

## Reporting a Vulnerability

Please do not report security vulnerabilities in public issues, discussions, or
pull requests.

Use [GitHub's private vulnerability report form](https://github.com/sjweiler/focaldesk/security/advisories/new).
If that form is unavailable, open a public issue asking the project owner to
establish private contact. Do not include vulnerability details, exploit code,
crash dumps, logs, reproduction steps, credentials, or affected code paths in
that public issue.

Include as much of the following as you can safely share:

- Affected component or crate.
- A short description of the vulnerability and impact.
- Reproduction steps or proof-of-concept details.
- Relevant platform details, including Linux distribution, compositor/session
  environment, GPU driver, and Wayland-related versions when applicable.
- Whether the issue is already public or known to be exploited.

## Response Expectations

Maintainers will acknowledge valid reports when possible, investigate the issue,
and coordinate a fix before public disclosure. Because FocalDesk is early alpha,
response times and release processes may vary.

Submitting a report does not guarantee a bounty or a particular disclosure
timeline. The project owner will try to acknowledge actionable private reports,
assess impact, prepare a fix, and coordinate disclosure according to available
time and the severity of the issue.

## Disclosure

Please allow maintainers reasonable time to investigate and prepare a fix before
publishing details. Once a fix is available, public disclosure should include
enough information for users and contributors to understand the risk and upgrade
or patch appropriately.
