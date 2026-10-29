<script lang="ts">
	// The bottom bar's activity indicator: "N promotions running", for everyone, and a small tray
	// listing them.
	import { onDestroy } from 'svelte';
	import { goto } from '$app/navigation';
	import { activePromotions } from '$lib/fleets';
	import {
		channelLabel,
		formatElapsed,
		promotionChangedBy,
		promotionPhaseLabel,
		retryingMessage,
		promotionSubmittedAt,
		promotionWorkflow,
		runningPromotionsText,
		shortSha
	} from '$lib/fleetFormat';
	import WorkflowLink from './WorkflowLink.svelte';

	let open = false;
	let now = Date.now();
	// Ticks the elapsed times, only while something is running.
	let clock: ReturnType<typeof setInterval> | null = null;
	const setTicking = (ticking: boolean) => {
		if (ticking && !clock) {
			now = Date.now();
			clock = setInterval(() => {
				now = Date.now();
			}, 1000);
		} else if (!ticking && clock) {
			clearInterval(clock);
			clock = null;
		}
	};
	$: setTicking($activePromotions.length > 0);
	onDestroy(() => {
		setTicking(false);
	});

	$: if ($activePromotions.length === 0) open = false;
</script>

{#if $activePromotions.length > 0}
	<div class="relative ml-auto flex-none">
		<button
			class="flex items-center gap-1 text-xs text-primary-400 hover:text-primary-300"
			on:click={() => {
				open = !open;
			}}
		>
			<span class="inline-block w-2 h-2 rounded-full bg-primary-400 animate-pulse" />
			{runningPromotionsText($activePromotions.length)}
		</button>
		{#if open}
			<div
				class="absolute bottom-6 right-0 w-[28rem] max-h-80 overflow-y-auto rounded-md border border-secondary-600 dark:border-space-800 bg-secondary-700 dark:bg-space-900 shadow-2xl p-2 flex flex-col gap-2"
			>
				{#each $activePromotions as promotion (promotion.metadata.name)}
					<div
						class="flex flex-col gap-0.5 text-xs text-gray-300 border-b border-secondary-600 dark:border-space-800 pb-2 last:border-b-0 last:pb-0"
					>
						<div class="flex items-center gap-2">
							<span class="text-white font-semibold">{promotion.spec.shard}</span>
							<span>{channelLabel(promotion.spec.channel)}</span>
							<code class="text-gray-400">{shortSha(promotion.spec.version)}</code>
							<span class="ml-auto text-primary-400">{promotionPhaseLabel(promotion)}</span>
						</div>
						<div class="flex items-center gap-2 text-gray-400">
							<span>{promotionChangedBy(promotion)}</span>
							<span title="since submitted"
								>· {formatElapsed(promotionSubmittedAt(promotion), now)}</span
							>
							<span class="ml-auto flex gap-3">
								<button
									class="text-primary-400 hover:underline"
									on:click={() => {
										open = false;
										void goto(`/shards/detail?name=${encodeURIComponent(promotion.spec.shard)}`);
									}}>Shard</button
								>
								<WorkflowLink workflow={promotionWorkflow(promotion)} label="Argo" />
							</span>
						</div>
						{#if retryingMessage(promotion)}
							<div class="text-yellow-400 truncate" title={retryingMessage(promotion)}>
								Retrying: {retryingMessage(promotion)}
							</div>
						{/if}
					</div>
				{/each}
			</div>
		{/if}
	</div>
{/if}
