<script lang="ts">
	import { Input, Modal } from 'flowbite-svelte';
	import { listen } from '@tauri-apps/api/event';
	import { startLogTail, stopLogTail } from '$lib/gameServers';

	export let serverName: string;
	export let showModal: boolean;

	let lines: string[] = [];
	let searchTerm: string = '';

	void listen('gameserver-log', (event) => {
		const line = event.payload as string;
		lines = [line, ...lines];
	});

	$: filteredLogs = lines.filter((line) => {
		if (searchTerm === '') {
			return true;
		}
		return line.toLowerCase().includes(searchTerm.toLowerCase());
	});

	const onOpen = async () => {
		lines = [];
		if (serverName) {
			await startLogTail(serverName);
		}
	};

	const onClose = async () => {
		lines = [];
		await stopLogTail();
	};
</script>

<Modal
	size="xl"
	class="bg-secondary-700 dark:bg-space-900 relative flex flex-col mx-auto max-h-[100vh] border-0"
	placement="top-center"
	bodyClass="!border-t-0 overflow-y-hidden h-full"
	backdropClass="fixed mt-8 inset-0 z-40 bg-gray-900 bg-opacity-50 dark:bg-opacity-80"
	dialogClass="fixed mt-8 top-0 start-0 end-0 h-modal md:inset-0 md:h-full z-50 w-full p-4 pb-12 flex"
	bind:open={showModal}
	autoclose={false}
	outsideclose
	on:open={onOpen}
	on:close={onClose}
>
	<div class="flex flex-col space-y-2 overflow-y-hidden h-full justify-between">
		<div class="flex flex-row gap-4 items-center">
			<h2 class="text-lg font-semibold text-primary-400 pb-0">Following logs for {serverName}</h2>
			<Input
				type="text"
				size="sm"
				placeholder="Search"
				class="w-1/4 tracking-wider"
				bind:value={searchTerm}
			/>
		</div>
		{#if filteredLogs.length > 0}
			<div
				class="p-2 flex flex-col-reverse bg-secondary-800 dark:bg-space-950 overflow-x-auto overflow-y-auto rounded-md border border-secondary-600"
			>
				{#each filteredLogs as line}
					<code class="text-sm">{line}</code>
				{/each}
			</div>
		{:else}
			<code class="text-sm">No logs have been emitted yet!</code>
		{/if}
		<div class="flex justify-end items-end">
			<code class="text-xs align-right">{filteredLogs.length} entries</code>
		</div>
	</div>
</Modal>
