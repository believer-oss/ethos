<script lang="ts">
	// One promotion on the shard page. Running ones, and a Steam `public` flip waiting for a person,
	// show in full: the header, notes, steps and what changed. A finished one is a single line that
	// expands in place to its summary and steps.
	import { Button } from 'flowbite-svelte';
	import { emit } from '@tauri-apps/api/event';
	import {
		channelLabel,
		formatAgo,
		isActivePromotion,
		isCancelling,
		promotionChangedBy,
		promotionNote,
		promotionSubmittedAt,
		retryingMessage
	} from '$lib/fleetFormat';
	import {
		cancelTooLate,
		cancellingText,
		endSummary,
		flipBuildId,
		gameConfigNote,
		historyLine,
		isExpandedByDefault,
		localizeTimes,
		promotionCardPhase,
		waitingForPerson
	} from '$lib/promotionCard';
	import type {
		FleetRelease,
		FleetsConfig,
		Nullable,
		Promotion,
		ShardConfigSpec
	} from '$lib/types';
	import ReleaseGate from './ReleaseGate.svelte';
	import StepTimeline from './StepTimeline.svelte';
	import ShaLink from './ShaLink.svelte';

	export let promotion: Promotion;
	// This shard's promotions, for typical step durations.
	export let history: Promotion[];
	export let releases: FleetRelease[];
	export let config: Nullable<ShardConfigSpec>;
	export let fleets: Nullable<FleetsConfig>;
	export let now: number;
	// Null: no Cancel button (switch off, or nothing to cancel).
	export let onCancel: Nullable<(p: Promotion) => void> = null;
	export let cancelling = false;

	let open = false;
	$: full = isExpandedByDefault(promotion) || open;
	$: waiting = waitingForPerson(promotion);
	$: line = historyLine(promotion, now);
	$: note = promotionNote(promotion);
	$: summary = endSummary(promotion, releases, now);
	$: gameConfig = gameConfigNote(promotion, config);
	$: buildId = flipBuildId(promotion);
	$: from = promotion.status?.previousVersion;

	const phaseClass = (p: Promotion): string => {
		if (waitingForPerson(p)) return 'text-yellow-300';
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

	const LINE_TONE = { ok: 'text-green-400', bad: 'text-red-400', muted: 'text-gray-400' };

	const copyBuildId = async () => {
		if (!buildId) return;
		try {
			await navigator.clipboard.writeText(buildId);
			await emit('success', `Copied Steam build ID ${buildId}`);
		} catch (e) {
			await emit('error', e);
		}
	};
</script>

{#if !full}
	<button
		class="flex items-center gap-2 text-xs text-gray-300 text-left rounded px-2 py-1 hover:bg-secondary-800 dark:hover:bg-space-800"
		title="Show the summary and steps"
		on:click={() => {
			open = true;
		}}
	>
		<span class="w-3 text-center {LINE_TONE[line.tone]}">{line.icon}</span>
		<span>{line.channel}</span>
		<code class="text-gray-400">{line.versions}</code>
		<span class="text-gray-400">{line.who}</span>
		{#if line.when}<span class="text-gray-500">{line.when}</span>{/if}
		{#if line.took}<span class="text-gray-500">took {line.took}</span>{/if}
		<span class="truncate text-gray-400">{line.outcome}</span>
	</button>
{:else}
	<div
		class="rounded-md border p-3 text-sm text-gray-300 flex flex-col gap-1 {waiting
			? 'border-yellow-600 bg-yellow-900/10'
			: 'border-secondary-600 dark:border-space-800'}"
	>
		<div class="flex items-center gap-2 flex-wrap">
			<span class="rounded bg-gray-800 px-1.5 py-0.5 text-xs font-semibold {phaseClass(promotion)}"
				>{promotionCardPhase(promotion)}</span
			>
			<span>{channelLabel(promotion.spec.channel)}</span>
			<span class="text-gray-300"
				>{#if from}<ShaLink sha={from} /> → {/if}<ShaLink sha={promotion.spec.version} /></span
			>
			<span class="text-gray-400 text-xs">
				{promotionChangedBy(promotion)} · {formatAgo(promotionSubmittedAt(promotion), now)}
			</span>
			{#if isCancelling(promotion) && cancellingText(promotion, now)}
				<span class="text-gray-400 text-xs">· cancel {cancellingText(promotion, now)}</span>
			{/if}
			<span class="ml-auto flex items-center gap-2">
				{#if !isExpandedByDefault(promotion)}
					<button
						class="text-xs text-gray-400 hover:underline"
						on:click={() => {
							open = false;
						}}>Collapse</button
					>
				{/if}
				{#if onCancel && isActivePromotion(promotion) && !isCancelling(promotion)}
					<ReleaseGate let:disabled>
						<Button
							size="xs"
							color="red"
							disabled={disabled || cancelling}
							on:click={() => {
								onCancel?.(promotion);
							}}>Cancel</Button
						>
					</ReleaseGate>
				{/if}
			</span>
		</div>
		{#if note}<div class="text-gray-400 italic">“{note}”</div>{/if}
		{#if waiting}
			<div
				class="rounded border border-yellow-700 bg-yellow-900/20 text-yellow-200 p-2 flex flex-col gap-2"
			>
				<span>
					Set Steam build {buildId ?? ''} live on <code>public</code> in Steamworks (it needs a Steam
					Mobile confirmation). The promotion finishes when Steam shows it.
				</span>
				{#if buildId}
					<span
						><Button size="xs" color="alternative" on:click={copyBuildId}>Copy build ID</Button
						></span
					>
				{/if}
			</div>
		{/if}
		{#if retryingMessage(promotion)}
			<div class="text-yellow-400">
				Retrying: {localizeTimes(retryingMessage(promotion) ?? '', now)}
			</div>
		{:else if summary.length}
			{#each summary as text}<div class="text-gray-300">{text}</div>{/each}
		{:else if promotion.status?.phase === 'Rejected' && promotion.status.message}
			<div class="text-red-300">{localizeTimes(promotion.status.message, now)}</div>
		{/if}
		{#if cancelTooLate(promotion)}
			<div class="text-yellow-300 text-xs">
				The cancel came after the channel switch started, which can't be undone: this promotion
				completed.
			</div>
		{/if}
		{#if promotion.spec.skipFleetGate}
			<div class="text-yellow-400 text-xs">
				Warm gate skipped: “{promotion.spec.skipFleetGate.reason}”
			</div>
		{/if}
		<StepTimeline {promotion} {now} {history} {releases} {fleets} />
		{#if gameConfig}<div class="text-gray-500 text-xs">{gameConfig}</div>{/if}
	</div>
{/if}
