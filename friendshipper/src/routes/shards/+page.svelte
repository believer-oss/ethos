<script lang="ts">
	import { onDestroy, onMount } from 'svelte';
	import { Button, Card, Spinner } from 'flowbite-svelte';
	import { ExclamationCircleOutline, RefreshOutline } from 'flowbite-svelte-icons';
	import {
		fleetError,
		fleetPromotions,
		fleetShards,
		refreshFleets,
		watchFleets
	} from '$lib/fleets';
	import {
		backendParts,
		backendWarning,
		channelSinceText,
		promotionProgressText,
		formatAge,
		formatAgo,
		formatBackendPart,
		isActivePromotion,
		newestBuiltPart,
		observationWarning,
		shortSha,
		steamBuildText
	} from '$lib/fleetFormat';
	import { fleetsColumnText } from '$lib/fleetReleases';
	import { fleetsAttention } from '$lib/shardPage';
	import ShaLink from '$lib/components/fleets/ShaLink.svelte';
	import type { ChannelStatus, Promotion, ShardSummary } from '$lib/types';

	let now = Date.now();
	let refreshing = false;

	const clock = setInterval(() => {
		now = Date.now();
	}, 30_000);
	onDestroy(() => {
		clearInterval(clock);
	});

	const refresh = async () => {
		refreshing = true;
		try {
			await refreshFleets();
		} finally {
			refreshing = false;
			now = Date.now();
		}
	};

	// With the switch off, the poller runs only while a Shards page is open.
	onMount(() => {
		const release = watchFleets();
		void refresh();
		return release;
	});

	const channels = (shard: ShardSummary, kind: 'launcher' | 'steam'): ChannelStatus[] =>
		(shard.status?.channels ?? []).filter((c) => c.kind === kind);

	const runningFor = (shard: string, promotions: Promotion[]): Promotion[] =>
		promotions.filter((p) => p.spec.shard === shard && isActivePromotion(p));

	const backendHover = (shard: ShardSummary): string => {
		const backend = shard.status?.backend;
		const lines = backendParts(backend?.parts).map((part) =>
			part.version
				? `${part.name}: ${part.version.sha} · ${part.version.branch} · ${
						part.version.release
				  } · built ${formatAgo(part.version.builtAt, now)}`
				: `${part.name}: ${part.raw}`
		);
		if (backend?.error) lines.push(`Error: ${backend.error}`);
		return lines.join('\n');
	};

	// The age shown after a channel's SHA is its `since`; the hover says what that means.
	const channelHover = (channel: ChannelStatus): string =>
		[channel.version, channelSinceText(channel, now), channel.error && `Error: ${channel.error}`]
			.filter(Boolean)
			.join('\n');

	const steamHover = (channel: ChannelStatus): string => {
		const lines: string[] = [];
		if (channel.version) lines.push(channel.version);
		if (channel.buildId) lines.push(`Steam build ${channel.buildId}`);
		if (channel.gameConfig) lines.push(channel.gameConfig);
		const since = channelSinceText(channel, now);
		if (since) lines.push(since);
		if (channel.error) lines.push(`Error: ${channel.error}`);
		return lines.join('\n');
	};

	// Only when something needs attention: a release stuck in Failed (Warming and Retiring take
	// their time, so they don't warn here).
	const detailHref = (name: string) => `/shards/detail?name=${encodeURIComponent(name)}`;
</script>

<div class="flex items-center gap-2">
	<p class="text-2xl my-2 text-primary-400 dark:text-primary-400">Shards</p>
	<Button disabled={refreshing} class="!p-1.5" primary on:click={refresh}>
		{#if refreshing}
			<Spinner size="4" />
		{:else}
			<RefreshOutline class="w-4 h-4" />
		{/if}
	</Button>
</div>
<Card
	class="w-full p-4 sm:p-4 max-w-full bg-secondary-700 dark:bg-space-900 h-full border-0 shadow-none overflow-auto"
>
	{#if $fleetError}
		<p class="text-red-400 mb-2">Couldn't refresh shards: {$fleetError}</p>
	{/if}
	{#if $fleetShards.length === 0}
		{#if refreshing}
			<p class="text-gray-400">Loading shards…</p>
		{:else if !$fleetError}
			<p class="text-gray-400">No shards found.</p>
		{/if}
	{:else}
		<table class="w-full text-sm text-left text-gray-300">
			<thead
				class="text-xs uppercase text-gray-400 border-b border-secondary-600 dark:border-space-800"
			>
				<tr>
					<th class="py-2 pr-4">Shard</th>
					<th class="py-2 pr-4">Backend</th>
					<th class="py-2 pr-4">Launcher</th>
					<th class="py-2 pr-4">Steam</th>
					<th class="py-2 pr-4">Fleets</th>
					<th class="py-2">Next</th>
				</tr>
			</thead>
			<tbody>
				{#each $fleetShards as shard (shard.name)}
					{@const backend = shard.status?.backend}
					{@const latest = newestBuiltPart(backendParts(backend?.parts))}
					{@const backendIssue = shard.status
						? backendWarning(backend, $fleetPromotions, shard.name, now)
						: null}
					<tr class="border-b border-secondary-600 dark:border-space-800 align-top">
						<td class="py-2 pr-4">
							<a class="text-white font-semibold hover:underline" href={detailHref(shard.name)}
								>{shard.displayName}</a
							>
						</td>
						<td class="py-2 pr-4" title={backendHover(shard)}>
							<span class="flex items-center gap-1">
								{#if latest}
									<code>{formatBackendPart(latest, now)}</code>
								{:else}
									<span class="text-gray-500">—</span>
								{/if}
								{#if backendIssue}
									<span title={backendIssue}
										><ExclamationCircleOutline class="w-4 h-4 text-yellow-400" /></span
									>
								{/if}
							</span>
						</td>
						<td class="py-2 pr-4">
							{#each channels(shard, 'launcher') as channel (channel.key)}
								{@const warning = observationWarning(channel, now)}
								<div class="flex items-center gap-1" title={channelHover(channel)}>
									<span class="text-gray-400">{channel.metadataPath}:</span>
									{#if channel.version}
										<ShaLink sha={channel.version} />
										<span class="text-gray-400">· {formatAge(channel.since, now)}</span>
									{:else}
										<span class="text-gray-500">unknown</span>
									{/if}
									{#if warning}
										<span title={warning}
											><ExclamationCircleOutline class="w-4 h-4 text-yellow-400" /></span
										>
									{/if}
								</div>
							{/each}
						</td>
						<td class="py-2 pr-4">
							{#each channels(shard, 'steam') as channel (channel.key)}
								{@const warning = observationWarning(channel, now)}
								<div class="flex items-center gap-1" title={steamHover(channel)}>
									<span class="text-gray-400">{channel.branch}:</span>
									{#if channel.version}
										<ShaLink sha={channel.version} />
									{:else}
										<span class="text-gray-500">{steamBuildText(channel)}</span>
									{/if}
									{#if channel.since}
										<span class="text-gray-400">· {formatAge(channel.since, now)}</span>
									{/if}
									{#if channel.source === 'promotion'}
										<span
											class="text-gray-500 text-xs"
											title="set from the last promotion; not yet confirmed by observing the channel"
											>◇</span
										>
									{/if}
									{#if warning}
										<span title={warning}
											><ExclamationCircleOutline class="w-4 h-4 text-yellow-400" /></span
										>
									{/if}
								</div>
							{/each}
						</td>
						<td class="py-2 pr-4">
							<span class="flex items-center gap-1">
								{fleetsColumnText(shard)}
								{#if fleetsAttention(shard)}
									<span title={fleetsAttention(shard)}
										><ExclamationCircleOutline class="w-4 h-4 text-yellow-400" /></span
									>
								{/if}
							</span>
						</td>
						<td class="py-2">
							{#each runningFor(shard.name, $fleetPromotions) as promotion (promotion.metadata.name)}
								<div>
									<a class="text-primary-400 hover:underline" href={detailHref(shard.name)}
										>Promoting {shortSha(promotion.spec.version)} · {promotionProgressText(
											promotion
										).toLowerCase()}</a
									>
								</div>
							{/each}
						</td>
					</tr>
				{/each}
			</tbody>
		</table>
	{/if}
</Card>
