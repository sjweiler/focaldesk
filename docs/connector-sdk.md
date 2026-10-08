# Connector SDK and Trust Store

FocalDesk connectors are bounded producers for the Event Fabric. A connector
cannot execute an agent or workflow. It can only submit a typed event through
the same consent, redaction, retention, Attention, capability, and native
confirmation boundaries as built-in producers.

## Manifests

`ConnectorManifest` version 1 declares a stable ID, display metadata, package
version, event sources, source-specific fields, optional network domains, an
optional managed runtime, and an opaque signing-key handle. Validation rejects unknown event fields,
undeclared sources, malformed domains, oversized manifests, built-in identity
claims, and external connectors without a signing-key handle.

External key handles must be inside `connectors/<connector-id>/`. Key material
is retrieved by the AI service from `focald-secrets`; it is never returned by
connector status or the AI Console. Built-in connectors use an internal trusted
path and have no key handle.

The five built-ins are installed disabled:

- `desktop-events`
- `workflow-events`
- `service-health`
- `notifications`
- `local-calendar`

Desktop and workflow connectors are wired to existing typed runtime events.
The remaining built-ins define the local schema and trust identity used by
their service producers. The separately packaged `focald-connectors` host
provides the bounded local adapters, but it does not poll a built-in until both
the connector and its Event Fabric source are explicitly enabled.

## Authenticated event publishing

External connectors construct a `ConnectorEventRequest` with their connector
ID, source, current Unix timestamp, random nonce, and JSON payload. The public
`sign_connector_event` and `random_connector_nonce` helpers produce the
HMAC-SHA256 request signature. The AI service resolves the connector's opaque
key handle, verifies the signature in constant time, requires a timestamp
within five minutes, and rejects reused nonces.

The authenticated request is still rejected unless all of these are true:

1. the connector is installed and enabled;
2. its manifest declares the event source and fields;
3. the Event Fabric source policy is enabled;
4. at least one disclosed field is present;
5. the Event Fabric emergency disconnect is off.

Signing authenticates the producer; it does not bypass consent or authorize an
action. Use `SimulateEvent` for unsigned, non-retaining fixture evaluation.

## Trust-store lifecycle

The default trust store is
`$XDG_CONFIG_HOME/focaldesk/connectors.json` (or
`~/.config/focaldesk/connectors.json`). It persists connector enablement,
network consent, health metadata, and Event Fabric source policies. Writes use
a mode-0600 temporary file and atomic rename in a mode-0700 directory. Before
replacement, the previous regular file is copied to `connectors.json.bak`.

Installing or updating a connector never enables it. Updates require an
explicit overwrite flag and preserve the previous manifest for rollback.
Rollback disables the connector and network authority. Built-in manifests are
restored from compiled definitions during load and cannot be replaced or
rolled back through the trust store.

Declared network domains are exact hostnames. Network permission defaults off
and cannot be enabled for a connector that declares no domains. The current
built-ins declare no network access. External producers may either publish
authenticated requests themselves or declare a managed runtime executed by
`focald-connectors`. The managed path applies transient systemd sandboxing,
resource limits, bounded retries, quarantine, and launch-time IP allow rules.
See [Managed Connector Host](connector-host.md) for its protocol and security
boundaries. The trust-store flag does not firewall an arbitrary independently
launched process.

## Testing and inspection

The Event Fabric Console page can install/update manifests, enable or disable
connectors, inspect health and last-event state, roll back updates, configure
source disclosure, simulate fixtures, and replay already-redacted retained
events. It also exposes managed-host status, polling, pause, resume, and
quarantine recovery. Recorded fixtures should contain synthetic or previously redacted
fields only. Simulation creates neither an event nor an Attention suggestion.
