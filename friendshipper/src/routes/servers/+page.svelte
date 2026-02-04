<script lang="ts">
	import { Button, Card, Spinner, Checkbox, Select } from 'flowbite-svelte';
	import { emit, listen } from '@tauri-apps/api/event';
	import { onMount } from 'svelte';
	import { CirclePlusOutline, RefreshOutline, FolderOpenOutline } from 'flowbite-svelte-icons';
	import { get } from 'svelte/store';
	import { appConfig, builds, dynamicConfig, selectedCommit } from '$lib/stores';
	import type { ArtifactEntry, GameServerCluster, GameServerResult, Nullable } from '$lib/types';
	import ServerTable from '$lib/components/servers/ServerTable.svelte';
	import {
		getServers,
		getClusterServers,
		initAdditionalClusters,
		openLogsFolder
	} from '$lib/gameServers';
	import ServerModal from '$lib/components/servers/ServerModal.svelte';
	import { handleError } from '$lib/utils';
	import { getBuilds } from '$lib/builds';

	let fetchingServers = false;
	let loadingMessage = 'Fetching servers...';
	let servers: GameServerResult[] = [];

	// create server modal
	let showModal = false;
	let selected: Nullable<ArtifactEntry> = get(selectedCommit);

	// Multi-cluster state
	let multiClusterEnabled = false;
	let selectedCluster: string = '';
	let initializingClusters = false;

	$: clusters = $dynamicConfig?.gameServerClusters ?? [];
	$: hasClusters = clusters.length > 0;
	$: clusterSelectItems = [
		{ value: '', name: 'Default' },
		...clusters.map((c: GameServerCluster) => ({ value: c.clusterName, name: c.displayName }))
	];

	const updateServers = async () => {
		if ($appConfig.initialized) {
			loadingMessage = 'Fetching servers...';
			fetchingServers = true;
			try {
				if (multiClusterEnabled && selectedCluster) {
					servers = await getClusterServers(selectedCluster);
				} else {
					servers = await getServers();
				}
			} catch (e) {
				await handleError(e);
			}
			fetchingServers = false;
		}
	};

	const handleMultiClusterToggle = async () => {
		if (multiClusterEnabled) {
			// Always initialize/refresh clusters when enabling to ensure fresh credentials
			initializingClusters = true;
			loadingMessage = 'Initializing cluster connections...';
			try {
				await initAdditionalClusters();
			} catch (e) {
				await handleError(e);
				multiClusterEnabled = false;
			}
			initializingClusters = false;
		}

		// Reset to default cluster when toggling
		selectedCluster = '';
		await updateServers();
	};

	const handleClusterChange = async () => {
		await updateServers();
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

<div class="flex items-center justify-between gap-2">
	<div class="flex items-center gap-2">
		<p class="text-2xl my-2 text-primary-400 dark:text-primary-400">Servers</p>
		<Button
			disabled={fetchingServers || initializingClusters}
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
			disabled={fetchingServers || initializingClusters}
			class="!p-1.5"
			primary
			on:click={async () => {
				loadingMessage = 'Fetching latest builds...';
				fetchingServers = true;
				builds.set(await getBuilds(250));
				fetchingServers = false;

				showModal = true;
			}}
		>
			{#if fetchingServers}
				<Spinner size="4" />
			{:else}
				<CirclePlusOutline class="w-4 h-4" />
			{/if}
		</Button>
		{#if fetchingServers || initializingClusters}
			<code class="text-xs text-gray-400 dark:text-gray-400">{loadingMessage}</code>
		{/if}
	</div>
	<div class="flex items-center gap-2">
		{#if hasClusters}
			<Checkbox
				bind:checked={multiClusterEnabled}
				on:change={handleMultiClusterToggle}
				disabled={initializingClusters}
				class="text-sm"
			>
				Show All Clusters
			</Checkbox>
			{#if multiClusterEnabled}
				<Select
					items={clusterSelectItems}
					bind:value={selectedCluster}
					on:change={handleClusterChange}
					disabled={fetchingServers || initializingClusters}
					size="sm"
					class="w-40 bg-secondary-600 dark:bg-space-800 text-white border-gray-500"
				/>
			{/if}
		{/if}
		<Button outline class="!p-1.5" size="xs" on:click={openLogsFolder}>
			<FolderOpenOutline class="w-4 h-4 mr-1" />
			Open logs folder
		</Button>
	</div>
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
