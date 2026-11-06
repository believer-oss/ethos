<script lang="ts">
	// The shard page's Fleet settings as one grid: a Defaults row, then a row per map with its on/off
	// box and live sizing, and per-cluster rows under it for overrides (the only place to park). Warm
	// sizing is one per shard, so only the Defaults row edits it. Empty cells show what they inherit.
	// Edits replace Shard.spec, recording changed-by, and are refused if someone else changed the
	// spec since it was loaded. With Fleets off, saving can also turn them on, in the same write.
	import { Button, Spinner } from 'flowbite-svelte';
	import { emit } from '@tauri-apps/api/event';
	import { getPromotionPlan, refreshFleets, setShardSpec } from '$lib/fleets';
	import {
		buildMapChoices,
		droppedOnSave,
		fleetMapProblem,
		fleetSettingsToForm,
		fleetsOnMissing,
		formSync,
		formToShardSpec,
		formToSizing,
		liveInherited,
		livePreviewText,
		mapSlug,
		MAP_SLUG_MAX_LEN,
		normalizedMapId,
		overrideRowIssues,
		pickBuildMaps,
		saveGeneration,
		servedVersions,
		sizingIssueText,
		sizingToForm,
		specChanges,
		STALE_SAVE,
		warmInherited,
		type FleetSettingsForm,
		type SizingForm
	} from '$lib/fleetSizing';
	import { releaseControlsEnabled, workflows } from '$lib/stores';
	import {
		formatSpan,
		gateStableForMs,
		gateTimeoutMs,
		shortSha,
		writeControl
	} from '$lib/fleetFormat';
	import type { Nullable, ShardDetail, ShardSpec } from '$lib/types';
	import ConfirmModal from '$lib/components/ConfirmModal.svelte';
	import ReleaseGate from './ReleaseGate.svelte';
	import SizingCells from './SizingCells.svelte';

	export let detail: ShardDetail;
	export let reload: () => Promise<void>;
	// With promotionViaCoordinator off: show the settings, change nothing.
	export let readOnly = false;
	// Collapses the grid back to the page's summary line.
	export let onCollapse: Nullable<() => void> = null;

	$: fleets = detail.config?.fleets ?? null;
	$: limit = fleets?.limits?.maxReplicasPerFleet ?? null;
	$: clusters = (fleets?.clusters ?? []).map((c) => c.name);
	$: liveDefault = detail.liveDefault ?? null;
	$: savedSpec = detail.spec;
	$: enabled = savedSpec?.fleetsEnabled === true;
	$: gateCannotPass = detail.releases.some((r) =>
		(r.status?.conditions ?? []).some(
			(c) => c.type === 'ConfigValid' && c.status === 'False' && c.reason === 'GateCannotPass'
		)
	);

	// The maps a recent build has levels for, from the first source with any: the manifest of a build
	// a channel serves (most recently arrived first), the releases' lists, then the newest
	// successful build's manifest. A ticked map outside them fails that build's release.
	$: fromRelease = buildMapChoices(detail.releases);
	$: releaseVersion = detail.releases.find((r) => r.metadata.name === fromRelease?.release)?.spec
		.version;
	$: newestBuild = $workflows.find(
		(c) => c.workflows.length > 0 && c.workflows.every((w) => w.status.phase === 'Succeeded')
	)?.commit;
	$: served = servedVersions(detail.status?.channels ?? []);
	// Manifest map IDs by version, once read; [] for a build with none or an unreadable manifest.
	let manifestMapIds: Record<string, string[]> = {};
	const readManifestMaps = async (shard: string, version: string) => {
		if (version in manifestMapIds) return;
		manifestMapIds = { ...manifestMapIds, [version]: [] };
		try {
			const plan = await getPromotionPlan(shard, version);
			manifestMapIds = { ...manifestMapIds, [version]: plan.manifest?.mapIds ?? [] };
		} catch {
			// Without it, the next source is used: no error for a help like this.
		}
	};
	$: for (const version of served) void readManifestMaps(detail.name, version);
	$: if (!fromRelease && newestBuild) void readManifestMaps(detail.name, newestBuild);
	$: buildMaps = pickBuildMaps(served, fromRelease, releaseVersion, newestBuild, manifestMapIds);
	$: buildMapIds = buildMaps?.mapIds ?? [];

	// The form, and the spec generation it was loaded at. A poll replaces it only while unedited, which
	// it is from the start, so the first poll loads it.
	let form: FleetSettingsForm = fleetSettingsToForm(null);
	let loadedForm: FleetSettingsForm = fleetSettingsToForm(null);
	let loadedAt: Nullable<number> = null;
	let loadedJson = JSON.stringify(form);
	// The spec the form was loaded from, to say what changed underneath an edit.
	let loadedSpec: Nullable<ShardSpec> = null;
	let saving = false;
	let saveError = '';

	const emptyForm = (): SizingForm => sizingToForm({});

	// Every fleet map gets a form for its own sizing, so its cells can bind; empty ones save nothing.
	const withMapForms = (f: FleetSettingsForm): FleetSettingsForm => {
		const liveMaps = { ...f.liveMaps };
		f.maps.forEach((m) => {
			liveMaps[m] ??= emptyForm();
		});
		return { ...f, liveMaps };
	};

	const loadForm = (spec: Nullable<ShardSpec>, version: Nullable<number>) => {
		form = withMapForms(fleetSettingsToForm(spec));
		loadedForm = JSON.parse(JSON.stringify(form)) as FleetSettingsForm;
		loadedJson = JSON.stringify(form);
		loadedSpec = spec;
		loadedAt = version;
		saveError = '';
	};

	// `stale`: the spec changed underneath an edit, so a save would be refused: say so, and disable
	// Save.
	$: ({ dirty, load, stale } = formSync(
		JSON.stringify(form),
		loadedJson,
		detail.generation,
		loadedAt
	));
	$: if (load) loadForm(savedSpec, detail.generation);
	$: changedUnderneath = stale ? specChanges(loadedSpec, savedSpec) : [];

	$: dropped = droppedOnSave(savedSpec, form);
	$: built = formToShardSpec(savedSpec, form, limit);
	// What Fleets on needs, in words, for the switch and "Save and turn Fleets on".
	$: missing = fleetsOnMissing(built.spec);
	$: problems = built.issues + (enabled ? missing.length : 0);

	// Which maps the grid lists: the ticked ones, or every map in the build too.
	let view: 'fleets' | 'all' = 'fleets';
	$: untickedInBuild = buildMapIds.filter((m) => !form.maps.includes(m));
	// Per map: whether its per-cluster rows are open.
	let openMaps: Record<string, boolean> = {};

	const toggleMap = (mapId: string, on: boolean) => {
		form.maps = on ? [...form.maps, mapId] : form.maps.filter((m) => m !== mapId);
		form = withMapForms(form);
	};

	const addOverride = (mapId: string) => {
		const used = form.overrides.filter((o) => o.mapId === mapId).map((o) => o.cluster);
		const cluster = clusters.find((c) => !used.includes(c)) ?? clusters[0] ?? '';
		form.overrides = [...form.overrides, { cluster, mapId, form: emptyForm() }];
		openMaps = { ...openMaps, [mapId]: true };
	};
	const removeOverride = (index: number) => {
		form.overrides = form.overrides.filter((_, i) => i !== index);
	};

	// A map typed by hand: the only way to pick one before any build has listed its maps.
	let newMap = '';
	$: newMapTrimmed = newMap.trim();
	$: newMapError = newMapTrimmed ? fleetMapProblem(newMapTrimmed, form.maps) : '';

	// A long map ID's Fleet names end in a shortened slug; show it, as kubectl lists that.
	let shortSlugs: Record<string, string> = {};
	const fillShortSlugs = async (mapIds: string[]) => {
		const unslugged = mapIds.filter(
			(mapId) => !shortSlugs[mapId] && normalizedMapId(mapId).length > MAP_SLUG_MAX_LEN
		);
		if (!unslugged.length) return;
		const slugs = await Promise.all(unslugged.map(mapSlug));
		shortSlugs = { ...shortSlugs, ...Object.fromEntries(unslugged.map((m, i) => [m, slugs[i]])) };
	};
	$: void fillShortSlugs(newMapTrimmed && !newMapError ? [...form.maps, newMapTrimmed] : form.maps);
	const addMap = () => {
		if (!newMapTrimmed || newMapError || form.maps.includes(newMapTrimmed)) return;
		toggleMap(newMapTrimmed, true);
		newMap = '';
	};

	$: warmIssues = formToSizing({ ...form.warm, parked: false }, limit, true).issues;
	$: defaultIssues = formToSizing(form.liveDefault, limit).issues;
	$: tickedOverrides = form.overrides.filter((o) => form.maps.includes(o.mapId));

	// Every save, the Fleets on/off ones included, tests the generation the form was loaded at, so a
	// spec that moved on since (a poll behind an open confirmation, say) is refused, not overwritten.
	const save = async (spec: ShardSpec, done: string) => {
		const generation = saveGeneration(loadedAt, detail.generation);
		if (generation === null) {
			saveError = STALE_SAVE;
			return;
		}
		saving = true;
		saveError = '';
		try {
			await setShardSpec(detail.name, generation, spec);
			await reload();
			await refreshFleets();
			loadForm(detail.spec, detail.generation);
			await emit('success', done);
		} catch (e) {
			saveError = (e as { message?: string })?.message ?? String(e);
		} finally {
			saving = false;
		}
	};

	// Turning off saves the saved spec with Fleets off, so it never carries unsaved edits along.
	// Turning on saves the form with Fleets on: one write, which CEL accepts once a map and the warm
	// buffer are set.
	let showToggleConfirm = false;
	const toggleText = (on: boolean): string =>
		on
			? `Turn Fleets off on ${detail.displayName}? No new releases are made, and releases no channel serves retire (unless pinned). Releases serving a channel keep their Fleets until the channel moves on; retire those yourself to drop them too.`
			: `Save and turn Fleets on for ${detail.displayName}? Promotions will warm Fleets for each build and wait for the warm gate before publishing.`;
	const onToggleConfirmed = async (confirmed: boolean) => {
		if (!confirmed) return;
		if (enabled) {
			await save(
				{ ...(savedSpec ?? { fleetsEnabled: false }), fleetsEnabled: false },
				`Turned Fleets off on ${detail.displayName}`
			);
		} else {
			await save(
				{ ...built.spec, fleetsEnabled: true },
				`Turned Fleets on for ${detail.displayName}`
			);
		}
	};

	$: gate = writeControl($releaseControlsEnabled);
	$: locked = readOnly || gate.disabled || saving;

	const OVERRIDE_SELECT =
		'rounded p-1 text-xs bg-secondary-600 dark:bg-space-800 text-white border border-gray-500 disabled:opacity-50';
</script>

<section id="fleet-settings" class="flex flex-col gap-3">
	<h2 class="text-lg text-white flex items-center gap-3 flex-wrap">
		Fleet settings
		{#if fleets}
			<span
				class="text-xs rounded px-1.5 py-0.5 {enabled
					? 'bg-green-900/40 text-green-300'
					: 'bg-gray-800 text-gray-400'}">{enabled ? 'On' : 'Off'}</span
			>
		{/if}
		{#if fleets && detail.generation != null}
			<span class="ml-auto flex text-xs rounded border border-secondary-600 dark:border-space-800">
				<button
					class="px-2 py-1 {view === 'fleets'
						? 'bg-secondary-800 dark:bg-space-800 text-white'
						: 'text-gray-400'}"
					on:click={() => {
						view = 'fleets';
					}}>Maps with Fleets ({form.maps.length})</button
				>
				<button
					class="px-2 py-1 {view === 'all'
						? 'bg-secondary-800 dark:bg-space-800 text-white'
						: 'text-gray-400'}"
					disabled={!buildMaps}
					title={buildMaps ? '' : 'No build has listed its maps yet'}
					on:click={() => {
						view = 'all';
					}}
					>All maps{#if buildMaps?.version}&nbsp;in build {shortSha(buildMaps.version)}{/if}
					({buildMapIds.length})</button
				>
			</span>
		{/if}
		{#if onCollapse}
			<button class="text-xs text-gray-400 hover:underline" on:click={onCollapse}>Collapse</button>
		{/if}
	</h2>

	{#if !fleets}
		<p class="text-gray-500 text-sm">
			This shard can't have Fleets: its ShardConfig has no fleets section.{#if enabled}&nbsp;Its
				Shard still has them on; turn them off to match.{/if}
		</p>
		{#if saveError}
			<div class="text-red-400 text-sm bg-red-900/20 p-2 rounded">Not saved: {saveError}</div>
		{/if}
		{#if enabled && detail.generation != null && !readOnly}
			<!-- The off switch stays, so a shard whose ShardConfig lost its fleets section can be turned off. -->
			<ReleaseGate let:disabled>
				<Button
					size="xs"
					color="alternative"
					class="w-fit"
					disabled={disabled || saving}
					on:click={() => {
						showToggleConfirm = true;
					}}>Turn Fleets off…</Button
				>
			</ReleaseGate>
		{/if}
	{:else if detail.generation == null}
		<p class="text-gray-500 text-sm">
			The coordinator hasn't created this shard's Shard object yet.
		</p>
	{:else}
		<p class="text-sm text-gray-400">
			Only the maps switched on get warm servers. Every other map still works: its matches start on
			on-demand servers, which take longer to start.
		</p>

		<div class="overflow-x-auto">
			<table class="text-xs text-gray-300 w-full">
				<thead>
					<tr class="align-top">
						<th class="text-left font-semibold pr-2 py-1 w-56">Map</th>
						<th
							colspan="3"
							class="text-left font-normal px-1 py-1 border-l border-secondary-600 dark:border-space-800"
						>
							<span class="font-semibold text-white">Warm buffer</span>
							<p class="text-gray-400">
								Servers kept Ready while a build warms up, before any channel serves it, so the
								first matches after the switch start at once.
							</p>
						</th>
						<th
							colspan="3"
							class="text-left font-normal px-1 py-1 border-l border-secondary-600 dark:border-space-800"
						>
							<span class="font-semibold text-white">Live sizing</span>
							<p class="text-gray-400">
								How Fleets scale once players are on the build: spare servers kept ahead of demand,
								with a floor and a ceiling, for as long as a channel serves it.
							</p>
						</th>
						<th class="min-w-[140px] border-l border-secondary-600 dark:border-space-800" />
					</tr>
					<tr class="text-gray-500 uppercase text-[10px]">
						<th />
						<th class="text-left px-1">Buffer</th><th class="text-left px-1">Min</th><th
							class="text-left px-1">Max</th
						>
						<th class="text-left px-1">Buffer</th><th class="text-left px-1">Min</th><th
							class="text-left px-1">Max</th
						>
						<th />
					</tr>
				</thead>
				<tbody>
					<tr class="bg-secondary-800 dark:bg-space-950">
						<td class="pr-2 py-1"
							><span class="font-semibold text-white">Defaults</span> every map with Fleets</td
						>
						<SizingCells
							bind:form={form.warm}
							inherited={warmInherited(limit)}
							issues={warmIssues}
							disabled={locked}
							loaded={loadedForm.warm}
						/>
						<SizingCells
							bind:form={form.liveDefault}
							inherited={liveInherited(form.liveDefault, [], liveDefault)}
							issues={defaultIssues}
							disabled={locked}
							loaded={loadedForm.liveDefault}
						/>
						<td class="px-2 text-gray-400 whitespace-nowrap"
							>{#if limit}max per Fleet: {limit}{/if}</td
						>
					</tr>
					{#if warmIssues.length || defaultIssues.length || (enabled && missing.includes('a warm buffer'))}
						<tr>
							<td colspan="8" class="pb-1 text-red-400">
								{[
									...warmIssues.map((i) => `Warm ${sizingIssueText(i)}`),
									...defaultIssues.map((i) => `Live ${sizingIssueText(i)}`),
									...(enabled && missing.includes('a warm buffer')
										? ['Fleets need a warm buffer; there is no default.']
										: [])
								].join(' · ')}
							</td>
						</tr>
					{/if}

					{#each form.maps as mapId (mapId)}
						{@const mapForm = form.liveMaps[mapId] ?? emptyForm()}
						{@const mapIssues = formToSizing(mapForm, limit).issues}
						{@const notInBuild = !!buildMaps && !buildMapIds.includes(mapId)}
						{@const overrides = form.overrides
							.map((row, index) => ({ row, index }))
							.filter(({ row }) => row.mapId === mapId)}
						<tr class="border-t border-secondary-600 dark:border-space-800">
							<td class="pr-2 py-1">
								<label class="flex items-center gap-1.5" class:text-yellow-300={notInBuild}>
									<input
										type="checkbox"
										checked
										disabled={locked}
										on:change={() => {
											toggleMap(mapId, false);
										}}
									/>
									<span class="truncate" title={mapId}>{mapId}</span>
									{#if shortSlugs[mapId]}<span class="text-gray-500"
											>(Fleets …-{shortSlugs[mapId]})</span
										>{/if}
								</label>
								{#if notInBuild}
									<span class="text-yellow-300"
										>not in build {shortSha(buildMaps?.version)}: a release of a build without it
										fails</span
									>
								{/if}
							</td>
							<td
								colspan="3"
								class="px-1 text-gray-500 italic"
								title="The API stores one warm buffer for the shard, not per map"
								>same for every map</td
							>
							{#if form.liveMaps[mapId]}
								<SizingCells
									bind:form={form.liveMaps[mapId]}
									inherited={liveInherited(form.liveMaps[mapId], [form.liveDefault], liveDefault)}
									issues={mapIssues}
									disabled={locked}
									loaded={loadedForm.liveMaps[mapId] ?? null}
									title={livePreviewText(savedSpec, built.spec, mapId, clusters, liveDefault)}
								/>
							{/if}
							<td class="px-2 whitespace-nowrap">
								<button
									class="text-gray-400 hover:text-white"
									on:click={() => {
										openMaps = { ...openMaps, [mapId]: !openMaps[mapId] };
									}}
									>Per cluster{#if overrides.length}&nbsp;({overrides.length}){/if}
									{openMaps[mapId] ? '▾' : '▸'}</button
								>
							</td>
						</tr>
						{#if mapIssues.length}
							<tr
								><td /><td colspan="7" class="pb-1 text-red-400"
									>Live {mapIssues.map(sizingIssueText).join(' · ')}</td
								></tr
							>
						{/if}
						{#if openMaps[mapId]}
							{#each overrides as { row, index } (index)}
								{@const rowIssues = overrideRowIssues(row, tickedOverrides, limit)}
								<tr class="text-gray-400">
									<td class="pl-6 py-1">
										in
										<select
											class={OVERRIDE_SELECT}
											bind:value={form.overrides[index].cluster}
											disabled={locked}
										>
											{#each clusters.includes(row.cluster) ? clusters : [row.cluster, ...clusters] as c}
												<option value={c}>{c}</option>
											{/each}
										</select>
									</td>
									<td colspan="3" class="px-1">
										<label
											class="flex items-center gap-1"
											title="Scale this Fleet to zero, with no autoscaler, in every phase"
										>
											<input
												type="checkbox"
												bind:checked={form.overrides[index].form.parked}
												disabled={locked}
											/>
											Parked: no servers for this map in this cluster
										</label>
									</td>
									<SizingCells
										bind:form={form.overrides[index].form}
										inherited={liveInherited(row.form, [mapForm, form.liveDefault], liveDefault)}
										issues={rowIssues}
										disabled={locked}
									/>
									<td class="px-2">
										<button
											class="text-red-400 hover:underline disabled:opacity-50"
											disabled={locked}
											on:click={() => {
												removeOverride(index);
											}}>Remove</button
										>
									</td>
								</tr>
								{#if rowIssues.length}
									<tr
										><td /><td colspan="7" class="pb-1 text-red-400"
											>{rowIssues.map(sizingIssueText).join(' · ')}</td
										></tr
									>
								{/if}
							{/each}
							<tr>
								<td class="pl-6 pb-1" colspan="8">
									<button
										class="text-primary-400 hover:underline disabled:opacity-50"
										disabled={locked || !clusters.length}
										on:click={() => {
											addOverride(mapId);
										}}>Add a cluster override</button
									>
								</td>
							</tr>
						{/if}
					{:else}
						<tr>
							<td colspan="8" class="py-1 {enabled ? 'text-red-400' : 'text-gray-500'}"
								>No maps switched on. Fleets need at least one; there is no default map.</td
							>
						</tr>
					{/each}

					{#if view === 'all'}
						{#each untickedInBuild as mapId (mapId)}
							{@const problem = fleetMapProblem(mapId, form.maps)}
							<tr class="border-t border-secondary-600 dark:border-space-800 text-gray-500">
								<td class="pr-2 py-1" colspan="8">
									<label class="flex items-center gap-1.5" title={problem}>
										<input
											type="checkbox"
											disabled={locked || !!problem}
											on:change={() => {
												toggleMap(mapId, true);
											}}
										/>
										{mapId}{#if dropped.includes(mapId)}<span class="text-yellow-300"
												>&nbsp;· unticked: its live sizing and overrides are dropped when you save</span
											>{/if}
									</label>
								</td>
							</tr>
						{/each}
					{/if}
				</tbody>
			</table>
		</div>

		{#if dropped.length && view === 'fleets'}
			<p class="text-xs text-yellow-300">
				Unticked, so {dropped.length === 1 ? 'its' : 'their'} live sizing and overrides are dropped when
				you save: {dropped.join(', ')}
			</p>
		{/if}

		{#if !readOnly}
			<div class="flex items-center gap-2 text-xs">
				<input
					class="w-48 rounded p-1 text-xs bg-secondary-600 dark:bg-space-800 text-white border border-gray-500 disabled:opacity-50"
					class:border-red-500={!!newMapError}
					bind:value={newMap}
					placeholder="Map ID, e.g. Arena"
					disabled={locked}
					on:keydown={(e) => {
						if (e.key === 'Enter') addMap();
					}}
				/>
				<Button
					size="xs"
					color="alternative"
					disabled={locked || !newMapTrimmed || !!newMapError}
					on:click={addMap}>Add a map</Button
				>
				{#if newMapError}<span class="text-red-400">{newMapError}</span>
				{:else if shortSlugs[newMapTrimmed]}<span class="text-gray-500"
						>its Fleet names end in -{shortSlugs[newMapTrimmed]}</span
					>{/if}
			</div>
		{/if}

		<p class="text-xs text-gray-400">
			Grey italic values are inherited (from Defaults, the coordinator's live default, or the
			shard's limit); type in a cell to override it for that map, clear it to inherit again. Hover a
			map's live cells for its sizing in each cluster, now and after saving.
		</p>

		<div
			class="text-sm rounded p-2 {gateCannotPass
				? 'bg-yellow-900/20 text-yellow-300'
				: 'bg-secondary-800 dark:bg-space-950 text-gray-300'}"
		>
			Gate before a promotion publishes: {fleets.gate.minReady ?? 1} Ready server{(fleets.gate
				.minReady ?? 1) === 1
				? ''
				: 's'} stable for {formatSpan(gateStableForMs(fleets))}, giving up after {formatSpan(
				gateTimeoutMs(fleets)
			)}. Set in the shard's ShardConfig (infra repo).{#if gateCannotPass}&nbsp;It can never pass:
				the timeout is shorter than the stable time.{/if}
		</div>

		{#if stale}
			<div class="text-yellow-400 text-sm bg-yellow-900/20 p-2 rounded">
				Someone changed these settings since you started editing{#if changedUnderneath.length}
					&nbsp;({changedUnderneath.join(', ')}){/if}, so this save would be refused. Reset to see
				theirs, then edit again.
			</div>
		{/if}
		{#if saveError}
			<div class="text-red-400 text-sm bg-red-900/20 p-2 rounded">Not saved: {saveError}</div>
		{/if}
		<div class="flex items-center gap-2 flex-wrap" class:hidden={readOnly}>
			{#if enabled}
				<ReleaseGate let:disabled>
					<Button
						size="xs"
						disabled={disabled || saving || !dirty || stale || problems > 0}
						on:click={() => save(built.spec, `Saved the Fleet settings of ${detail.displayName}`)}
					>
						{#if saving}<Spinner size="3" class="mr-1" />{/if}Save
					</Button>
				</ReleaseGate>
				<Button
					size="xs"
					color="alternative"
					disabled={saving || !dirty}
					on:click={() => {
						loadForm(savedSpec, detail.generation);
					}}>Reset</Button
				>
				<ReleaseGate let:disabled>
					<Button
						size="xs"
						color="alternative"
						disabled={disabled || saving || dirty}
						on:click={() => {
							showToggleConfirm = true;
						}}>Turn Fleets off…</Button
					>
				</ReleaseGate>
				{#if dirty}<span class="text-xs text-gray-400"
						>Save or reset your changes to turn Fleets off.</span
					>{/if}
			{:else}
				<ReleaseGate let:disabled>
					<Button
						size="xs"
						color="green"
						disabled={disabled || saving || stale || problems > 0 || missing.length > 0}
						on:click={() => {
							showToggleConfirm = true;
						}}
					>
						{#if saving}<Spinner size="3" class="mr-1" />{/if}Save and turn Fleets on
					</Button>
				</ReleaseGate>
				<ReleaseGate let:disabled>
					<Button
						size="xs"
						color="alternative"
						disabled={disabled || saving || !dirty || stale || problems > 0}
						on:click={() => save(built.spec, `Saved the Fleet settings of ${detail.displayName}`)}
						>Save only</Button
					>
				</ReleaseGate>
				<Button
					size="xs"
					color="alternative"
					disabled={saving || !dirty}
					on:click={() => {
						loadForm(savedSpec, detail.generation);
					}}>Reset</Button
				>
				{#if missing.length}
					<span class="text-xs text-gray-400">Still needed: {missing.join(' and ')}.</span>
				{/if}
			{/if}
			{#if problems > 0}
				<span class="text-xs text-red-400"
					>Fix the {problems} problem{problems === 1 ? '' : 's'} above to save.</span
				>
			{/if}
		</div>
	{/if}
</section>

<ConfirmModal
	bind:showModal={showToggleConfirm}
	title={toggleText(enabled)}
	resultCallback={onToggleConfirmed}
/>
