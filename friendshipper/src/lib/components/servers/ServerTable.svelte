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
	import {
		ArchiveArrowDownOutline,
		ArrowDownToBracketOutline,
		CodeOutline,
		FileCopyOutline,
		PhoneOutline
	} from 'flowbite-svelte-icons';
	import { emit } from '@tauri-apps/api/event';
	import { ProgressModal } from '@ethos/core';
	import type { GameServerResult, SyncClientRequest } from '$lib/types';
	import { LaunchMode } from '$lib/types';
	import { appConfig, backgroundSyncInProgress, builds, dynamicConfig } from '$lib/stores';
	import { syncClient } from '$lib/builds';
	import {
		copyProfileDataFromGameserver,
		downloadServerLogs,
		terminateServer
	} from '$lib/gameServers';
	import ServerLogsModal from '$lib/components/servers/ServerLogsModal.svelte';

	const defaultLogTooltip = 'Download server logs';

	export let servers: GameServerResult[] = [];
	export let onUpdateServers: () => Promise<void>;
	export let showHeader = false;

	// Loading states
	let syncing = false;
	let progressModalTitle = 'Syncing client';
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
		const entry = $builds.entries?.find((e) => e.commit === server.version);

		if (!entry) {
			return;
		}

		syncing = true;
		progressModalTitle = 'Syncing client';
		const req: SyncClientRequest = {
			artifactEntry: entry,
			methodPrefix: $builds.methodPrefix,
			launchOptions: {
				name: server.name,
				launchMode: LaunchMode.WithServer
			}
		};

		try {
			if (server.ready) {
				await syncClient(req);
			} else {
				await emit('error', 'Server is not ready. Try again shortly.');
			}
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

	const handleCopyMobileLaunchText = async (server: GameServerResult) => {
		try {
			const url = `${$dynamicConfig.mobileURLScheme}://?${server.ip}:${server.port}&NetImguiClientPort=${server.netimguiPort}&PlayerName=${$appConfig.userDisplayName}`;
			void navigator.clipboard.writeText(url);
		} catch (e) {
			await emit('error', e);
		}
	};

	const getAgeString = (creationTimestamp: string): string => {
		const date = new Date(creationTimestamp);
		const now = new Date();
		const diff = now.getTime() - date.getTime();
		const days = Math.floor(diff / (1000 * 60 * 60 * 24));
		const hours = Math.floor((diff % (1000 * 60 * 60 * 24)) / (1000 * 60 * 60));
		const minutes = Math.floor((diff % (1000 * 60 * 60)) / (1000 * 60));
		const seconds = Math.floor((diff % (1000 * 60)) / 1000);
		if (days > 0) {
			return `${days}d ${hours}h`;
		}
		if (hours > 0) {
			return `${hours}h ${minutes}m`;
		}
		if (minutes > 0) {
			return `${minutes}m ${seconds}s`;
		}
		return `${seconds}s`;
	};
</script>

<Table color="custom" striped={true} divClass="w-full h-full overflow-x-hidden overflow-y-auto">
	{#if showHeader}
		<TableHead class="text-center border-b-0 p-2 bg-secondary-800 dark:bg-space-950">
			<TableHeadCell class="">Name</TableHeadCell>
			<TableHeadCell class="">Version</TableHeadCell>
			<TableHeadCell class="">Age</TableHeadCell>
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
					<TableBodyCell class="py-2 w-64 max-w-[16rem] text-xs"
						>{formatServerName(server.displayName || server.name)}</TableBodyCell
					>
					<TableBodyCell
						class="py-2 w-28 max-w-[7rem] break-normal overflow-ellipsis whitespace-nowrap text-xs"
						><code>{server.version.substring(0, 8)}</code></TableBodyCell
					>
					<TableBodyCell
						class="py-2 w-24 max-w-[6rem] break-normal overflow-ellipsis whitespace-nowrap text-xs text-center"
						>{getAgeString(server.creationTimestamp)}</TableBodyCell
					>
					<TableBodyCell class="py-2 flex gap-2 justify-end">
						<Button
							outline
							disabled={syncing || $backgroundSyncInProgress}
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
						{#if $dynamicConfig.profileDataPath?.length > 0}
							<Button
								outline
								size="sm"
								disabled={syncing}
								on:click={async () => {
									progressModalTitle = 'Copying profile data';
									syncing = true;
									await copyProfileDataFromGameserver(server.name);
									syncing = false;
								}}
							>
								<ArrowDownToBracketOutline class="w-4 h-4" />
							</Button>
							<Tooltip
								class="w-auto text-xs text-primary-400 bg-secondary-600 dark:bg-space-800"
								placement="bottom"
								>Copy profile data
							</Tooltip>
						{/if}
						<Button
							outline
							size="sm"
							on:click={() => {
								handleCopyText(server);
							}}
						>
							<FileCopyOutline class="w-4 h-4" />
						</Button>
						<Tooltip
							class="w-auto text-xs text-primary-400 bg-secondary-600 dark:bg-space-800"
							placement="bottom"
							>Copy launch URL
						</Tooltip>
						{#if $dynamicConfig.mobileURLScheme?.length > 0}
							<Button
								outline
								size="sm"
								on:click={() => {
									void handleCopyMobileLaunchText(server);
								}}
							>
								<PhoneOutline class="w-4 h-4" />
							</Button>
						{/if}
						<Tooltip
							class="w-auto text-xs text-primary-400 bg-secondary-600 dark:bg-space-800"
							placement="bottom"
							>Copy mobile launch URL
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

<ProgressModal bind:showModal={syncing} title={progressModalTitle} />
<ServerLogsModal bind:showModal={showServerLogsModal} serverName={selectedServerName} />
