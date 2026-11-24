# Project Roadmap

The [repository roadmap](../../ROADMAP.md) tracks technical milestones and completion. Use this page to connect current priorities to the evidence needed for release decisions.

| Priority | Evidence to review |
| --- | --- |
| Stabilize session startup and recovery | Reproducible startup and recovery checks, with known failure modes documented |
| Improve display and application compatibility | Recorded nested and DRM/KMS results, including relevant hotplug, scaling, and mixed-refresh cases |
| Harden permissions and service boundaries | Relevant permission, consent, socket, and service lifecycle checks |
| Keep alpha delivery repeatable | Passing release checks and documented installation, verification, and removal results |

Review priorities when compatibility results, regressions, or security findings change what can ship. Track release evidence in [Status Reports](STATUS_REPORTS.md) and apply the [Release Plan](RELEASE_PLAN.md) to each candidate.
