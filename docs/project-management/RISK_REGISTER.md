# Risk Register

These are initial tracking entries based on the documented alpha priorities. The project owner reviews them when assessing a release candidate or a material scope change.

| ID | Open risk | Impact | Mitigation and review evidence |
| --- | --- | --- | --- |
| R-01 | Hardware-dependent rendering, hotplug, or display regressions | High | Record GPU, driver, backend, and output configuration for manual compatibility checks |
| R-02 | Session startup, recovery, or configuration migration failure | High | Exercise affected recovery and migration paths and document known failure modes |
| R-03 | An IPC, automation, or connector change grants excess access | High | Review the affected permission boundary and verify expected denials |
| R-04 | Installation or uninstall leaves a broken session or stale files | High | Run the release and install lifecycle checks against the candidate |
| R-05 | Solo maintainer availability delays review or release | Medium | Keep blockers and deferred work visible in status reports and avoid publishing unverified dates |

When a risk changes, record the supporting issue or test result and update its status. Use the [security policy](../../SECURITY.md) for vulnerability reports and keep sensitive details out of this register.

See [Compatibility Testing](../compatibility-testing.md) and [Release Plan](RELEASE_PLAN.md).
