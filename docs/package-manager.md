# AIOS Package Manager

FocalDesk AIOS packages are signed, declarative JSON bundles conventionally
stored with a `.fai` extension. A bundle can contain agent, workflow, routine,
and connector manifests plus one or more synthetic Scenario Lab fixtures. It
cannot contain an executable, a secret value, or an instruction to silently
grant authority.

## Lifecycle

1. **Inspect** validates every manifest, verifies the Ed25519 signature, runs
   every included scenario in the isolated evaluator, and reports requested
   authority and additions relative to the active version.
2. **Trust signer** records an explicit signer-id/public-key association in the
   private local package store. Signature validity alone does not establish
   trust.
3. **Stage** requires a valid trusted signature, passing scenarios, and exact
   active dependency versions.
4. **Activate** atomically selects the staged version and rebuilds the live
   agent, workflow, and routine registries. A failed registry reload restores
   the previous package selection.
5. **Rollback** swaps the active and prior versions and rebuilds the registries.

After a successful activation, every agent supplied by that package is disabled
until a user enables it again. This applies to first install, update, and
rollback, including an agent whose ID already existed in the prior version.
Review its authority and enable it from **Agent Studio** when it is ready to
run. This gives a changed package a fresh explicit enable decision.

An ordinary AI server restart preserves the last enable or disable choice for
each active packaged agent. A newly seen packaged agent starts disabled and
stays disabled across restarts until explicitly enabled. See
[Agent Studio](agent-studio.md) for the console workflow and the distinction
between agent-manifest rollback and package-version rollback.

Packaged connectors are installed disabled with network access denied. Package
activation does not enable connectors, event sources, agent triggers, voice
capture, or background execution. Those controls retain their existing,
separate consent paths.

The store defaults to
`$XDG_CONFIG_HOME/focaldesk/packages.json`, with directory mode `0700` and file
mode `0600`. Override it with `FOCALDESK_AI_PACKAGE_STORE`.

## CLI

```text
focaldesk-cli ai package inspect bundle.fai
focaldesk-cli ai package trust <signer-id> <ed25519-public-key-hex>
focaldesk-cli ai package stage bundle.fai
focaldesk-cli ai package activate <package-id>
focaldesk-cli ai package rollback <package-id>
focaldesk-cli ai package list
```

The AI Console exposes the same operations in the **Packages** workspace. Paste
the bundle JSON to inspect or stage it; authority and gate results are displayed
before activation. **Refresh installed** lists active and staged versions,
signer identity, and whether a prior version is available for rollback.

## SDK signing

Rust integrations can construct `FaiBundle` and call `sign_fai_bundle` with an
in-memory 32-byte Ed25519 secret key. The helper writes only the public key and
signature into the bundle. Applications should obtain private keys from a
secret service or protected signing process and must not serialize them into a
`.fai` file.

For project scaffolding, protected key generation, reproducible builds, and the
offline catalog, see [AIOS Package Forge](package-forge.md).

Signing covers the manifest, complete payload, and scenario suite using the
canonical field order emitted by the typed Rust structures. Any change after
signing invalidates the bundle.
