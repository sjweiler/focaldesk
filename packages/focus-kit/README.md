# Focus Kit

Focus Kit is FocalDesk's first-party AIOS supply-chain example. Version 1.0.2
contains the read-only `daily-focus` agent, the two-node `focus-start` workflow,
and the suggestion-only `focus-kit-morning` routine. Version 1.0.1 is revoked
because its catalog summary omitted the local provider origin.

The package requests no filesystem, internet, application, service, secret, or
connector authority. Its only network origin is the loopback Ollama provider at
`http://127.0.0.1:11434`. It declares no automatic agent triggers. Active-window
and workspace context still require the normal separate live context grant.

The checked-in release bundles were signed by `focaldesk-focus-kit` with public key
`7ba2a8f7f471d29fefde802bf39fb69ef83899d1c7203fcaf448370fcb25a126`.
Its private signing seed remains in `focald-secrets`.

Validate the source project with:

```text
focaldesk-cli ai package test packages/focus-kit
```

Published package versions are immutable. Change the manifest version and
output filename before building a future release.
