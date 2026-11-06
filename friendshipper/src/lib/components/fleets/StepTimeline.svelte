<script lang="ts">
	// A promotion's steps, in the order listed: each with its state and what it says (the backend
	// plan, the warm gate, why it was skipped, how it failed), and one link: "Argo ↗" once its
	// workflow started, or "Release ↓" to its release's card. Below them, what the promotion changed.
	import { promotionChanges } from '$lib/fleetFormat';
	import { promotionStepRows, type StepRowState } from '$lib/promotionCard';
	import type { FleetRelease, FleetsConfig, Nullable, Promotion } from '$lib/types';
	import WorkflowLink from './WorkflowLink.svelte';

	export let promotion: Promotion;
	export let now: number;
	// This shard's promotions, for the pending steps' typical durations.
	export let history: Promotion[] = [];
	export let releases: FleetRelease[] = [];
	export let fleets: Nullable<FleetsConfig> = null;

	$: rows = promotionStepRows(promotion, { history, releases, fleets, now });
	$: changes = promotionChanges(promotion, now);

	const ICONS: Record<StepRowState, string> = {
		pending: '○',
		running: '◐',
		succeeded: '●',
		failed: '✕',
		skipped: '–',
		stopped: '■',
		notRun: '–'
	};

	const CLASSES: Record<StepRowState, string> = {
		pending: 'text-gray-500',
		running: 'text-primary-400',
		succeeded: 'text-green-400',
		failed: 'text-red-400',
		skipped: 'text-gray-500',
		stopped: 'text-gray-400',
		notRun: 'text-gray-500'
	};

	const MUTED: StepRowState[] = ['skipped', 'stopped', 'notRun'];

	// The release's card, or the releases section when the card is hidden (a retired release).
	const showRelease = (name: string) => {
		const target =
			document.getElementById(`release-${name}`) ?? document.getElementById('releases');
		target?.scrollIntoView({ behavior: 'smooth' });
	};
</script>

{#if rows.length > 0}
	<ol class="flex flex-col gap-0.5 text-xs">
		{#each rows as row (row.name)}
			<li
				class="flex items-baseline gap-2 {MUTED.includes(row.state)
					? 'text-gray-500'
					: 'text-gray-300'}"
			>
				<span class="w-3 flex-none text-center {CLASSES[row.state]}">{ICONS[row.state]}</span>
				<span class="flex-none" class:text-white={row.state === 'running'}>{row.label}</span>
				{#if row.elapsed}
					<span class="text-gray-500 flex-none">{row.elapsed}</span>
				{:else if row.typical}
					<span class="text-gray-500 flex-none">{row.typical}</span>
				{/if}
				{#if row.detail}
					<span
						class="truncate"
						class:text-red-400={row.state === 'failed'}
						class:text-gray-400={row.state !== 'failed'}
						title={row.detail}>{row.detail}</span
					>
				{/if}
				<span class="ml-auto flex-none">
					{#if row.release}
						<button
							class="text-primary-400 hover:underline"
							title="FleetRelease {row.release}"
							on:click={() => {
								showRelease(row.release ?? '');
							}}>Release ↓</button
						>
					{:else if row.workflow}
						<WorkflowLink workflow={row.workflow} label="Argo ↗" />
					{/if}
				</span>
			</li>
		{/each}
	</ol>
{/if}
{#if changes.length > 0}
	<div class="text-xs text-gray-400 flex flex-col gap-0.5">
		<span class="text-gray-500 uppercase">Changed</span>
		{#each changes as change}
			<span>{change}</span>
		{/each}
	</div>
{/if}
