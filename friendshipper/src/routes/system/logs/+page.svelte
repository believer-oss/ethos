<script lang="ts">
	import { FolderOpenOutline, RotateOutline } from 'flowbite-svelte-icons';
	import {
		Badge,
		Button,
		Card,
		Label,
		MultiSelect,
		Spinner,
		TableBody,
		TableBodyCell,
		TableBodyRow,
		TableSearch
	} from 'flowbite-svelte';
	import { onMount } from 'svelte';
	import { listen } from '@tauri-apps/api/event';
	import { getLogs, openSystemLogsFolder } from '$lib/system';
	import type { LogEvent } from '$lib/types';

	let loading = false;
	let logs: LogEvent[] = [];

	const refreshLogs = async () => {
		loading = true;
		logs = await getLogs();
		loading = false;
	};

	let selectedLevels: string[] = ['DEBUG', 'INFO', 'WARN', 'ERROR'];
	const levels = [
		{ value: 'DEBUG', name: 'DEBUG' },
		{ value: 'INFO', name: 'INFO' },
		{ value: 'WARN', name: 'WARN' },
		{ value: 'ERROR', name: 'ERROR' }
	];

	$: () => {
		if (selectedLevels.length === 0) {
			selectedLevels = ['DEBUG', 'INFO', 'WARN', 'ERROR'];
		}
	};

	let searchTerm = '';
	$: filteredItems = logs.filter(
		(log) =>
			JSON.stringify(log.fields).toLowerCase().indexOf(searchTerm.toLowerCase()) !== -1 &&
			(selectedLevels.includes(log.level) || selectedLevels.length === 0)
	);

	const getBadgeClass = (level: string): string => {
		switch (level) {
			case 'DEBUG':
				return 'bg-gray-500 dark:bg-gray-500';
			case 'INFO':
				return 'bg-blue-500 dark:bg-blue-500';
			case 'WARN':
				return 'bg-yellow-500 dark:bg-yellow-500';
			case 'ERROR':
				return 'bg-red-700 dark:bg-red-700';
			default:
				return 'bg-gray-500 dark:bg-gray-500';
		}
	};

	onMount(() => {
		void refreshLogs();

		void listen('log-event', () => {
			void refreshLogs();
		});
	});
</script>

<div class="flex items-center justify-between gap-2">
	<div class="flex items-center gap-2">
		<p class="text-2xl my-2 text-primary-400 dark:text-primary-400">Logs</p>
		<Button disabled={loading} class="!p-1.5" primary on:click={refreshLogs}>
			<RotateOutline class="w-4 h-4" />
		</Button>
		<Label class="text-sm text-gray-300">Filter by level:</Label>
		<MultiSelect
			class="text-white min-h-[2rem]"
			dropdownClass="min-w-[12rem] border-0"
			items={levels}
			bind:value={selectedLevels}
			size="sm"
		/>
		{#if loading}
			<Spinner size="4" />
		{/if}
	</div>
	<div class="flex items-center gap-2">
		<Button outline class="!p-1.5" size="xs" on:click={openSystemLogsFolder}>
			<FolderOpenOutline class="w-4 h-4 mr-1" />
			Open logs folder
		</Button>
	</div>
</div>
<Card
	class="w-full p-4 sm:p-4 max-w-full flex-grow overflow-y-auto bg-secondary-700 dark:bg-space-900 border-0 shadow-none"
>
	<div>
		<TableSearch
			placeholder="Search log messages"
			color="custom"
			divClass="relative overflow-x-auto sm:rounded-lg"
			innerDivClass="p-2 pt-0 pl-0"
			inputClass="bg-gray-50 border border-gray-300 text-gray-900 text-sm rounded-lg focus:ring-blue-500 focus:border-blue-500 block w-80 p-2 pl-10 bg-gray-700 dark:bg-gray-700 border-gray-300 dark:border-gray-300 placeholder-gray-400 dark:placeholder-gray-400 text-white dark:text-white focus:ring-primary-500 dark:focus:ring-primary-500 focus:border-primary-500 dark:focus:border-primary-500"
			hoverable={true}
			bind:inputValue={searchTerm}
		>
			<TableBody>
				{#each filteredItems as log}
					<TableBodyRow class="text-left border-b-0 p-2 bg-secondary-700 dark:bg-space-900">
						<TableBodyCell class="p-2 py-1 w-52">
							<p class="text-sm text-gray-300 dark:text-gray-300">{log.timestamp}</p>
						</TableBodyCell>
						<TableBodyCell class="text-left border-b-0 p-2 py-1 w-20">
							<p class="text-sm text-primary-400 dark:text-primary-400">
								<Badge class="text-white dark:text-white w-full h-full {getBadgeClass(log.level)}"
									>{log.level}</Badge
								>
							</p>
						</TableBodyCell>
						<TableBodyCell class="text-left border-b-0 p-2 py-1">
							<p class="text-sm text-primary-400 dark:text-primary-400 whitespace-normal">
								{JSON.stringify(log.fields, null, 1)}
							</p>
						</TableBodyCell>
					</TableBodyRow>
				{/each}
			</TableBody>
		</TableSearch>
	</div>
</Card>
