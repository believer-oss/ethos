<script lang="ts">
	// The promote form when promotionViaCoordinator is on: creates a Promotion for the fleet
	// coordinator. Targets come from ShardConfig channels; progress shows in the bottom-bar tray,
	// on the shard page and as toasts.
	//
	// The form works from a snapshot taken when it opens, so a poll can't change the target under
	// the user. Promote opens a confirmation that fetches the shard fresh and shows the channel's
	// current state next to the requested one; only Confirm creates the Promotion.
	import { onMount } from 'svelte';
	import { get } from 'svelte/store';
	import { Button, Checkbox, Input, Label, Select, Spinner } from 'flowbite-svelte';
	import { RefreshOutline } from 'flowbite-svelte-icons';
	import { emit } from '@tauri-apps/api/event';
	import {
		createPromotion,
		fleetError,
		fleetPromotions,
		fleetReleases,
		fleetShards,
		getPromotionPlan,
		getShard,
		refreshFleets
	} from '$lib/fleets';
	import { workflows } from '$lib/stores';
	import {
		busyPromotion,
		channelKey,
		channelLabel,
		channelSinceText,
		gatePlan,
		mapPreflight,
		promotionPreview,
		shortSha,
		type PromotionPreview
	} from '$lib/fleetFormat';
	import type {
		FleetRelease,
		GameConfig,
		Nullable,
		Promotion,
		PromotionPlan,
		PromotionSpec,
		ShardChannel,
		ShardDetail,
		ShardSummary
	} from '$lib/types';
	import {
		BACKEND_UNAVAILABLE,
		backendDiff,
		buildLine,
		deployBackendDefault,
		olderParts,
		releasePlan,
		rollbackOf,
		rollbackText,
		stepsLine,
		type PartDirection
	} from '$lib/promotePlan';
	import ReleaseGate from './ReleaseGate.svelte';

	export let commit: string;
	export let close: () => void;
	// Opened for one channel, e.g. from the shard page's Roll back…: picked when the form opens.
	export let presetShard = '';
	export let presetChannel = '';

	// After a backend part on the confirmation: which way deploying it moves the part.
	const DIRECTION_TEXT: Record<PartDirection, string> = {
		older: ' ↓ older',
		newer: ' ↑ newer',
		same: '',
		unknown: ''
	};

	const FULL_SHA = /^[0-9a-f]{40}$/;
	const ALL_CONFIGS: GameConfig[] = ['Development', 'Test', 'Shipping'];
	const STEAM_CONFIGS: GameConfig[] = ['Test', 'Shipping'];

	const errorText = (e: unknown): string => (e as { message?: string })?.message ?? String(e);

	// The snapshot the form works from.
	let snapshotLoading = true;
	let snapshotError = '';
	let shards: ShardSummary[] = [];
	let promotions: Promotion[] = [];
	let releases: FleetRelease[] = [];
	let snapshotAt = Date.now();

	let shardName = '';
	let channelValue = '';
	let gameConfig: GameConfig = 'Development';
	let deployBackend = true;
	let sign = true;
	// spec.sign stays unset unless the user changes the checkbox, so the template default applies.
	let signTouched = false;
	let note = '';
	let error = '';
	let skipGate = false;
	let skipReason = '';
	// Deploy backend follows the default (off for a rollback) until the user changes it.
	let deployTouched = false;

	// The confirmation step.
	let confirming: Nullable<PromotionSpec> = null;
	let preview: Nullable<PromotionPreview> = null;
	let previewLoading = false;
	let previewError = '';
	let submitError = '';
	let submitting = false;
	let previewRequest = 0;
	// The shard as fetched for the confirmation, for its map pre-flight.
	let previewDetail: Nullable<ShardDetail> = null;

	$: shard = shards.find((s) => s.name === shardName);
	$: config = shard?.config;
	$: channels = config?.channels ?? [];
	$: channel = channels.find((c) => channelKey(c) === channelValue);
	$: isSteam = channel?.kind === 'steam';
	$: allowDeployBackend = config?.promotion?.allowDeployBackend === true;
	$: configItems = (isSteam ? STEAM_CONFIGS : ALL_CONFIGS).map((c) => ({ value: c, name: c }));
	$: version = commit.trim().toLowerCase();
	$: gate = gatePlan(shard, skipGate);
	$: hasFleets = !!shard?.config?.fleets && shard.spec?.fleetsEnabled === true;
	$: current = shard?.status?.channels.find((o) => o.key === channelValue)?.version;
	$: rollback =
		shard && channel ? rollbackOf(promotions, shard.name, channel, version, current) : null;
	// The build's manifest next to the shard's backend; null while loading, or when unreadable.
	// Promote waits while it loads, since Deploy backend's default depends on it.
	let promotionPlan: Nullable<PromotionPlan> = null;
	let planLoading = false;
	let planRequest = 0;
	const loadPlan = async (forShard: string, forVersion: string) => {
		planRequest += 1;
		const mine = planRequest;
		promotionPlan = null;
		planLoading = false;
		if (!forShard || !FULL_SHA.test(forVersion)) return;
		planLoading = true;
		try {
			const fetched = await getPromotionPlan(forShard, forVersion);
			if (mine === planRequest) promotionPlan = fetched;
		} catch {
			// Shown as no manifest: Deploy backend then starts off, with the comparison unavailable.
		} finally {
			if (mine === planRequest) planLoading = false;
		}
	};
	$: void loadPlan(shardName, version);
	$: manifestMapIds = promotionPlan?.manifest?.mapIds ?? [];
	$: preflight = shard
		? mapPreflight(
				{
					...shard,
					promotions: [],
					releases: releases.filter((r) => r.spec.shard === shard?.name)
				},
				version,
				manifestMapIds
		  )
		: [];
	$: parts = backendDiff(promotionPlan, promotions, snapshotAt);
	$: older = olderParts(parts);
	// A different build has its own default: an older one starts with Deploy backend off again,
	// whatever the user picked for the previous build.
	let defaultsFor = '';
	$: if (version !== defaultsFor) {
		defaultsFor = version;
		deployTouched = false;
	}
	$: if (!deployTouched) deployBackend = deployBackendDefault(promotionPlan, !!rollback, older);
	$: noManifest = !planLoading && FULL_SHA.test(version) && !promotionPlan?.manifest;
	$: built = buildLine(
		$workflows.find((w) => w.commit === version),
		snapshotAt
	);
	$: buildRelease = releases.find(
		(r) => r.spec.shard === shardName && r.spec.version === version && !r.metadata.deletionTimestamp
	);
	$: plan = releasePlan(buildRelease, config?.fleets, snapshotAt);
	$: steps = stepsLine(allowDeployBackend && deployBackend, hasFleets ? { plan, skipGate } : null);

	const channelOption = (s: ShardSummary, c: ShardChannel) => {
		const key = channelKey(c);
		const observed = s.status?.channels.find((o) => o.key === key);
		const since = observed ? channelSinceText(observed, snapshotAt) : '';
		let serves = 'nothing known';
		if (observed?.version) {
			serves = since ? `${shortSha(observed.version)}, ${since}` : shortSha(observed.version);
		}
		const busy = busyPromotion(promotions, s.name, c)?.metadata.name;
		return {
			value: key,
			name: `${channelLabel(c)} (serves ${serves})${busy ? ` - busy: ${busy}` : ''}`,
			disabled: !!busy
		};
	};

	// The per-channel defaults, set only in the pickers' change handlers, never reactively, so
	// nothing but the user changes them.
	const resetChoices = (picked: ShardChannel | undefined) => {
		gameConfig = picked?.gameConfig ?? 'Development';
		deployBackend = true;
		deployTouched = false;
		sign = true;
		signTouched = false;
		skipGate = false;
		skipReason = '';
	};

	const onShardChange = () => {
		channelValue = '';
		resetChoices(undefined);
	};

	// `channel` is reactive and not updated yet inside the handler, so look the pick up here.
	const onChannelChange = () => {
		resetChoices(channels.find((c) => channelKey(c) === channelValue));
	};

	onMount(async () => {
		await refreshFleets();
		shards = get(fleetShards).filter((s) => s.config);
		promotions = get(fleetPromotions);
		releases = get(fleetReleases);
		snapshotError = get(fleetError);
		snapshotAt = Date.now();
		if (shards.length === 1) shardName = shards[0].name;
		if (presetShard && shards.some((s) => s.name === presetShard)) {
			shardName = presetShard;
			const preset = shards
				.find((s) => s.name === presetShard)
				?.config?.channels.find((c) => channelKey(c) === presetChannel);
			if (preset) {
				channelValue = presetChannel;
				resetChoices(preset);
			}
		}
		snapshotLoading = false;
	});

	const loadPreview = async () => {
		if (!confirming) return;
		const spec = confirming;
		previewRequest += 1;
		const request = previewRequest;
		previewLoading = true;
		previewError = '';
		try {
			const detail = await getShard(spec.shard);
			if (request !== previewRequest) return;
			preview = promotionPreview(detail, spec, Date.now());
			previewDetail = detail;
		} catch (e) {
			if (request !== previewRequest) return;
			preview = null;
			previewError = errorText(e);
		} finally {
			if (request === previewRequest) previewLoading = false;
		}
	};

	const target = (spec: PromotionSpec): string =>
		`${shard?.displayName ?? spec.shard} ${channelLabel(spec.channel)}`;

	const review = async () => {
		error = '';
		if (!channel) {
			error = 'Pick a channel.';
			return;
		}
		if (!FULL_SHA.test(version)) {
			error = 'The build must be a full 40-character commit SHA.';
			return;
		}
		if (hasFleets && skipGate && !skipReason.trim()) {
			error = 'Say why the warm gate is skipped.';
			return;
		}
		if (hasFleets && plan.blocks && !skipGate) {
			error = plan.blocks;
			return;
		}
		const spec: PromotionSpec = {
			shard: shardName,
			channel: isSteam
				? { kind: 'steam', branch: channel.branch }
				: { kind: 'launcher', metadataPath: channel.metadataPath },
			version,
			gameConfig,
			deployBackend: allowDeployBackend && deployBackend
		};
		if (isSteam && signTouched) spec.sign = sign;
		if (hasFleets && skipGate) spec.skipFleetGate = { reason: skipReason.trim() };
		confirming = spec;
		preview = null;
		submitError = '';
		await loadPreview();
	};

	const back = () => {
		previewRequest += 1;
		confirming = null;
		preview = null;
		previewDetail = null;
		previewLoading = false;
		previewError = '';
		submitError = '';
	};

	const confirm = async () => {
		if (!confirming) return;
		const spec = confirming;
		submitting = true;
		submitError = '';
		try {
			await createPromotion({ spec, note: note.trim() || undefined });
		} catch (e) {
			// e.g. ChannelBusy from a race: keep the confirmation open and show the latest state.
			submitError = errorText(e);
			submitting = false;
			await loadPreview();
			return;
		}
		submitting = false;
		const message = `Promotion started: ${target(spec)}`;
		close();
		await emit('fleet-success', { message, shard: spec.shard });
		await refreshFleets();
	};
</script>

{#if confirming}
	<div class="flex flex-col gap-4">
		<div class="flex items-center gap-2 text-white">
			<span>Promote to <strong>{target(confirming)}</strong>?</span>
			<Button
				disabled={previewLoading || submitting}
				class="!p-1.5 ml-auto"
				primary
				title="Fetch the channel's state again"
				on:click={loadPreview}
			>
				{#if previewLoading}
					<Spinner size="4" />
				{:else}
					<RefreshOutline class="w-4 h-4" />
				{/if}
			</Button>
		</div>

		{#if rollback}<p class="text-sm text-yellow-300">{rollbackText(rollback, snapshotAt)}</p>{/if}
		<p class="text-xs text-gray-400">{steps}</p>

		{#if confirming.deployBackend && parts.some((p) => p.direction !== 'same')}
			<div class="flex flex-col gap-0.5 text-xs text-gray-300">
				<span class="text-gray-400">Backend</span>
				{#each parts.filter((p) => p.direction !== 'same') as part (part.name)}
					<span class:text-yellow-300={part.direction === 'older'}
						>{part.name} · {part.shard} · {part.build}{DIRECTION_TEXT[part.direction]}</span
					>
				{/each}
			</div>
		{/if}
		{#if preview}
			<table class="text-sm text-gray-300 w-full">
				<thead class="text-xs uppercase text-gray-400">
					<tr>
						<th />
						<th class="text-left py-1 pr-4">Current</th>
						<th class="text-left py-1">New</th>
					</tr>
				</thead>
				<tbody>
					{#each preview.rows as row (row.label)}
						<tr class="border-t border-secondary-600 dark:border-space-800">
							<td class="py-1 pr-4 text-gray-400" title={row.labelTitle}>{row.label}</td>
							<td class="py-1 pr-4"><code>{row.current}</code></td>
							<td class="py-1 text-white"><code>{row.requested}</code></td>
						</tr>
					{/each}
				</tbody>
			</table>
		{:else if previewLoading}
			<p class="text-gray-400 text-sm">Fetching the channel's current state…</p>
		{/if}

		{#if previewDetail}
			{@const checks = mapPreflight(
				previewDetail,
				confirming.version,
				confirming.version === version ? manifestMapIds : []
			)}
			{#if checks.length}
				<div class="text-sm text-gray-300 flex flex-col gap-0.5">
					<span class="text-gray-400">Fleet maps</span>
					{#each checks as check (check.mapId)}
						<span class:text-yellow-400={!check.ok}
							>{check.ok ? '✓' : '!'} {check.mapId}: {check.text}</span
						>
					{/each}
				</div>
			{/if}
		{/if}

		{#if note.trim()}
			<div class="text-sm text-gray-300">
				<span class="text-gray-400">Note:</span> “{note.trim()}”
			</div>
		{/if}

		{#if previewError}
			<div class="text-red-400 text-sm bg-red-900/20 p-2 rounded">
				Couldn't fetch the shard: {previewError}
			</div>
		{:else if preview?.blocked}
			<div class="text-yellow-400 text-sm bg-yellow-900/20 p-2 rounded">{preview.blocked}</div>
		{/if}
		{#if submitError}
			<div class="text-red-400 text-sm bg-red-900/20 p-2 rounded">
				Not started: {submitError}
			</div>
		{/if}

		<div class="flex flex-row-reverse gap-2 pt-2">
			<ReleaseGate let:disabled>
				<Button
					class="bg-lime-700 dark:bg-lime-700 hover:bg-lime-600 dark:hover:bg-lime-600"
					disabled={disabled ||
						submitting ||
						previewLoading ||
						!preview ||
						!!preview.blocked ||
						!!previewError}
					on:click={confirm}
				>
					{#if submitting}<Spinner size="4" class="mr-2" />{/if}
					Confirm
				</Button>
			</ReleaseGate>
			<Button
				class="bg-gray-700 dark:bg-gray-700 hover:bg-gray-600 dark:hover:bg-gray-600"
				disabled={submitting}
				on:click={back}
			>
				Back
			</Button>
		</div>
	</div>
{:else}
	<div class="flex flex-col gap-4">
		{#if snapshotLoading}
			<p class="text-gray-400">Loading shards…</p>
		{:else if shards.length === 0}
			{#if snapshotError}
				<div class="text-red-400 text-sm bg-red-900/20 p-2 rounded">
					Couldn't load shards: {snapshotError}
				</div>
			{:else}
				<p class="text-gray-300">No shards are configured for promotion.</p>
			{/if}
		{:else}
			{#if snapshotError}
				<div class="text-yellow-400 text-sm bg-yellow-900/20 p-2 rounded">
					Couldn't refresh shards, so this may be out of date: {snapshotError}
				</div>
			{/if}
			<div>
				<Label for="fleetShard" class="text-primary-400 mb-2">Shard</Label>
				<select
					id="fleetShard"
					bind:value={shardName}
					on:change={onShardChange}
					class="block w-full rounded-lg p-2.5 text-sm bg-secondary-600 dark:bg-space-800 text-white border-gray-500"
				>
					<option value="" disabled>Pick a shard</option>
					{#each shards as s (s.name)}
						<option value={s.name}>{s.displayName}</option>
					{/each}
				</select>
			</div>
			<div>
				<Label for="fleetChannel" class="text-primary-400 mb-2">Channel</Label>
				<select
					id="fleetChannel"
					bind:value={channelValue}
					on:change={onChannelChange}
					disabled={!shard}
					class="block w-full rounded-lg p-2.5 text-sm bg-secondary-600 dark:bg-space-800 text-white border-gray-500"
				>
					<option value="" disabled>Pick a channel</option>
					{#if shard}
						{#each channels as c (channelKey(c))}
							{@const item = channelOption(shard, c)}
							<option value={item.value} disabled={item.disabled}>{item.name}</option>
						{/each}
					{/if}
				</select>
			</div>
			<div>
				<Label for="fleetCommit" class="text-primary-400 mb-2">Build</Label>
				<Input
					id="fleetCommit"
					bind:value={commit}
					placeholder="40-character commit SHA"
					class="bg-secondary-600 dark:bg-space-800 text-white border-gray-500 font-mono"
				/>
				{#if built}<p class="text-xs text-gray-400 mt-1">{built}</p>{/if}
				{#if rollback}
					<p class="text-xs text-yellow-300 mt-2">{rollbackText(rollback, snapshotAt)}</p>
				{/if}
			</div>
			{#if channel}
				<div>
					<Label for="fleetGameConfig" class="text-primary-400 mb-2">Game config</Label>
					<Select
						id="fleetGameConfig"
						bind:value={gameConfig}
						items={configItems}
						class="bg-secondary-600 dark:bg-space-800 text-white border-gray-500"
					/>
				</div>
				<div class="flex flex-col gap-2">
					{#if allowDeployBackend}
						<Label class="flex items-center gap-2 text-xs text-white">
							<Checkbox
								bind:checked={deployBackend}
								on:change={() => {
									deployTouched = true;
								}}
							/>
							Deploy backend ({shardName})
						</Label>
						{#if noManifest}
							<span class="text-xs text-gray-400">{BACKEND_UNAVAILABLE}.</span>
						{/if}
						{#if deployBackend && older.length}
							<span class="text-xs text-yellow-300">
								Ticking this deploys older {older.join(', ')} to the whole shard, for every channel.
							</span>
						{:else if deployBackend && noManifest}
							<span class="text-xs text-yellow-300">
								Ticking this deploys this build's backend parts, whatever their age, to the whole
								shard, for every channel.
							</span>
						{:else if deployBackend && rollback}
							<span class="text-xs text-yellow-300">
								This is a rollback: ticking this deploys this build's backend, likely older, to the
								whole shard, for every channel.
							</span>
						{/if}
						{#if parts.length}
							<div class="flex flex-col gap-0.5 text-xs">
								{#each parts as part (part.name)}
									<span
										class:text-gray-500={part.direction === 'same'}
										class:text-yellow-300={part.direction === 'older'}
										class:text-gray-300={part.direction === 'newer' || part.direction === 'unknown'}
										title="Built times come from the version (UTC). A deploy time shows only where a promotion recorded the deploy."
									>
										{part.name} · {part.shard} · {part.build}
										{#if part.direction === 'older'}↓ older{:else if part.direction === 'newer'}↑
											newer{:else if part.direction === 'same'}unchanged{/if}
									</span>
								{/each}
							</div>
						{/if}
					{/if}
					{#if isSteam}
						<Label class="flex items-center gap-2 text-xs text-white">
							<Checkbox
								bind:checked={sign}
								on:change={() => {
									signTouched = true;
								}}
							/>
							Sign
						</Label>
					{/if}
				</div>
			{/if}
			{#if channel && shard?.config?.fleets}
				<div class="flex flex-col gap-2 text-xs text-gray-300">
					<span class="text-primary-400 text-sm">Fleets</span>
					<span class="text-gray-400">{gate.text}</span>
					{#if hasFleets}
						<span class:text-red-300={!!plan.blocks && !skipGate}>{plan.text}</span>
					{/if}
					{#each preflight as check (check.mapId)}
						<span class:text-yellow-400={!check.ok}
							>{check.ok ? '✓' : '!'} {check.mapId}: {check.text}</span
						>
					{/each}
					{#if hasFleets}
						<Label class="flex items-center gap-2 text-xs text-white">
							<Checkbox bind:checked={skipGate} />
							Skip the warm gate
						</Label>
						{#if skipGate}
							<Input
								bind:value={skipReason}
								maxlength={1024}
								placeholder="Why (required; shown with the promotion)"
								class="bg-secondary-600 dark:bg-space-800 text-white border-gray-500"
							/>
						{/if}
					{/if}
				</div>
			{/if}
			<div>
				<Label for="fleetNote" class="text-primary-400 mb-2">Note (optional)</Label>
				<Input
					id="fleetNote"
					bind:value={note}
					maxlength={200}
					placeholder="Why this build"
					class="bg-secondary-600 dark:bg-space-800 text-white border-gray-500"
				/>
			</div>
		{/if}

		{#if channel}<p class="text-xs text-gray-400">{steps}</p>{/if}
		{#if error}
			<div class="text-red-400 text-sm bg-red-900/20 p-2 rounded">{error}</div>
		{/if}

		<div class="flex flex-row-reverse gap-2 pt-2">
			<ReleaseGate let:disabled>
				<Button
					class="bg-lime-700 dark:bg-lime-700 hover:bg-lime-600 dark:hover:bg-lime-600"
					disabled={disabled || !channel || planLoading}
					title={planLoading ? "Reading the build's manifest…" : ''}
					on:click={review}
				>
					Promote
				</Button>
			</ReleaseGate>
			<Button
				class="bg-gray-700 dark:bg-gray-700 hover:bg-gray-600 dark:hover:bg-gray-600"
				on:click={close}
			>
				Cancel
			</Button>
		</div>
	</div>
{/if}
