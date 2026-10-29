# fleet-api

Kubernetes custom resources for fleet management: API group `fleet.believer.dev`, version `v1alpha1`.

This crate is the contract between the fleet coordinator (which watches and writes these objects) and its
clients, Friendshipper among them. It holds the `CustomResource` types, their validation, small helpers, and
the generated CRD YAML. It holds no deployment-specific values: those live in the `ShardConfig` objects (shard
names, healthcheck URLs, channels) and in the coordinator's own configuration (where builds and launcher
pointers are stored).

## Kinds

All kinds are namespaced; they live in `fleet-system` (`DEFAULT_NAMESPACE`).

| Kind | Short name | Created by | Spec written by | Status written by |
|---|---|---|---|---|
| `ShardConfig` | `shardcfg` | git (Flux) | git (Flux) | coordinator (conditions only) |
| `Shard` | `shard` | coordinator | — (`spec` is empty) | coordinator |
| `Promotion` | `promo` | Friendshipper, or by hand | immutable after creation | coordinator |

### ShardConfig

The guardrails for one shard.

- `metadata.name` must equal `spec.shardName`.
- `channels` is a non-empty list of `{kind: launcher, metadataPath, gameConfig}` or
  `{kind: steam, branch, gameConfig}`, unique by channel key. A Steam channel's `gameConfig` must be `Test` or
  `Shipping`.
- A Steam branch may belong to at most one `ShardConfig` in the namespace. CEL can't check across objects, so
  the coordinator reports a conflict as a `ConfigValid=False` condition (reason `SteamBranchConflict`).
- `promotion.allowDeployBackend` (default `false`) decides whether promotions may deploy the backend.

### Shard

What a shard serves, observed by the coordinator: the backend's parts (from its healthcheck), each channel's
version, and recent promotions. The coordinator creates one per `ShardConfig`, without an owner reference, so
deleting a `ShardConfig` keeps the observed history.

- **Partial failure never blanks good data.** Each item has its own `observedAt` and `error`; a failed
  observation changes only `error`, so `observedAt` is the last successful observation. Readers show an item
  as stale when `observedAt` is older than three poll intervals (`STALE_AFTER_POLL_INTERVALS`).
- **Backend parts deploy independently,** so differing versions across parts are normal. Each part's version
  is `<build time>_<release>_<branch>_<sha>`; parse it with `BackendVersion` rather than splitting by hand.
  The time is when the artifact was built, in UTC, not when it was deployed. Nothing reports a per-part
  deploy time, and `launchedAt` is when the backend process last started, which a crash or reschedule also
  changes.
- **`source`** is `observed` when the version was read from the channel, or `promotion` when it is the
  fallback: the last successful `Promotion` set it, and the channel hasn't been observed since.
- **`since`** changes only when `version` does. For a launcher channel it is the pointer's S3 `LastModified`
  (when the pointer was written). For Steam it is when the coordinator first saw the build, not when it went
  live: it lags by up to a poll interval, and on a channel's first observation it is that observation's time.
  Steam's `buildCreatedAt` is when the build was uploaded, not when it was set live.

### Promotion

A request to put a build on one channel of a shard.

- `spec.version` is a full 40-character lowercase commit SHA (checked by the schema).
- The spec is immutable. To cancel, set the annotations `fleet.believer.dev/cancel-requested-at` (RFC 3339 UTC,
  whole seconds) and `fleet.believer.dev/cancel-requested-by` in one merge patch (`Promotion::cancel_patch`); the
  coordinator only reads them, and `Promotion::cancel_request` reports them. The presence of `cancel-requested-at`
  is the request, even if it doesn't parse. A cancelled promotion stays in history as `Cancelled` /
  `CancelRequested`, or finishes with its real result if its workflow had already finished. Deleting one by hand
  also stops its workflow (`Cancelled` / `Deleted`), but the record goes.
- Phases: `Pending → Running → Succeeded | Failed`, or `Rejected` (validation failed, nothing ran), or
  `Cancelled`. A promotion with no phase yet hasn't been picked up: `Promotion::phase` returns `None`, and
  `Promotion::is_active` counts it as holding the channel.
- `spec.gameConfig` is passed to the workflow's `game_config` parameter unchanged (`Development`, `Test`,
  `Shipping`; `GameConfig::workflow_value`). The Steam upload writes it verbatim into the build description,
  which the coordinator parses back, so it is never lowercased on the way in.
- `spec.sign` (Steam only) is optional. When it is unset the coordinator doesn't pass `sign` to the workflow,
  so the workflow template's default applies.
- `status.reason` takes the values in `reasons`, e.g. `ChannelBusy` when another promotion holds the channel.
- Labels `fleet.believer.dev/shard` and `fleet.believer.dev/channel` mirror the spec, and annotation
  `fleet.believer.dev/changed-by` records who asked. `Promotion::request` sets all three.
- The `Channel` print column reads the `fleet.believer.dev/channel` label, so a hand-made Promotion created
  without labels shows it blank until the coordinator corrects the labels.
- The coordinator keeps the last `RETAINED_FINISHED_PROMOTIONS` (5) finished promotions per channel, plus each
  channel's latest `Succeeded` one however old.

## Channel keys

A channel is identified within its shard by a key:

| Form | Launcher | Steam | Used in |
|---|---|---|---|
| Key | `launcher/<metadataPath>` | `steam/<branch>` | `Shard.status.channels[].key`, `promotions[].channel` |
| Label | `launcher.<metadataPath>` | `steam.<branch>` | the `fleet.believer.dev/channel` label |

`metadataPath` and `branch` are restricted to label-safe characters (`[A-Za-z0-9._-]`, alphanumeric at both
ends, at most 54 characters), so the two forms convert both ways without loss. Use `ChannelKey` for both.

## CRD YAML

The CRDs under `crds/` are generated from the types. After changing a type, regenerate and commit them:

```sh
cargo run -p fleet-api --features crdgen --bin crdgen
```

`cargo test -p fleet-api` fails while the committed YAML is stale.

The `crdgen` feature exists only to give k8s-openapi a Kubernetes version so the generator can build on its
own. Library consumers must not enable it: k8s-openapi accepts exactly one version feature per build, and that
choice belongs to the final binary.

## Using the crate

- Within this workspace, use a path dependency.
- From another repository, use a git dependency pinned by `rev`. To develop against a local
  checkout, add a `[patch]` for the git URL in `~/.cargo/config.toml` rather than editing the dependency.
- Enable a k8s-openapi version feature (`v1_32` or later) in your binary, as for any k8s-openapi library.

## Changes

`v1alpha1` may gain optional fields freely. Renaming or removing a field, or changing what one means, needs an
entry in [CHANGELOG.md](CHANGELOG.md), and every consumer bumps together.
