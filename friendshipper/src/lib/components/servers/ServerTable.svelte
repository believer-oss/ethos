<script lang="ts">
	import {
		Button,
		Table,
		TableBody,
		TableBodyCell,
		TableBodyRow,
		TableHead,
		TableHeadCell,
		Tooltip
	} from 'flowbite-svelte';
	import { ArchiveArrowDownOutline, CodeOutline, FileCopyOutline } from 'flowbite-svelte-icons';
	import { emit } from '@tauri-apps/api/event';
	import { ProgressModal } from '@ethos/core';
	import type { GameServerResult, SyncClientRequest } from '$lib/types';
	import { builds } from '$lib/stores';
	import { syncClient } from '$lib/builds';
	import { downloadServerLogs, terminateServer } from '$lib/gameServers';
	import ServerLogsModal from '$lib/components/servers/ServerLogsModal.svelte';

	const defaultLogTooltip = 'Download server logs';

	export let servers: GameServerResult[] = [];
	export let onUpdateServers: () => Promise<void>;
	export let showHeader = false;

	// Loading states
	let syncing = false;
	let downloadingLogs = false;
	let logTooltip = defaultLogTooltip;

	// logs modal
	let showServerLogsModal = false;
	let selectedServerName = '';

	const formatServerName = (name: string): string => {
		if (name.length > 30) {
			return `${name.slice(0, 27)}...`;
		}

		return name;
	};

	const handleSyncClient = async (server: GameServerResult) => {
		const entry = $builds.entries.find((e) => e.commit === server.version);

		if (!entry) {
			return;
		}

		syncing = true;
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

		syncing = false;
	};

	const handleDownloadLogs = async (server: GameServerResult) => {
		downloadingLogs = true;
		logTooltip = 'Downloading...';
		await downloadServerLogs(server.name);

		if (downloadingLogs) {
			logTooltip = 'Done!';
		}
	};

	const resetLogDownloadState = () => {
		logTooltip = defaultLogTooltip;
		downloadingLogs = false;
	};

	const handleCopyText = (server: GameServerResult) => {
		const url = `friendshipper://launch/${server.name}`;
		void navigator.clipboard.writeText(url);
	};
</script>

<Table color="custom" striped={true} divClass="w-full h-full overflow-x-hidden overflow-y-auto">
	{#if showHeader}
		<TableHead class="text-center border-b-0 p-2 bg-secondary-800 dark:bg-space-950">
			<TableHeadCell class="">Name</TableHeadCell>
			<TableHeadCell class="">Version</TableHeadCell>
			<TableHeadCell class="" />
		</TableHead>
	{/if}
	<TableBody>
		{#if servers.length === 0}
			<TableBodyRow class="text-center p-2">
				<TableBodyCell class="" colspan="4">
					<p class="text-gray-400 dark:text-gray-400">No servers found</p>
				</TableBodyCell>
			</TableBodyRow>
		{:else}
			{#each servers as server, index}
				<TableBodyRow
					class="text-center border-b-0 p-2 {index % 2 === 0
						? 'bg-secondary-700 dark:bg-space-900'
						: 'bg-secondary-800 dark:bg-space-950'}"
				>
					<TableBodyCell class="py-2 text-xs"
						>{formatServerName(server.displayName || server.name)}</TableBodyCell
					>
					<TableBodyCell
						class="py-2 w-16 max-w-[4rem] break-normal overflow-ellipsis whitespace-nowrap text-xs"
						>{server.version.substring(0, 8)}</TableBodyCell
					>
					<TableBodyCell class="py-2 flex gap-2 justify-end">
						<Button
							outline
							disabled={syncing}
							size="sm"
							on:click={async () => handleSyncClient(server)}
							>Sync & Join
						</Button>
						<Button
							outline
							size="sm"
							on:click={() => {
								showServerLogsModal = true;
								selectedServerName = server.name;
							}}
						>
							<CodeOutline class="w-4 h-4" />
						</Button>
						<Tooltip
							class="w-auto text-xs text-primary-400 bg-secondary-600 dark:bg-space-800"
							placement="bottom"
							>Tail server logs
						</Tooltip>
						<Button
							outline
							size="sm"
							on:click={() => handleDownloadLogs(server)}
							on:mouseleave={resetLogDownloadState}
						>
							<ArchiveArrowDownOutline class="w-4 h-4" />
						</Button>
						<Tooltip
							class="w-auto text-xs text-primary-400 bg-secondary-600 dark:bg-space-800"
							placement="bottom"
							>{logTooltip}
						</Tooltip>
						<Button outline size="sm" on:click={() => handleCopyText(server)}>
							<FileCopyOutline class="w-4 h-4" />
						</Button>
						<Tooltip
							class="w-auto text-xs text-primary-400 bg-secondary-600 dark:bg-space-800"
							placement="bottom"
							>Copy launch URL
						</Tooltip>
						<Button
							outline
							disabled={syncing}
							size="sm"
							color="red"
							on:click={async () => {
								await terminateServer(server.name);
								await onUpdateServers();
							}}
						>
							Terminate
						</Button>
					</TableBodyCell>
				</TableBodyRow>
			{/each}
		{/if}
	</TableBody>
</Table>

<ProgressModal bind:showModal={syncing} />
<ServerLogsModal bind:showModal={showServerLogsModal} serverName={selectedServerName} />