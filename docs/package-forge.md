# AIOS Package Forge

Package Forge is the authoring layer for signed FocalDesk `.fai` packages. It
creates reproducible declarative bundles, runs their Scenario Lab contracts,
and delegates signing to the AI service so private Ed25519 seeds never cross
the AI IPC boundary.

## First package

```text
focaldesk-cli ai package keygen local-dev
focaldesk-cli ai package init focus-kit \
  --id focus-kit --name "Focus Kit" --signer local-dev
cd focus-kit
focaldesk-cli ai package test .
focaldesk-cli ai package build . focus-kit.fai
focaldesk-cli ai package verify focus-kit.fai
```

`keygen` stores the seed at the opaque
`aios/signers/<signer-id>/ed25519` handle in `focald-secrets`. It prints only
the signer ID and public key. Creating an existing signer is rejected rather
than silently rotating it.

`init` refuses to overwrite an existing directory. The generated
`fai-project.json` contains a bounded read-only example agent and a passing
synthetic scenario. `build` and its `sign` alias refuse to overwrite an output
file, test every scenario, retrieve the seed inside the AI service, and return
only the signed bundle.

## Local registry

```text
focaldesk-cli ai package registry add focus-kit.fai
focaldesk-cli ai package registry list
focaldesk-cli ai package registry search focus
```

The filesystem registry defaults to
`$XDG_DATA_HOME/focaldesk/fai-registry` and has no network behavior. Override
it with `FOCALDESK_FAI_REGISTRY`. It accepts only correctly signed bundles with
passing scenarios and requires exact dependency versions to exist before a
dependent bundle is added. Replacing a version requires the explicit
`--overwrite` option.

The AI Console **Packages** workspace includes a Forge project editor,
protected signer generation, local Scenario Lab testing, and signed bundle
building. A built bundle is loaded into the existing inspection editor; it is
not trusted, staged, or activated automatically.

## CI

Use [`package-ci.example.yml`](package-ci.example.yml) as a starting point.
CI should run `package test` on source projects. Signing should happen only in
a protected release job connected to an approved signing service; do not place
the raw seed in a repository or ordinary CI variable.
