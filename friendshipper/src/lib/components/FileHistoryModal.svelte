<script lang="ts">
	import { Modal } from 'flowbite-svelte';
	import { emit } from '@tauri-apps/api/event';
	import { getFileHistory } from '$lib/repo';
	import type { FileHistoryRevision } from '$lib/types';
	import FileHistoryPanel from './FileHistoryPanel.svelte';

	export let open: boolean = false;
	export let filePath: string | null = null;
	export let onReverted: (() => void | Promise<void>) | null = null;

	let revisions: FileHistoryRevision[] = [];
	let displayName = '';
	let loading = false;
	let lastLoadedPath: string | null = null;

	const load = async (path: string) => {
		loading = true;
		revisions = [];
		displayName = '';
		try {
			const result = await getFileHistory(path);
			revisions = result.revisions;
			displayName = result.displayName ?? '';
			lastLoadedPath = path;
		} catch (e) {
			await emit('error', e);
		} finally {
			loading = false;
		}
	};

	$: if (open && filePath && filePath !== lastLoadedPath) {
		void load(filePath);
	}
	$: if (!open) {
		lastLoadedPath = null;
	}
</script>

<Modal
	bind:open
	size="xl"
	color="none"
	class="bg-secondary-700 dark:bg-space-900"
	bodyClass="!border-t-0 !p-0 flex-1 overflow-y-auto overscroll-contain"
	backdropClass="fixed inset-0 z-40 bg-gray-900 bg-opacity-50 dark:bg-opacity-80"
	dialogClass="fixed top-0 start-0 end-0 h-modal md:inset-0 md:h-full z-50 w-full p-4 pb-12 flex"
	dismissable
	outsideclose
>
	<svelte:fragment slot="header">
		<h3 class="text-primary-400 text-xl">File history</h3>
	</svelte:fragment>

	<div class="h-[65vh]">
		<FileHistoryPanel {filePath} {displayName} {revisions} {loading} {onReverted} />
	</div>
</Modal>
