# Private AIOS Registry

The private registry shares signed `.fai` packages without weakening the local
Package Manager boundary. Network access is opt-in: the registry unit is
installed disabled, the CLI performs network operations only for explicit
`sync`, `download`, `publish`, `approve`, or `revoke` commands, and downloaded
bundles remain ordinary quarantine files until separately inspected, trusted,
staged, and activated.

## Trust model

- The registry signs every catalog with a dedicated Ed25519 key held by
  `focald-secrets` at `aios/registries/<id>/catalog-ed25519`.
- Clients pin that catalog public key. Catalog sequence rollback, signature
  failure, duplicate coordinates, malformed digests, and key changes fail
  closed.
- Registry policy contains approved package signer IDs and public keys.
- Published package coordinates are immutable. Revoked versions remain visible
  in catalogs but cannot be downloaded or selected into a lockfile.
- Read and publish bearer tokens are distinct, live in `focald-secrets` on the
  server, and are read from private `0600` files by explicit CLI clients.
- Direct serving is loopback-only. Put a TLS-authenticated reverse proxy in
  front of the loopback listener for access from another machine.

## Server setup

Install the opt-in service:

```text
just install-ai-registry-fedora
```

Generate bootstrap tokens without printing them:

```text
focaldesk-ai-registry generate-token ~/.config/focaldesk/registry-read.token
focaldesk-ai-registry generate-token ~/.config/focaldesk/registry-publish.token
```

Create `~/.config/focaldesk/ai-registry.env` with mode `0600`:

```text
FOCALDESK_AI_REGISTRY_COMMAND=init
FOCALDESK_AI_REGISTRY_ID=engineering
FOCALDESK_AI_REGISTRY_ROOT=/home/USER/.local/share/focaldesk/private-registry
FOCALDESK_AI_REGISTRY_BIND=127.0.0.1:9473
FOCALDESK_AI_REGISTRY_READ_TOKEN_FILE=/home/USER/.config/focaldesk/registry-read.token
FOCALDESK_AI_REGISTRY_PUBLISH_TOKEN_FILE=/home/USER/.config/focaldesk/registry-publish.token
```

Install the updated secrets ACL, then start the unit once. Initialization
prints only the registry ID and catalog public key. Change
`FOCALDESK_AI_REGISTRY_COMMAND` to `serve`, run `systemctl --user daemon-reload`,
and explicitly enable the service only when network sharing is desired.

The registry writes a private append-only `audit.jsonl` for accepted signer
approvals, publications, and revocations. It never records bearer tokens or
package contents.

## Client workflow

Approve a package signer and publish:

```text
focaldesk-cli ai package registry approve \
  http://127.0.0.1:9473 ~/.config/focaldesk/registry-publish.token \
  release-signer <public-key-hex>

focaldesk-cli ai package registry publish \
  http://127.0.0.1:9473 ~/.config/focaldesk/registry-publish.token package.fai
```

Explicitly synchronize and browse a pinned catalog:

```text
focaldesk-cli ai package registry sync \
  http://127.0.0.1:9473 <catalog-public-key> \
  ~/.config/focaldesk/registry-read.token catalog.json

focaldesk-cli ai package registry browse catalog.json <catalog-public-key>

focaldesk-cli ai package registry diff \
  catalog.json <catalog-public-key> focus-kit 1.0.0 1.1.0
```

Resolve exact dependencies into a deterministic lockfile, then download into
quarantine:

```text
focaldesk-cli ai package registry lock \
  catalog.json <catalog-public-key> focus-kit 1.0.0 focus-kit.lock.json

focaldesk-cli ai package registry download \
  http://127.0.0.1:9473 <catalog-public-key> \
  ~/.config/focaldesk/registry-read.token catalog.json \
  focus-kit 1.0.0 focus-kit-1.0.0.fai
```

The download checks catalog coordinates, digest, signer key, package signature,
and the bundled Scenario Lab suite. It does not add the file to the local
registry or Package Manager. Continue with `package inspect`, explicit signer
trust, `stage`, and `activate`.

The AI Console Packages workspace can verify and browse a pasted signed catalog
against its pinned key, including revocation status and authority summaries.
It performs no background synchronization.
