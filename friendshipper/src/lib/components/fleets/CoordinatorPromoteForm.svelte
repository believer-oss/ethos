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
		fleetShards,
		getShard,
		refreshFleets
	} from '$lib/fleets';
	import {
		busyPromotion,
		channelKey,
		channelLabel,
		channelSinceText,
		promotionPreview,
		shortSha,
		type PromotionPreview
	} from '$lib/fleetFormat';
	import type {
		GameConfig,
		Nullable,
		Promotion,
		PromotionSpec,
		ShardChannel,
		ShardSummary
	} from '$lib/types';
	import ReleaseGate from './ReleaseGate.svelte';

	export let commit: string;
	export let close: () => void;

	const FULL_SHA = /^[0-9a-f]{40}$/;
	const ALL_CONFIGS: GameConfig[] = ['Development', 'Test', 'Shipping'];
	const STEAM_CONFIGS: GameConfig[] = ['Test', 'Shipping'];

	const errorText = (e: unknown): string => (e as { message?: string })?.message ?? String(e);

	// The snapshot the form works from.
	let snapshotLoading = true;
	let snapshotError = '';
	let shards: ShardSummary[] = [];
	let promotions: Promotion[] = [];
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

	// The confirmation step.
	let confirming: Nullable<PromotionSpec> = null;
	let preview: Nullable<PromotionPreview> = null;
	let previewLoading = false;
	let previewError = '';
	let submitError = '';
	let submitting = false;
	let previewRequest = 0;

	onMount(async () => {
		await refreshFleets();
		shards = get(fleetShards).filter((s) => s.config);
		promotions = get(fleetPromotions);
		snapshotError = get(fleetError);
		snapshotAt = Date.now();
		if (shards.length === 1) shardName = shards[0].name;
		snapshotLoading = false;
	});

	$: shard = shards.find((s) => s.name === shardName);
	$: config = shard?.config;
	$: channels = config?.channels ?? [];
	$: channel = channels.find((c) => channelKey(c) === channelValue);
	$: isSteam = channel?.kind === 'steam';
	$: allowDeployBackend = config?.promotion?.allowDeployBackend === true;
	$: configItems = (isSteam ? STEAM_CONFIGS : ALL_CONFIGS).map((c) => ({ value: c, name: c }));
	$: version = commit.trim().toLowerCase();

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
		sign = true;
		signTouched = false;
	};

	const onShardChange = () => {
		channelValue = '';
		resetChoices(undefined);
	};

	// `channel` is reactive and not updated yet inside the handler, so look the pick up here.
	const onChannelChange = () => {
		resetChoices(channels.find((c) => channelKey(c) === channelValue));
	};

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
		confirming = spec;
		preview = null;
		submitError = '';
		await loadPreview();
	};

	const back = () => {
		previewRequest += 1;
		confirming = null;
		preview = null;
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
							<td class="py-1 pr-4 text-gray-400">{row.label}</td>
							<td class="py-1 pr-4"><code>{row.current}</code></td>
							<td class="py-1 text-white"><code>{row.requested}</code></td>
						</tr>
					{/each}
				</tbody>
			</table>
		{:else if previewLoading}
			<p class="text-gray-400 text-sm">Fetching the channel's current state…</p>
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
							<Checkbox bind:checked={deployBackend} />
							Deploy backend ({shardName})
						</Label>
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

		{#if error}
			<div class="text-red-400 text-sm bg-red-900/20 p-2 rounded">{error}</div>
		{/if}

		<div class="flex flex-row-reverse gap-2 pt-2">
			<ReleaseGate let:disabled>
				<Button
					class="bg-lime-700 dark:bg-lime-700 hover:bg-lime-600 dark:hover:bg-lime-600"
					disabled={disabled || !channel}
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
