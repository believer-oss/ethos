<script lang="ts">
	import { Button, Card, Spinner } from 'flowbite-svelte';
	import { emit, listen } from '@tauri-apps/api/event';
	import { onMount } from 'svelte';
	import { CirclePlusOutline, RefreshOutline } from 'flowbite-svelte-icons';
	import { get } from 'svelte/store';
	import { appConfig, selectedCommit } from '$lib/stores';
	import type { ArtifactEntry, GameServerResult, Nullable } from '$lib/types';
	import ServerTable from '$lib/components/servers/ServerTable.svelte';
	import { getServers } from '$lib/gameServers';
	import ServerModal from '$lib/components/servers/ServerModal.svelte';

	let fetchingServers = false;
	let servers: GameServerResult[] = [];

	// create server modal
	let showModal = false;
	let selected: Nullable<ArtifactEntry> = get(selectedCommit);

	const updateServers = async () => {
		if ($appConfig.initialized) {
			fetchingServers = true;
			try {
				servers = await getServers();
			} catch (e) {
				await emit('error', e);
			}
			fetchingServers = false;
		}
	};

	const handleServerCreate = async () => {
		if (selected === null) {
			return;
		}

		try {
			await updateServers();
			selected = get(selectedCommit);
		} catch (e) {
			await emit('error', e);
		}
	};

	void listen('preferences-closed', () => {
		void updateServers();
	});

	onMount(() => {
		void updateServers();
	});
</script>

<div class="flex items-center gap-2">
	<p class="text-2xl my-2 text-primary-400 dark:text-primary-400">Servers</p>
	<Button
		disabled={fetchingServers}
		class="!p-1.5"
		primary
		on:click={async () => {
			await updateServers();
		}}
	>
		{#if fetchingServers}
			<Spinner size="4" />
		{:else}
			<RefreshOutline class="w-4 h-4" />
		{/if}
	</Button>
	<Button
		disabled={fetchingServers}
		class="!p-1.5"
		primary
		on:click={() => {
			showModal = true;
		}}
	>
		{#if fetchingServers}
			<Spinner size="4" />
		{:else}
			<CirclePlusOutline class="w-4 h-4" />
		{/if}
	</Button>
</div>
<Card
	class="w-full p-2 sm:p-2 max-w-full bg-secondary-700 dark:bg-space-900 h-full overflow-y-hidden border-0 shadow-none flex flex-col gap-0 overflow-auto"
>
	<ServerTable
		{servers}
		onUpdateServers={async () => {
			await updateServers();
		}}
		showHeader
	/>
</Card>

<ServerModal bind:showModal {handleServerCreate} />
