<script lang="ts">
	import { Button, Modal, Spinner } from 'flowbite-svelte';
	import { DownloadOutline, PlayOutline } from 'flowbite-svelte-icons';
	import { save as saveDialog } from '@tauri-apps/plugin-dialog';
	import { emit } from '@tauri-apps/api/event';
	import { downloadUtrace, openUtraceInInsights } from '$lib/utrace';
	import { handleError } from '$lib/utils';

	export let open: boolean;
	export let trace: {
		date: string;
		serverName: string;
		filename: string;
		key: string;
	} | null;

	let downloading = false;
	let opening = false;

	$: if (!open) {
		downloading = false;
		opening = false;
	}

	const handleDownload = async () => {
		if (!trace) return;
		const dest = await saveDialog({
			defaultPath: trace.filename,
			filters: [{ name: 'Unreal Insights Trace', extensions: ['utrace'] }]
		});
		if (!dest) return;

		downloading = true;
		try {
			await downloadUtrace(trace.key, dest);
			await emit('success', `Saved ${trace.filename} to ${dest}`);
			open = false;
		} catch (e) {
			await handleError(e);
		}
		downloading = false;
	};

	const handleOpen = async () => {
		if (!trace) return;
		opening = true;
		try {
			await openUtraceInInsights(trace.key);
			open = false;
		} catch (e) {
			await handleError(e);
		}
		opening = false;
	};
</script>

<Modal
	size="sm"
	color="none"
	class="bg-secondary-700 dark:bg-space-900"
	bodyClass="!border-t-0"
	backdropClass="fixed mt-8 inset-0 z-40 bg-gray-900 bg-opacity-50 dark:bg-opacity-80"
	dialogClass="fixed mt-8 top-0 start-0 end-0 h-modal md:inset-0 md:h-full z-50 w-full p-4 pb-12 flex"
	bind:open
	autoclose={false}
	outsideclose
>
	<svelte:fragment slot="header">
		<h4 class="text-lg font-semibold text-primary-400">Open trace from link</h4>
	</svelte:fragment>

	{#if trace}
		<div class="flex flex-col gap-3">
			<div class="flex flex-col gap-1 min-w-0">
				<div class="flex items-center gap-2 min-w-0">
					<span class="text-sm text-white truncate" title={trace.filename}>
						{trace.filename}
					</span>
					<span
						class="shrink-0 text-[10px] px-1.5 py-0.5 rounded bg-secondary-600 dark:bg-space-800 text-gray-300 border border-gray-600"
						title={trace.serverName}
					>
						{trace.serverName}
					</span>
				</div>
				<div class="text-[11px] text-gray-400">{trace.date}</div>
			</div>

			<div class="flex items-center gap-2">
				<Button size="xs" outline disabled={downloading || opening} on:click={handleDownload}>
					{#if downloading}
						<Spinner size="4" class="mr-1" />
					{:else}
						<DownloadOutline class="w-4 h-4 mr-1" />
					{/if}
					Download
				</Button>
				<Button size="xs" outline disabled={downloading || opening} on:click={handleOpen}>
					{#if opening}
						<Spinner size="4" class="mr-1" />
					{:else}
						<PlayOutline class="w-4 h-4 mr-1" />
					{/if}
					Open with UnrealInsights
				</Button>
			</div>
		</div>
	{/if}
</Modal>
