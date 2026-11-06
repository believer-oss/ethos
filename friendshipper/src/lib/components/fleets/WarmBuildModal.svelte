<script lang="ts">
	// "Warm on shard…" from a build row: creates the build's FleetRelease (phase Warm) on a shard
	// with Fleets on, without promoting it. It retires on the shard's warm TTL unless a channel
	// comes to serve it or someone pins it. A Retired release of the build is replaced by a fresh one.
	import { Button, Modal, Spinner } from 'flowbite-svelte';
	import { emit } from '@tauri-apps/api/event';
	import { fleetReleases, fleetShards, refreshFleets, warmFleetRelease } from '$lib/fleets';
	import { warmCheck } from '$lib/fleetReleases';
	import { formatSpan, shortSha, warmTtlMs } from '$lib/fleetFormat';
	import ReleaseGate from './ReleaseGate.svelte';

	export let showModal = false;
	export let commit = '';

	let shardName = '';
	let submitting = false;
	let error = '';

	$: shards = $fleetShards.filter((s) => s.config?.fleets && s.spec?.fleetsEnabled);
	$: if (showModal && !shards.some((s) => s.name === shardName)) shardName = shards[0]?.name ?? '';
	$: existing = $fleetReleases.find((r) => r.spec.shard === shardName && r.spec.version === commit);
	$: shard = shards.find((s) => s.name === shardName);
	$: check = warmCheck(existing);
	$: ttl = formatSpan(warmTtlMs(shard?.config?.fleets));

	let wasOpen = false;
	$: if (showModal && !wasOpen) {
		error = '';
		void refreshFleets();
	}
	$: wasOpen = showModal;

	const warm = async () => {
		submitting = true;
		error = '';
		try {
			const release = await warmFleetRelease(shardName, commit);
			showModal = false;
			await emit('fleet-success', {
				message: `Warming ${shortSha(commit)} on ${shard?.displayName ?? shardName} (${
					release.metadata.name ?? ''
				})`,
				shard: shardName
			});
			await refreshFleets();
		} catch (e) {
			error = (e as { message?: string })?.message ?? String(e);
		} finally {
			submitting = false;
		}
	};
</script>

<!-- The body scrolls, not the frame: flowbite's Modal lets the mouse wheel through only there. -->
<Modal
	defaultClass="bg-secondary-700 dark:bg-space-900 relative flex flex-col mx-auto max-h-full"
	bodyClass="!border-t-0 min-h-0"
	backdropClass="fixed mt-8 inset-0 z-40 bg-gray-900 bg-opacity-50 dark:bg-opacity-80"
	dialogClass="fixed mt-8 top-0 start-0 end-0 h-modal md:inset-0 md:h-full z-50 w-full p-4 pb-12 flex"
	size="sm"
	outsideclose
	bind:open={showModal}
>
	<div class="flex flex-col gap-4 text-sm text-gray-300">
		<p class="text-xl text-primary-400">Warm <code>{shortSha(commit)}</code> on a shard</p>
		{#if shards.length === 0}
			<p>No shard has Fleets turned on.</p>
		{:else}
			<select
				bind:value={shardName}
				class="block w-full rounded-lg p-2.5 text-sm bg-secondary-600 dark:bg-space-800 text-white border-gray-500"
			>
				{#each shards as s (s.name)}
					<option value={s.name}>{s.displayName}</option>
				{/each}
			</select>
			{#if check.blocked}
				<p class="text-yellow-400">{check.blocked}</p>
			{:else}
				{#if check.note}
					<p class="text-yellow-400">{check.note}</p>
				{/if}
				<p class="text-gray-400">
					Its Fleets warm in every cluster of the shard, with the warm buffer. It retires after {ttl}
					unless a channel serves it or someone pins it.
				</p>
			{/if}
		{/if}
		{#if error}
			<div class="text-red-400 bg-red-900/20 p-2 rounded">Not warmed: {error}</div>
		{/if}
		<div class="flex flex-row-reverse gap-2">
			<ReleaseGate let:disabled>
				<Button
					class="bg-lime-700 dark:bg-lime-700 hover:bg-lime-600 dark:hover:bg-lime-600"
					disabled={disabled || submitting || !shardName || !!check.blocked}
					on:click={warm}
				>
					{#if submitting}<Spinner size="4" class="mr-2" />{/if}Warm
				</Button>
			</ReleaseGate>
			<Button
				class="bg-gray-700 dark:bg-gray-700 hover:bg-gray-600 dark:hover:bg-gray-600"
				on:click={() => {
					showModal = false;
				}}>Cancel</Button
			>
		</div>
	</div>
</Modal>
