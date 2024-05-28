<script lang="ts">
	import { Button, Card, Spinner } from 'flowbite-svelte';
	import { emit } from '@tauri-apps/api/event';
	import { onMount } from 'svelte';
	import { CirclePlusOutline, RefreshOutline } from 'flowbite-svelte-icons';
	import { get } from 'svelte/store';
	import { appConfig, builds, selectedCommit } from '$lib/stores';
	import type { ArtifactEntry, GameServerResult, Nullable, SyncClientRequest } from '$lib/types';
	import ServerTable from '$lib/components/servers/ServerTable.svelte';
	import { getServers } from '$lib/gameServers';
	import ServerModal from '$lib/components/servers/ServerModal.svelte';
	import { syncClient } from '$lib/builds';

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

	const handleSyncClient = async (entry: Nullable<ArtifactEntry>, server: GameServerResult) => {
		if (entry === null) {
			return;
		}

		const req: SyncClientRequest = {
			artifactEntry: entry,
			methodPrefix: $builds.methodPrefix,
			launchOptions: {
				ip: server.ip,
				port: server.port,
				netimguiPort: server.netimguiPort
			}
		};

		try {
			await syncClient(req);
		} catch (e) {
			await emit('error', e);
		}
	};

	const handleAutoLaunch = async (serverName: string) => {
		if (selected === null) {
			return;
		}

		const server = servers.find((s) => s.displayName === serverName);
		if (server) {
			try {
				await handleSyncClient(selected, server);
			} catch (e) {
				await emit('error', e);
			}
		}
	};

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

<ServerModal bind:showModal {handleServerCreate} {handleAutoLaunch} />
