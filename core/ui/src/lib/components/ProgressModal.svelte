<script lang="ts">
	import { Modal, Spinner, Progressbar, Helper, Button } from 'flowbite-svelte';
	import { listen } from '@tauri-apps/api/event';

	export let showModal: boolean;
	export let title: string = 'Syncing';
	export let cancellable: boolean = false;
	export let onCancel: () => void = () => {};

	let progress = 0;
	let elapsed = '';
	let remaining = '';

	// High-level sync phase label (e.g. "Pulling latest changes from GitHub").
	// Sent by the backend on the `sync-phase` event. Rendered so users see a
	// persistent, plain-language step name.
	let phase = '';

	// Current build tool being installed (e.g. "Installing Visual Studio Community").
	let installingTools = '';

	void listen('longtail-sync-progress', (event) => {
		const captures = event.payload as { progress: string; elapsed: string; remaining: string };
		progress = parseFloat(captures.progress.replace('%', ''));
		elapsed = captures.elapsed;
		remaining = captures.remaining;
	});

	void listen('sync-phase', (event) => {
		phase = event.payload as string;
	});

	void listen('installing-build-tools', (event) => {
		installingTools = event.payload as string;
	});

	const onOpen = () => {
		progress = 0;
		elapsed = '';
		remaining = '';
		phase = '';
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

			{#if progress > 0}
				<Progressbar {progress} size="h-4" class="w-full" labelInside />
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
		</div>
	{/if}
	{#if elapsed && remaining}
		<Helper class="text-sm text-gray-400 dark:text-gray-400 align-middle text-right">
			Elapsed: {elapsed} / ETA: {remaining}
		</Helper>
	{/if}
</Modal>
