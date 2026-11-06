# Changelog

Changes to the `fleet.believer.dev` API. Adding an optional field needs no entry; renaming or removing a field,
or changing what one means, does.

## 0.2.0

- New kinds `FleetRelease` and `LocalFleetRelease`, and Fleet settings: `ShardConfig.spec` gains `shardId`,
  `backendAddress` and `fleets` (which must set `limits.maxReplicasPerFleet`, and a `gate.timeout` of at least `gate.stableFor`); `Shard.spec` gains `fleetsEnabled`, `maps`, `warm` and `live` (what no live level sets comes from `fleet_defaults::LIVE`); `Shard.status`
  gains `releases`. Fleet maps are checked against the build manifest's `maps.levelAssets` and
  `maps.cooked.server`; `spec.maps` must be non-empty and `spec.warm.bufferSize` set when `fleetsEnabled`.
  A shard with `fleets` has a name of at most 15 characters, and Fleet names are at most 51; a map slug longer
  than 26 characters is shortened with a hash of the map ID. Map IDs are label values (at most 63 characters) and
  must not match once lowercased with `-`, `_` and `.` alike, so each map's Fleet name is its own.
- `Promotion`: a promotion now runs several steps. `status.steps` lists `deploy-backend`, `warm-fleets`,
  `publish-client` and `flip-channel` instead of the single `promote` step, so `steps[0]` is no longer the whole
  promotion; read steps by name. Each step's workflow is `promote-<promotion>-<step>` (`step_workflow_name`);
  `workflow_name` still names the single workflow of promotions made before the split. A step's `phase` may now
  be `Pending` or `Skipped`, and steps run by the coordinator have an empty `workflow`. New fields:
  `spec.skipFleetGate`, `status.currentStep`, `status.fleetRelease`, `status.changes`, `steps[].fleetRelease`
  and `steps[].outputs`.

## 0.1.0

- `v1alpha1`: `ShardConfig`, `Shard` (status only) and `Promotion`: channel observation and promotions
  through the coordinator.
