<script lang="ts">
	import { Card, Spinner } from 'flowbite-svelte';
	import { emit } from '@tauri-apps/api/event';
	import { onMount } from 'svelte';
	import { CirclePlusOutline, RefreshOutline } from 'flowbite-svelte-icons';
	import { get } from 'svelte/store';
	import { appConfig, builds, selectedCommit } from '$lib/stores';
	import type { ArtifactEntry, GameServerResult, Nullable, SyncClientRequest } from '$lib/types';
	import ServerTable from '$lib/components/servers/ServerTable.svelte';
	import { getServers } from '$lib/gameServers';

	let fetchingServers = false;
	let servers: GameServerResult[] = [];

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

	onMount(() => {
		void updateServers();
	});
</script>

<div class="flex items-center gap-2">
	<p class="text-2xl my-2 text-primary-400 dark:text-primary-400">Servers</p>
	{#if fetchingServers}
		<Spinner size="4" />
	{/if}
</div>
<Card
	class="w-full p-2 sm:p-2 max-w-full bg-secondary-700 dark:bg-space-900 h-full overflow-y-hidden border-0 shadow-none flex flex-col gap-0 overflow-auto"
>
	<ServerTable
		{servers}
		onUpdateServers={async () => {
			await updateServers();
		}}
	/>
</Card>
