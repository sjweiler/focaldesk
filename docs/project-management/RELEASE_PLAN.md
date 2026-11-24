# Release Plan

FocalDesk ships alpha releases from tags through the existing GitHub Actions release workflow. The project owner decides when a candidate is ready. See the [alpha release workflow](../../.github/workflows/release.yml) and [repository readiness criteria](../../ROADMAP.md#release-readiness).

## Candidate checks

1. Identify the candidate revision and confirm the required CI checks pass.
2. Run release readiness checks and review the build instructions and known issues.
3. Verify the install and uninstall lifecycle. Smoke-test affected session, compositor, or service behavior on relevant configurations and record the results.
4. Review open risks and update the changelog with user-visible changes and known limitations.
5. Tag the approved revision using the workflow's tag pattern. Verify that the workflow publishes the expected release artifacts and link the release from the candidate status report.

Use the [readiness script](../../scripts/check-release-readiness.sh), [install lifecycle test](../../scripts/test-install-lifecycle.sh), [building guide](../building.md), [known issues](../known-issues.md), and [changelog](../../CHANGELOG.md).
