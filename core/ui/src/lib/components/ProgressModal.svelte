<script lang="ts">
	import { Modal, Spinner, Progressbar, Helper } from 'flowbite-svelte';
	import { listen } from '@tauri-apps/api/event';

	export let showModal: boolean;
	export let title: string = 'Syncing';

	let progress = 0;
	let elapsed = '';
	let remaining = '';

	let message = '';

	void listen('longtail-sync-progress', (event) => {
		const captures = event.payload as { progress: string; elapsed: string; remaining: string };
		progress = parseFloat(captures.progress.replace('%', ''));
		elapsed = captures.elapsed;
		remaining = captures.remaining;
	});

	void listen('git-log', (event) => {
		message = event.payload as string;
	});

	const onOpen = () => {
		progress = 0;
		elapsed = '';
		remaining = '';
		message = '';
	};
</script>

<Modal
	defaultClass="bg-secondary-700 dark:bg-space-900 overflow-y-auto"
	bodyClass="!border-t-0"
	dismissable={false}
	size="lg"
	bind:open={showModal}
	on:open={onOpen}
>
	<div class="flex items-center justify-start gap-2">
		<Spinner size="4" />
		<p class="text-xl text-primary-400 whitespace-nowrap">{title}...</p>

		{#if progress > 0}
			<Progressbar {progress} size="h-4" class="w-full" labelInside />
		{/if}
	</div>
	{#if message}
		<div class="rounded-md p-2 bg-secondary-800 dark:bg-space-950">
			<p class="text-sm text-gray-300 dark:text-gray-300 m-0">{message}</p>
		</div>
	{/if}
	{#if elapsed && remaining}
		<Helper class="text-sm text-gray-400 dark:text-gray-400 align-middle text-right">
			Elapsed: {elapsed} / ETA: {remaining}
		</Helper>
	{/if}
</Modal>
