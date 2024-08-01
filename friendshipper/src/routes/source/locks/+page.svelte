<script lang="ts">
	import {
		Button,
		Card,
		Checkbox,
		Spinner,
		Table,
		TableBody,
		TableBodyCell,
		TableBodyRow,
		TableHead,
		TableHeadCell,
		TableSearch,
		Toggle,
		Tooltip
	} from 'flowbite-svelte';
	import { derived } from 'svelte/store';
	import { emit } from '@tauri-apps/api/event';
	import { RefreshOutline } from 'flowbite-svelte-icons';
	import { ProgressModal } from '@ethos/core';
	import type { Lock } from '$lib/types';
	import { releaseLocks, getRepoStatus } from '$lib/repo';
	import { allModifiedFiles, repoStatus } from '$lib/stores';

	let loading = false;
	let selectedForRelease: string[] = [];
	let numOthersSelected = 0;

	let showOtherLocks = false;
	let allowReleaseOtherLocks = false;
	let searchTerm = '';

	let showProgressModal: boolean = false;

	const formatPath = (path: string) => {
		if (path === '/') return path;
		return path.replace(/\/$/, '').split('/').pop();
	};

	const getLockDisplayName = (lock: Lock): string => {
		if (lock.display_name === null || lock.display_name === '') {
			return formatPath(lock.path);
		}
		return lock.display_name;
	};

	const sortLocksFunc = (a, b) => {
		const aName = getLockDisplayName(a);
		const bName = getLockDisplayName(b);
		return aName < bName ? -1 : 1;
	};

	$: sortedOurs = derived(
		repoStatus,
		($repoStatus) => $repoStatus.locksOurs.sort(sortLocksFunc),
		[]
	);
	$: sortedTheirs = derived(
		repoStatus,
		($repoStatus) => $repoStatus.locksTheirs.sort(sortLocksFunc),
		[]
	);

	$: filteredOurs = $sortedOurs.filter((item): boolean => {
		const search = searchTerm.toLowerCase();
		return (
			item.path.toLowerCase().includes(search) || item.display_name.toLowerCase().includes(search)
		);
	});
	$: filteredTheirs = $sortedTheirs.filter((item): boolean => {
		const search = searchTerm.toLowerCase();
		return (
			item.path.toLowerCase().includes(search) ||
			item.display_name.toLowerCase().includes(search) ||
			item.owner?.name.toLowerCase().includes(search)
		);
	});
	$: unmodifiedLockedFiles = $sortedOurs.filter(
		(lock) => !$allModifiedFiles.find((file) => file.path === lock.path)
	);

	const handleRelease = (e: Event, path: string, ours: boolean) => {
		if ((e.target as HTMLInputElement).checked) {
			selectedForRelease = [...selectedForRelease, path];

			if (!ours) numOthersSelected += 1;
		} else {
			selectedForRelease = selectedForRelease.filter((item) => item !== path);

			if (!ours) numOthersSelected -= 1;
		}
	};

	const handleReleaseAllTheirs = (e: Event) => {
		if (!allowReleaseOtherLocks) return;

		if ((e.target as HTMLInputElement).checked) {
			const paths = $sortedTheirs.map((lock) => lock.path);
			selectedForRelease = selectedForRelease.concat(paths);

			numOthersSelected += $sortedTheirs.length;

			selectedForRelease = selectedForRelease.filter(
				(item, index) => selectedForRelease.indexOf(item) === index
			);
		} else {
			numOthersSelected -= $sortedTheirs.length;
			selectedForRelease = selectedForRelease.filter(
				(path) => !$sortedTheirs.map((lock) => lock.path).includes(path)
			);
		}
	};

	const handleReleaseAllOurs = (e: Event) => {
		if ((e.target as HTMLInputElement).checked) {
			const paths = $sortedOurs.map((lock) => lock.path);
			selectedForRelease = selectedForRelease.concat(paths);

			selectedForRelease = selectedForRelease.filter(
				(item, index) => selectedForRelease.indexOf(item) === index
			);
		} else {
			selectedForRelease = selectedForRelease.filter(
				(path) => !$sortedOurs.map((lock) => lock.path).includes(path)
			);
		}
	};

	const refreshLocks = async () => {
		loading = true;
		showProgressModal = true;
		try {
			repoStatus.set(await getRepoStatus());
		} catch (e) {
			await emit('error', e);
		}
		loading = false;
		showProgressModal = false;
	};

	const handleReleaseSelected = async () => {
		if (selectedForRelease.length === 0) return;

		loading = true;
		showProgressModal = true;
		try {
			await releaseLocks(selectedForRelease, numOthersSelected > 0);
			await refreshLocks();
		} catch (e) {
			await emit('error', e);
		}
		selectedForRelease = [];
		loading = false;
		showProgressModal = false;
	};

	const handleReleaseUnmodified = async () => {
		if (unmodifiedLockedFiles.length === 0) return;

		loading = true;
		showProgressModal = true;
		try {
			await releaseLocks(
				unmodifiedLockedFiles.map((lock) => lock.path),
				false
			);
			await refreshLocks();
		} catch (e) {
			await emit('error', e);
		}
		selectedForRelease = [];
		loading = false;
		showProgressModal = false;
	};

	const getLockTimestamp = (locked_at: string): string => {
		const date = new Date(locked_at);
		return date.toLocaleString();
	};
</script>

<div class="flex items-center justify-between gap-2">
	<div class="flex items-center gap-2">
		<p class="text-2xl my-2 text-primary-400 dark:text-primary-400">File Locks</p>
		<Button class="!p-1.5" primary on:click={refreshLocks}>
			<RefreshOutline class="w-4 h-4" />
		</Button>
		<Button
			id="release-selected"
			disabled={selectedForRelease.length === 0}
			class="!p-1.5 text-xs"
			color={numOthersSelected > 0 ? 'red' : 'primary'}
			on:click={handleReleaseSelected}
			>Release Selected
		</Button>
		<Button
			id="release-unmodified"
			disabled={unmodifiedLockedFiles.length === 0}
			class="!p-1.5 text-xs"
			color="primary"
			on:click={handleReleaseUnmodified}
			>Unlock Unmodified (<span class="px-0.5">{unmodifiedLockedFiles.length}</span>)
		</Button>
		{#if loading}
			<Spinner size="4" />
		{/if}
	</div>
	<div class="flex items-center gap-2">
		{#if showOtherLocks}
			<Toggle class="text-white" bind:checked={allowReleaseOtherLocks}>Release Others' Locks</Toggle
			>
		{/if}
		<Toggle class="text-white" bind:checked={showOtherLocks}>Show Others' Locks</Toggle>
	</div>
	{#if numOthersSelected > 0}
		<Tooltip
			triggeredBy="#release-selected"
			class="w-auto text-xs text-white bg-red-700 dark:bg-red-700"
			placement="right"
			>Warning: This will release other users' locks!
		</Tooltip>
	{/if}
	<Tooltip
		triggeredBy="#release-unmodified"
		class="w-auto text-xs text-white bg-secondary-800 dark:bg-space-950"
		placement="bottom"
		>Release locks for any unmodified files
	</Tooltip>
</div>
<Card
	class="w-full min-h-[12rem] p-4 sm:p-4 max-w-full bg-secondary-700 dark:bg-space-900 overflow-y-hidden border-0 shadow-none"
>
	<h3 class="text-primary-400 text-xl pb-2">My Locks</h3>
	<TableSearch
		placeholder="Search by file path"
		hoverable={true}
		bind:inputValue={searchTerm}
		color="custom"
		divClass="relative overflow-x-auto sm:rounded-lg"
		innerDivClass="p-2 pt-0 pl-0"
		inputClass="bg-gray-50 border border-gray-300 text-gray-900 text-sm rounded-lg focus:ring-blue-500 focus:border-blue-500 block w-80 p-2 pl-10 bg-gray-700 dark:bg-gray-700 border-gray-300 dark:border-gray-300 placeholder-gray-400 dark:placeholder-gray-400 text-white dark:text-white focus:ring-primary-500 dark:focus:ring-primary-500 focus:border-primary-500 dark:focus:border-primary-500"
		striped
	>
		<TableHead class="text-left border-b-0 p-2 bg-secondary-800 dark:bg-space-950">
			<TableHeadCell class="!p-2">
				<Checkbox on:change={handleReleaseAllOurs} />
			</TableHeadCell>
			<TableHeadCell class="p-2">Path</TableHeadCell>
			<TableHeadCell class="p-2">Owner</TableHeadCell>
			<TableHeadCell class="p-2">Locked At</TableHeadCell>
		</TableHead>
		<TableBody>
			{#each filteredOurs as lock, index}
				<TableBodyRow
					class="text-left border-b-0 p-2 {index % 2 === 0
						? 'bg-secondary-700 dark:bg-space-900'
						: 'bg-secondary-800 dark:bg-space-950'}"
				>
					<TableBodyCell class="!p-2">
						<Checkbox
							checked={selectedForRelease.includes(lock.path)}
							on:change={(e) => {
								handleRelease(e, lock.path, true);
							}}
						/>
					</TableBodyCell>
					<TableBodyCell id="lock-{index}" class="p-2">
						{getLockDisplayName(lock)}
					</TableBodyCell>
					<TableBodyCell class="p-2">
						{lock.owner?.name}
					</TableBodyCell>
					<TableBodyCell class="p-2">
						{getLockTimestamp(lock.locked_at)}
					</TableBodyCell>
				</TableBodyRow>
				<Tooltip
					triggeredBy="#lock-{index}"
					class="w-auto text-xs text-primary-400 bg-secondary-600 dark:bg-space-800"
					placement="top"
					>{lock.path}
				</Tooltip>
			{:else}
				<TableBodyRow>
					<TableBodyCell class="p-2" />
					<TableBodyCell class="p-2">No locks found!</TableBodyCell>
				</TableBodyRow>
			{/each}
		</TableBody>
	</TableSearch>
</Card>
{#if showOtherLocks}
	<Card
		class="w-full p-4 sm:p-4 my-2 max-w-full bg-secondary-700 dark:bg-space-900 border-0 overflow-y-hidden shadow-none"
	>
		<h3 class="text-primary-400 text-xl pb-2">Other Locks</h3>
		<Table color="custom" class="mt-3" striped>
			<TableHead class="text-left border-b-0 p-2 bg-secondary-800 dark:bg-space-950">
				<TableHeadCell class="!p-2">
					<Checkbox disabled={!allowReleaseOtherLocks} on:change={handleReleaseAllTheirs} />
				</TableHeadCell>
				<TableHeadCell class="p-2">Path</TableHeadCell>
				<TableHeadCell class="p-2">Owner</TableHeadCell>
				<TableHeadCell class="p-2">Locked At</TableHeadCell>
			</TableHead>
			<TableBody>
				{#each filteredTheirs as lock, index}
					<TableBodyRow
						class="text-left border-b-0 p-2 {index % 2 === 0
							? 'bg-secondary-700 dark:bg-space-900'
							: 'bg-secondary-800 dark:bg-space-950'}"
					>
						<TableBodyCell class="!p-2">
							<Checkbox
								disabled={!allowReleaseOtherLocks && !selectedForRelease.includes(lock.path)}
								checked={selectedForRelease.includes(lock.path)}
								on:change={(e) => {
									handleRelease(e, lock.path, false);
								}}
							/>
						</TableBodyCell>
						<TableBodyCell id="lock-{index}" class="p-2">
							{getLockDisplayName(lock)}
						</TableBodyCell>
						<TableBodyCell class="p-2">
							{lock.owner?.name}
						</TableBodyCell>
						<TableBodyCell class="p-2">
							{getLockTimestamp(lock.locked_at)}
						</TableBodyCell>
					</TableBodyRow>
					<Tooltip
						triggeredBy="#lock-{index}"
						class="w-auto text-xs text-primary-400 bg-secondary-700 dark:bg-space-900"
						placement="top"
						>{lock.path}
					</Tooltip>
				{:else}
					<TableBodyRow>
						<TableBodyCell class="p-2" />
						<TableBodyCell class="p-2">No locks found!</TableBodyCell>
					</TableBodyRow>
				{/each}
			</TableBody>
		</Table>
	</Card>
{/if}

<ProgressModal bind:showModal={showProgressModal} title="Updating locks" />
