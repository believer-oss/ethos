<script lang="ts">
	import { Modal, Spinner, Progressbar, Helper, Button } from 'flowbite-svelte';
	import { listen } from '@tauri-apps/api/event';
	import { SyncTracker, formatDuration, type SyncEvent } from '../types/sync.js';

	export let showModal: boolean;
	export let title: string = 'Syncing';
	export let cancellable: boolean = false;
	export let onCancel: () => void = () => {};

	const tracker = new SyncTracker();

	// null while the current phase reports no byte total - longtail has phases, such as
	// reading a full store index, that run for a while with nothing to divide by. Showing
	// a spinner there is honest; showing 0% looks hung.
	let percent: number | null = null;
	let elapsed = '';
	let remaining = '';
	let syncPhase = '';
	let failure = '';

	// High-level sync phase label (e.g. "Pulling latest changes from GitHub").
	// Sent by the backend on the `sync-phase` event. Rendered so users see a
	// persistent, plain-language step name.
	let phase = '';

	// Current build tool being installed (e.g. "Installing Visual Studio Community").
	let installingTools = '';

	void listen<SyncEvent>('sync-event', (event) => {
		const payload = event.payload;
		switch (payload.type) {
			case 'started':
				tracker.reset();
				percent = null;
				elapsed = '';
				remaining = '';
				syncPhase = '';
				failure = '';
				break;
			case 'progress':
				tracker.update(payload.progress);
				percent = tracker.percent;
				syncPhase = tracker.phase;
				elapsed = formatDuration(tracker.elapsedSeconds);
				remaining = formatDuration(tracker.remainingSeconds(payload.progress));
				break;
			case 'failed':
				failure = payload.error.summary;
				break;
			case 'finished':
			case 'cancelled':
				percent = null;
				break;
			default:
				break;
		}
	});

	void listen('sync-phase', (event) => {
		phase = event.payload as string;
	});

	void listen('installing-build-tools', (event) => {
		installingTools = event.payload as string;
	});

	const onOpen = () => {
		tracker.reset();
		percent = null;
		elapsed = '';
		remaining = '';
		phase = '';
		syncPhase = '';
		failure = '';
		installingTools = '';
	};
</script>

<Modal
	defaultClass="bg-secondary-700 dark:bg-space-900 overflow-y-auto"
	bodyClass="!border-t-0"
	backdropClass="fixed mt-8 inset-0 z-40 bg-gray-900 bg-opacity-50 dark:bg-opacity-80"
	dialogClass="fixed mt-8 top-0 start-0 end-0 h-modal md:inset-0 md:h-full z-50 w-full p-4 pb-12 flex"
	dismissable={false}
	size="lg"
	bind:open={showModal}
	on:open={onOpen}
>
	<div class="flex items-center justify-between gap-2 w-full">
		<div class="flex items-center justify-start gap-2 w-full">
			<Spinner size="4" />
			<p class="text-xl text-primary-400 whitespace-nowrap">{title}...</p>

			{#if percent !== null}
				<Progressbar progress={percent} size="h-4" class="w-full" labelInside />
			{/if}
		</div>

		<div class="flex items-center justify-end gap-2">
			{#if cancellable}
				<Button color="red" on:click={onCancel}>Cancel</Button>
			{/if}
		</div>
	</div>
	{#if installingTools}
		<div class="rounded-md p-2 bg-secondary-800 dark:bg-space-950">
			<p class="text-sm text-gray-300 dark:text-gray-300 m-0">{installingTools}</p>
		</div>
	{:else if phase}
		<div class="rounded-md p-3 bg-secondary-800 dark:bg-space-950">
			<p class="text-base text-primary-300 dark:text-primary-300 font-medium m-0">{phase}</p>
			{#if syncPhase}
				<p class="text-sm text-gray-400 dark:text-gray-400 m-0">{syncPhase}</p>
			{/if}
		</div>
	{:else if syncPhase}
		<div class="rounded-md p-3 bg-secondary-800 dark:bg-space-950">
			<p class="text-base text-primary-300 dark:text-primary-300 font-medium m-0">{syncPhase}</p>
		</div>
	{/if}
	{#if failure}
		<div class="rounded-md p-3 bg-red-950">
			<p class="text-sm text-red-200 m-0">{failure}</p>
		</div>
	{/if}
	{#if elapsed}
		<Helper class="text-sm text-gray-400 dark:text-gray-400 align-middle text-right">
			Elapsed: {elapsed} / ETA: {remaining}
		</Helper>
	{/if}
</Modal>
