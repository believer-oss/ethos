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
| `Shard` | `shard` | coordinator | Friendshipper (Fleet settings) | coordinator |
| `Promotion` | `promo` | Friendshipper, or by hand | immutable after creation | coordinator |
| `FleetRelease` | `fr` | coordinator, or Friendshipper ("warm this build") | coordinator; Friendshipper (pin, overrides, retire) | coordinator |
| `LocalFleetRelease` | `lfr` | coordinator, in each game cluster | coordinator | cluster mode |

The coordinator runs on the cluster that holds `ShardConfig`, `Shard`, `Promotion` and `FleetRelease`. Cluster
mode runs in each game cluster, reads only its `LocalFleetRelease`s, and renders the Agones `Fleet`s and
`FleetAutoscaler`s; it keeps reconciling while the coordinator is unreachable.

### ShardConfig

The guardrails for one shard.

- `metadata.name` must equal `spec.shardName`.
- `channels` is a non-empty list of `{kind: launcher, metadataPath, gameConfig}` or
  `{kind: steam, branch, gameConfig}`, unique by channel key. A Steam channel's `gameConfig` must be `Test` or
  `Shipping`.
- A Steam branch may belong to at most one `ShardConfig` in the namespace. CEL can't check across objects, so
  the coordinator reports a conflict as a `ConfigValid=False` condition (reason `SteamBranchConflict`).
- `promotion.allowDeployBackend` (default `false`) decides whether promotions may deploy the backend.
- `shardId` and `backendAddress` are what game servers are labelled and started with. They are optional in the
  schema; the coordinator reports a missing one as `ConfigValid=False` once the shard has Fleets.
- `fleets` (absent: the shard never has Fleets) lists the clusters it may run Fleets in (`{name, zone}`,
  non-empty, unique by name), the image repository (`<imageRepository>:<version>`), `limits.maxReplicasPerFleet`
  (required, CEL: the most `maxReplicas` any Fleet may have, and the warm sizing's `maxReplicas` when it sets none),
  the warm `gate` (`minReady`, `stableFor`, `timeout`), `retireAfterIdle` and `warmTtl`. Unset values take the
  defaults in `fleet_defaults` (gate 1 / 2m / 20m, `retireAfterIdle` 30m, `warmTtl` 24h); the accessors
  (`FleetGate::timeout`, `FleetsConfig::warm_ttl`, …) apply them. The gate's `timeout` must be at least its
  `stableFor`, with unset fields as their defaults (CEL), or the gate could never pass. A shard with `fleets` must have a name of at
  most 15 characters (`FLEET_SHARD_NAME_MAX_LEN`), so Fleet names fit.

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
- **`spec`** holds the shard's Fleet settings, edited in Friendshipper:
  - `fleetsEnabled`: the master switch. Off stops new releases and retires those no channel serves (unless
    pinned); releases serving a channel keep their Fleets until the channel moves on.
  - `maps`: the map IDs that get Fleets, keys of the build manifest's `maps.levelAssets`. At least one when
    `fleetsEnabled` (CEL); there is no default map. A map ID is a label value of at most `MAP_ID_MAX_LEN` (63)
    characters, and no two may match once lowercased with `-`, `_` and `.` alike (CEL). For IDs short
    enough to keep their whole slug, such a pair (`QA_BOX_Rotator`, `QA-BOX-Rotator`) would name the same Fleet.
  - `warm`: the absolute buffer each Fleet keeps while its release is Warming or Warm. `warm.bufferSize` is
    required when `fleetsEnabled` (CEL); there is no default.
  - `live`: sizing while a channel serves the release, most specific first: `overrides` (map and cluster) >
    `maps` (map) > `default` > `fleet_defaults::LIVE` (20% / 1 / 10; the coordinator fills min 0 under an
    absolute buffer and caps max at the limit). Levels may be partial; unset fields come from the next level.
- **`status.releases`** summarises the shard's `FleetRelease`s.
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
- **Steps.** A promotion runs `deploy-backend`, `warm-fleets`, `publish-client` and `flip-channel`
  (`PROMOTION_STEPS`), then marks its release live. `status.steps` lists all four from acceptance on; one that
  won't run has phase `Skipped` and a message saying why. Find steps by name (`PromotionStatus::step`), not
  position. `status.currentStep` names the running one. A step's workflow is `promote-<promotion>-<step>`
  (`step_workflow_name`) and carries the `fleet.believer.dev/step` label; `warm-fleets` runs in the coordinator
  and has no workflow. `steps[].outputs` copies a workflow's output parameters (e.g. `build_id`). A promotion
  made before steps were split has one step, `promote`, with workflow `promote-<promotion>` (`workflow_name`).
- **The warm gate.** `warm-fleets` waits for the build's `FleetRelease` (`status.fleetRelease`) to be Warm, and
  fails the promotion with `FleetGateTimeout` after the shard's `gate.timeout`; nothing has been published by
  then. `spec.skipFleetGate: {reason}` (reason required) skips the wait; the release is still created.
- **`status.changes`** records each effect outside the cluster as it happens (`BackendDeployed`,
  `FleetReleaseWarming`, `ClientUploaded`, `ChannelFlipped`, `FleetReleaseLive`), for every outcome, so a
  cancelled or failed promotion shows what it left behind.

### FleetRelease

One build's Fleets on one shard, named `<shard>-<first 8 characters of the version>` (`release_name`; CEL
checks it). `spec.shard` and `spec.version` are immutable.

- `spec.phase` is the phase wanted: people set `Warm` (create one to warm a build) or `Retired`; the coordinator
  sets `Live` when a channel serves the version, and `Retired` when a Draining release has been idle for
  `retireAfterIdle` or an unserved one passes `warmUntil`. `pinned` blocks both automatic retirements. `force`
  lets retiring end matches still running. `FleetRelease::request`, `pin_patch` and `retire_patch` build the
  objects and patches, with `changed-by`.
- The coordinator puts the `fleet.believer.dev/retire` finalizer on every release. Deleting one, by hand or by
  retention, retires it as if `spec.phase` were `Retired`, even a Live one: the coordinator deletes its
  `LocalFleetRelease` in every cluster and removes the finalizer once no cluster holds one, waiting for any it
  can't reach.
- `status.phase`: `Warming → Warm → Live → Draining → Retiring → Retired`, or `Failed` when nothing could be
  rendered. Draining goes back to Live if a channel serves the version again before it retires.
  `status.phaseSince` is when the phase last changed; a Retired release is deleted `RETIRED_RELEASE_RETENTION`
  (7 days) after it. Conditions: `ConfigValid`, `MapsChecked`, `ClustersSynced`, `GatePassed` (current, not
  sticky: False again while a cluster takes a new spec), `ImageExists` and `DesiredPhaseIgnored`; their reasons are
  in `conditions`.
- `status.buildMapIds` lists every map the build has a level for (the keys of its manifest's `maps.levelAssets`),
  for map pickers. `status.maps` is the map check per fleet map against the manifest: `OK`, `NotInBuild` (no
  `levelAssets` entry) or `NotCooked` (level not in `maps.cooked.server`), with the level URL. Anything but `OK`
  fails the release, as does a manifest without either field (`MapsChecked=False`, reason `NoLevelAssets` or
  `NoCookedMaps`); `status.clusters` reports the gate and each map's Fleet (desired, ready, allocated) per cluster, and
  `retireStuck` (`since`, `message`) while that cluster's `LocalFleetRelease` reports `RetireStuck=True`.

### LocalFleetRelease

A `FleetRelease` resolved for one game cluster: shard name and ID, version, image (`<repository>:<version>`,
checked by CEL), backend address, zone, gate, and per map the level URL and a resolved sizing (`bufferSize` and
`maxReplicas` set, or `parked: true`). Named like its `FleetRelease`; `LocalFleetRelease::request` sets the
name, labels and the `fleet.believer.dev/retire` finalizer. Deleting it retires its Fleets: cluster mode
removes the finalizer once every Fleet is gone, waiting for Allocated servers unless `force` is set.

## Sizing

One shape everywhere (`Sizing`): `{bufferSize?, minReplicas?, maxReplicas?, parked?}`. `bufferSize` is an
integer or a percentage string such as `"20%"`. `parked: true` scales the Fleet to zero (Agones spares
Allocated servers) with no autoscaler, and the other fields are ignored.

CEL refuses a sizing that breaks Agones' Buffer policy rules on the fields it sets: `minReplicas ≤
maxReplicas`; an absolute `bufferSize` is at least 1 and at most `maxReplicas`, with `minReplicas` either 0 or
at least `bufferSize`; a percentage is 1%–99% and needs `minReplicas ≥ 1`. Partial objects are checked only on
the fields they have. `Sizing::filled_from` fills a partial sizing from a less specific one, and
`Sizing::validate` checks a resolved value in full, including the shard's `limits.maxReplicasPerFleet` (which
CEL can't see).

## Names

| Helper | Gives | Example |
|---|---|---|
| `release_name(shard, version)` | `FleetRelease` and `LocalFleetRelease` name | `test-abc12345` |
| `map_slug(map_id)` | map ID in name form, shortened past 26 characters | `arena-tutorial` for `Arena_Tutorial` |
| `fleet_name(shard, version, map_id)` | `Fleet` and `FleetAutoscaler` name | `test-abc12345-arena-tutorial` |
| `check_map_slugs(map_ids)` | `SlugCollision` for two map IDs that would name one Fleet | `QA_BOX_Rotator`, `QA-BOX-Rotator` |
| `step_workflow_name(promotion, step)` | a promotion step's workflow | `promote-test-steam-abcde-deploy-backend` |
| `level_url(level_asset, map_id)` | a Fleet server's first argument | `/Game/Maps/L_Arena.L_Arena?MapRowName=Arena_Tutorial` |
| `level_package(level_asset)` | the level package, to compare with a build's cooked maps | `/Game/Maps/L_Arena` |

A Fleet name is at most `FLEET_NAME_MAX_LEN` (51) characters, not 63, so every name Agones generates from it
keeps it whole: GameServerSets are `<fleet>-xxxxx` and GameServers `<set>-xxxxx` (`generateName`, whose base
Kubernetes cuts short rather than pass 63 characters). A 15-character
shard (`FLEET_SHARD_NAME_MAX_LEN`) and the 8-character version leave `MAP_SLUG_MAX_LEN` (26) for the map slug.
A longer slug keeps its first 19 characters (less a trailing `-`), then `-` and the first 6 hex characters of the
map ID's SHA-256 (`printf %s <map ID> | sha256sum`), e.g. `abcdefghijklmnopqrs-ebf512` for
`Abcdefghijklmnopqrstuvwxyz1`; a slug that fits is used as is. The Fleet's `believer.dev/map` label keeps the full
map ID. CEL can't hash, so two long IDs whose shortened slugs match are refused only by `check_map_slugs`, when
the Fleets are rendered.

`level_url` replaces any `?…` the level asset carries: when two maps share a level, `maps.json` lists it with
one map's row name, and each Fleet needs its own. `game_server_labels` holds the labels the allocation
contract requires on Fleet game servers.

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
