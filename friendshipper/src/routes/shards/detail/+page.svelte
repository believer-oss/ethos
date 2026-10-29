<script lang="ts">
	// One shard: what it serves, and its promotions (running first, then recent). Reached as
	// /shards/detail?name=<shard>: the app is prerendered with no fallback page, so a dynamic
	// /shards/[name] route can't be built.
	import { onDestroy, onMount } from 'svelte';
	import { Button, Card, Spinner } from 'flowbite-svelte';
	import {
		ArrowLeftOutline,
		ExclamationCircleOutline,
		RefreshOutline
	} from 'flowbite-svelte-icons';
	import { emit } from '@tauri-apps/api/event';
	import { page } from '$app/stores';
	import {
		cancelPromotion,
		fleetRevision,
		getShard,
		promotionViaCoordinator,
		refreshFleets,
		watchFleets
	} from '$lib/fleets';
	import {
		backendParts,
		backendWarning,
		channelSinceText,
		channelKey,
		channelLabel,
		formatAge,
		isActivePromotion,
		observationWarning,
		promotionChangedBy,
		promotionNote,
		isCancelling,
		promotionPhaseLabel,
		retryingMessage,
		promotionSubmittedAt,
		promotionWorkflow,
		shortSha
	} from '$lib/fleetFormat';
	import type {
		ChannelRef,
		ChannelStatus,
		GameConfig,
		Nullable,
		Promotion,
		ShardDetail
	} from '$lib/types';
	import ConfirmModal from '$lib/components/ConfirmModal.svelte';
	import WorkflowLink from '$lib/components/fleets/WorkflowLink.svelte';
	import ReleaseGate from '$lib/components/fleets/ReleaseGate.svelte';

	let detail: Nullable<ShardDetail> = null;
	let error = '';
	let loading = false;
	let now = Date.now();
	let cancelTarget: Nullable<Promotion> = null;
	let showCancelConfirm = false;
	let cancelling = '';

	const clock = setInterval(() => {
		now = Date.now();
	}, 30_000);
	onDestroy(() => {
		clearInterval(clock);
	});

	// With the switch off, the poller runs only while a Shards page is open.
	onMount(watchFleets);

	// Bumped per fetch, so a slow older response can't overwrite a newer one.
	let request = 0;

	$: name = $page.url.searchParams.get('name') ?? '';

	// The spinner is for manual refreshes; background polls update in place.
	const load = async (shard: string, manual = false) => {
		if (!shard) return;
		request += 1;
		const mine = request;
		loading = manual || !detail;
		try {
			const fetched = await getShard(shard);
			if (mine !== request) return;
			detail = fetched;
			error = '';
		} catch (e) {
			if (mine !== request) return;
			error = (e as { message?: string })?.message ?? String(e);
		} finally {
			if (mine === request) {
				loading = false;
				now = Date.now();
			}
		}
	};

	// Refetch when the name changes and on every poll of the shared poller, which runs fast while a
	// promotion is active.
	const refetch = (shard: string, _revision: number) => {
		void load(shard);
	};
	$: refetch(name, $fleetRevision);

	const phaseClass = (p: Promotion): string => {
		switch (p.status?.phase) {
			case 'Succeeded':
				return 'text-green-400';
			case 'Failed':
			case 'Rejected':
				return 'text-red-400';
			case 'Cancelled':
				return 'text-gray-400';
			default:
				return 'text-primary-400';
		}
	};

	interface ChannelRow {
		ref: ChannelRef;
		observed: ChannelStatus | undefined;
		configuredGameConfig: GameConfig | undefined;
	}

	// Configured channels first, then any observed channel no longer configured, so a Shard whose
	// ShardConfig was removed still shows what it serves, as the list page does.
	const channelRows = (d: ShardDetail): ChannelRow[] => {
		const observed = d.status?.channels ?? [];
		const rows: ChannelRow[] = (d.config?.channels ?? []).map((c) => ({
			ref: c,
			observed: observed.find((o) => o.key === channelKey(c)),
			configuredGameConfig: c.gameConfig
		}));
		observed
			.filter((o) => !rows.some((r) => channelKey(r.ref) === o.key))
			.forEach((o) => {
				rows.push({ ref: o, observed: o, configuredGameConfig: undefined });
			});
		return rows;
	};

	const askCancel = (p: Promotion) => {
		cancelTarget = p;
		showCancelConfirm = true;
	};

	const onCancelConfirmed = async (confirmed: boolean) => {
		const target = cancelTarget?.metadata.name;
		cancelTarget = null;
		if (!confirmed || !target) return;
		cancelling = target;
		try {
			await cancelPromotion(target);
			await emit('success', `Cancel requested for ${target}`);
			await refreshFleets();
		} catch (e) {
			await emit('error', e);
		} finally {
			cancelling = '';
		}
	};
</script>

<div class="flex items-center gap-2">
	<a href="/shards" class="text-gray-400 hover:text-white"><ArrowLeftOutline class="w-5 h-5" /></a>
	<p class="text-2xl my-2 text-primary-400 dark:text-primary-400">
		{detail?.displayName ?? name}
	</p>
	{#if detail && detail.displayName !== detail.name}
		<code class="text-gray-400">{detail.name}</code>
	{/if}
	<Button disabled={loading} class="!p-1.5" primary on:click={() => load(name, true)}>
		{#if loading}
			<Spinner size="4" />
		{:else}
			<RefreshOutline class="w-4 h-4" />
		{/if}
	</Button>
</div>

<Card
	class="w-full p-4 sm:p-4 max-w-full bg-secondary-700 dark:bg-space-900 h-full border-0 shadow-none overflow-auto flex flex-col gap-6"
>
	{#if error}
		<p class="text-red-400">{error}</p>
	{/if}
	{#if detail}
		{@const backend = detail.status?.backend}
		{@const backendIssue = detail.status
			? backendWarning(backend, detail.promotions, detail.name, now)
			: null}
		<section class="flex flex-col gap-2">
			<h2 class="text-lg text-white flex items-center gap-2">
				Backend
				{#if backend && !backend.healthy}
					<span class="text-xs text-red-400">unhealthy</span>
				{/if}
				{#if backendIssue}
					<span title={backendIssue}
						><ExclamationCircleOutline class="w-4 h-4 text-yellow-400" /></span
					>
				{/if}
			</h2>
			{#if backendParts(backend?.parts).length === 0}
				<p class="text-gray-500 text-sm">Not observed yet.</p>
			{:else}
				<table class="text-sm text-gray-300 w-fit">
					{#each backendParts(backend?.parts) as part (part.name)}
						<tr>
							<td class="pr-4 text-gray-400">{part.name}</td>
							{#if part.version}
								<td class="pr-4"><code>{part.version.sha}</code></td>
								<td class="pr-4">{part.version.branch}</td>
								<td class="pr-4 text-gray-400">{part.version.release}</td>
								<td class="text-gray-400">
									{#if part.version.builtAt}built {formatAge(part.version.builtAt, now)} ago{/if}
								</td>
							{:else}
								<td colspan="4"><code>{part.raw}</code></td>
							{/if}
						</tr>
					{/each}
				</table>
			{/if}
		</section>

		<section class="flex flex-col gap-2">
			<h2 class="text-lg text-white">Channels</h2>
			<table class="text-sm text-gray-300 w-fit">
				{#each channelRows(detail) as row (channelKey(row.ref))}
					{@const { observed } = row}
					{@const warning = observed ? observationWarning(observed, now) : 'Not observed yet'}
					<tr>
						<td class="pr-4 text-gray-400">{channelLabel(row.ref)}</td>
						<td class="pr-4">
							{#if observed?.version}
								<code title={observed.version}>{shortSha(observed.version)}</code>
							{:else if observed?.buildId}
								build {observed.buildId}
							{:else}
								<span class="text-gray-500">no build</span>
							{/if}
							{#if observed?.source === 'promotion'}
								<span
									class="text-gray-500 text-xs"
									title="set from the last promotion; not yet confirmed by observing the channel"
									>◇</span
								>
							{/if}
						</td>
						<td class="pr-4 text-gray-400">
							{#if observed}{channelSinceText(observed, now)}{/if}
						</td>
						<td class="pr-4 text-gray-400">
							<!-- Only Steam reports a game config; the configured one is the promotion default. -->
							{#if observed?.gameConfig}
								{observed.gameConfig}
							{:else if row.configuredGameConfig}
								<span title="the channel's default for promotions, not observed"
									>default {row.configuredGameConfig}</span
								>
							{/if}
						</td>
						<td>
							{#if warning}
								<span title={warning}
									><ExclamationCircleOutline class="w-4 h-4 text-yellow-400" /></span
								>
							{/if}
						</td>
					</tr>
				{/each}
			</table>
		</section>

		<section class="flex flex-col gap-2">
			<h2 class="text-lg text-white">Promotions</h2>
			{#if detail.promotions.length === 0}
				<p class="text-gray-500 text-sm">No promotions through the coordinator yet.</p>
			{/if}
			{#each detail.promotions as promotion (promotion.metadata.name)}
				{@const plan = promotion.status?.plan?.backend}
				{@const note = promotionNote(promotion)}
				<div
					class="rounded-md border border-secondary-600 dark:border-space-800 p-3 text-sm text-gray-300 flex flex-col gap-1"
				>
					<div class="flex items-center gap-3 flex-wrap">
						<span class="font-semibold {phaseClass(promotion)}"
							>{promotionPhaseLabel(promotion)}</span
						>
						<span>{channelLabel(promotion.spec.channel)}</span>
						<code title={promotion.spec.version}>{shortSha(promotion.spec.version)}</code>
						<span class="text-gray-400">
							{promotionChangedBy(promotion)} · submitted {formatAge(
								promotionSubmittedAt(promotion),
								now
							)} ago
						</span>
						<span class="ml-auto flex items-center gap-3">
							<WorkflowLink workflow={promotionWorkflow(promotion)} label="Argo workflow" />
							{#if isActivePromotion(promotion) && !isCancelling(promotion) && $promotionViaCoordinator}
								<ReleaseGate let:disabled>
									<Button
										size="xs"
										color="red"
										disabled={disabled || cancelling === promotion.metadata.name}
										on:click={() => {
											askCancel(promotion);
										}}
									>
										Cancel
									</Button>
								</ReleaseGate>
							{/if}
						</span>
					</div>
					{#if retryingMessage(promotion)}
						<div class="text-yellow-400">Retrying: {retryingMessage(promotion)}</div>
					{:else if promotion.status?.reason || promotion.status?.message}
						<div class="text-gray-400">
							{#if promotion.status.reason}<code>{promotion.status.reason}</code>{/if}
							{promotion.status.message}
						</div>
					{/if}
					{#if note}
						<div class="text-gray-400 italic">“{note}”</div>
					{/if}
					{#if plan}
						<div class="text-gray-400">
							<!-- Recorded at acceptance: `from` is the shard's backend as read then, not now. -->
							Backend plan at acceptance ({plan.deploy
								? 'deploy'
								: 'no deploy'}{#if plan.observedAt}, backend read {formatAge(plan.observedAt, now)} ago{/if}):
							{#each plan.parts.filter((p) => p.from !== p.to) as change (change.name)}
								<span
									class="mr-3"
									title={`${change.from || 'unknown'} (shard's backend at acceptance) → ${
										change.to
									} (build)`}
								>
									{change.name}
									<code>{change.from.split('_').pop()}</code> →
									<code>{change.to.split('_').pop()}</code>
								</span>
							{:else}
								<span>no part changes</span>
							{/each}
						</div>
					{/if}
					{#if promotion.status?.gameConfig}
						<div class="text-gray-500 text-xs">Game config {promotion.status.gameConfig}</div>
					{/if}
					{#if promotion.status?.previousVersion}
						<div class="text-gray-500 text-xs">
							Channel version at acceptance <code title={promotion.status.previousVersion}
								>{shortSha(promotion.status.previousVersion)}</code
							>
						</div>
					{/if}
				</div>
			{/each}
		</section>
	{:else if loading}
		<p class="text-gray-400">Loading…</p>
	{/if}
</Card>

<ConfirmModal
	bind:showModal={showCancelConfirm}
	title={`Cancel promotion ${
		cancelTarget?.metadata.name ?? ''
	}? If its workflow is running it will be stopped; if it has already finished, the result stands.`}
	resultCallback={onCancelConfirmed}
/>
