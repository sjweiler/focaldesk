# Agent Studio

The AI Console's **Agent Studio** is the control surface for installed agent
profiles, agent-manifest packages, and bounded agent authoring.

## Inspect and control installed agents

1. Open **Agent Studio** and select **Refresh health**.
2. Review the installed-agent list. Each profile shows its enabled state,
   description, built-in or installed source, declared tool allowlist,
   execution limits, voice and memory flags, triggers, daily usage, and
   declared capability scopes.
3. Choose an agent in the **Installed agent** selector, then use **Enable** or
   **Disable** to persist its state. Refresh the view after a change to confirm
   the result.

Capability scopes include any declared filesystem roots, network origins,
application, workspace, service, secret-handle, and context-kind access. A
missing capability policy means the manifest specifies no extra resource
scope; its listed tools are still subject to the effective capability lease
and daemon permission checks. An enabled profile does not bypass one-shot
confirmation for a proposed mutation.

## Rollback controls

The **Roll back** button in Agent Studio restores the previous `agent.toml`
manifest for an installed agent-directory package. It does not roll back a
signed `.fai` package. To restore a prior signed package version, open
**Packages**, enter the package ID, and select **Rollback** there. The Packages
workspace's **Refresh installed** button shows active and staged versions,
signer identity, and rollback availability. See the
[AIOS Package Manager](package-manager.md) for signed package requirements.

After successful `.fai` package installation, update, or rollback, all agents
from the active package are disabled until explicitly enabled again. This
applies even when an agent ID is unchanged. An ordinary AI server restart
restores each active package agent's persisted enabled or disabled choice.
Newly installed package agents start disabled and stay disabled across
restarts until enabled.

## Build and simulate agents

The form can author an agent's identity, instructions, allowed tools, resource
budgets, capability scopes, and optional trigger. **Validate & preview** checks
the definition and displays normalized TOML without installing it. **Install
package** writes it to the configured agent directory; updating an existing
agent requires the explicit update option and retains a backup manifest.

**Preview authority** shows the effective capability boundary for the selected
form agent. **Simulate plan** asks the configured provider for a bounded plan
without executing tools. The health view also reports retained run counts,
failures, tokens, and estimated cost when pricing is configured. The emergency
trigger switch pauses scheduled and event-driven starts while leaving manual
runs available.

For the Rust API, trigger model, durable run behavior, and agent-building SDK,
see the [FocalDesk Agent SDK](agent-sdk.md).
