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
	import { onMount } from 'svelte';
	import { RotateOutline } from 'flowbite-svelte-icons';
	import { emit } from '@tauri-apps/api/event';
	import { unlockFiles, verifyLocks } from '$lib/repo';
	import { locks } from '$lib/stores';

	let loading = false;
	let selectedForRelease: string[] = [];
	let numOthersSelected = 0;

	let showOtherLocks = false;
	let allowReleaseOtherLocks = false;
	let searchTerm = '';

	$: filteredOurs = $locks.ours.filter((item) =>
		item.path.toLowerCase().includes(searchTerm.toLowerCase())
	);
	$: filteredTheirs = $locks.theirs.filter(
		(item) =>
			item.path.toLowerCase().includes(searchTerm.toLowerCase()) ||
			item.owner?.name.toLowerCase().includes(searchTerm.toLowerCase())
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
			const paths = $locks.theirs.map((lock) => lock.path);
			selectedForRelease = selectedForRelease.concat(paths);

			numOthersSelected += $locks.theirs.length;

			selectedForRelease = selectedForRelease.filter(
				(item, index) => selectedForRelease.indexOf(item) === index
			);
		} else {
			numOthersSelected -= $locks.theirs.length;
			selectedForRelease = selectedForRelease.filter(
				(path) => !$locks.theirs.map((lock) => lock.path).includes(path)
			);
		}
	};

	const handleReleaseAllOurs = (e: Event) => {
		if ((e.target as HTMLInputElement).checked) {
			const paths = $locks.ours.map((lock) => lock.path);
			selectedForRelease = selectedForRelease.concat(paths);

			selectedForRelease = selectedForRelease.filter(
				(item, index) => selectedForRelease.indexOf(item) === index
			);
		} else {
			selectedForRelease = selectedForRelease.filter(
				(path) => !$locks.ours.map((lock) => lock.path).includes(path)
			);
		}
	};

	const refreshLocks = async () => {
		loading = true;

		try {
			$locks = await verifyLocks();
		} catch (e) {
			await emit('error', e);
		}

		loading = false;
	};

	const handleReleaseSelected = async () => {
		if (selectedForRelease.length === 0) return;

		loading = true;
		await unlockFiles(selectedForRelease, numOthersSelected > 0);
		await refreshLocks();
		selectedForRelease = [];
		loading = false;
	};

	const formatPath = (path: string) => {
		if (path === '/') return path;
		return path.replace(/\/$/, '').split('/').pop();
	};

	const getLockTimestamp = (locked_at: string): string => {
		const date = new Date(locked_at);
		return date.toLocaleString();
	};

	onMount(() => {
		void refreshLocks();
	});
</script>

<div class="flex items-center justify-between gap-2">
	<div class="flex items-center gap-2">
		<p class="text-2xl my-2 dark:text-primary-400">File Locks</p>
		<Button class="!p-1.5" primary on:click={refreshLocks}>
			<RotateOutline class="w-4 h-4" />
		</Button>
		<Button
			id="release-selected"
			disabled={selectedForRelease.length === 0}
			class="!p-1.5 text-xs"
			color={numOthersSelected > 0 ? 'red' : 'primary'}
			on:click={handleReleaseSelected}
			>Release Selected
		</Button>
		{#if loading}
			<Spinner size="4" />
		{/if}
	</div>
	<div class="flex items-center gap-2">
		{#if showOtherLocks}
			<Toggle bind:checked={allowReleaseOtherLocks}>Release Others' Locks</Toggle>
		{/if}
		<Toggle bind:checked={showOtherLocks}>Show Others' Locks</Toggle>
	</div>
	{#if numOthersSelected > 0}
		<Tooltip
			triggeredBy="#release-selected"
			class="w-auto text-xs text-white dark:bg-red-700"
			placement="right"
			>Warning: This will release other users' locks!
		</Tooltip>
	{/if}
</div>
<Card
	class="w-full min-h-[12rem] p-4 sm:p-4 max-w-full dark:bg-secondary-600 overflow-y-hidden border-0 shadow-none"
>
	<h3 class="text-primary-400 text-xl pb-2">My Locks</h3>
	<TableSearch
		placeholder="Search by file path"
		hoverable={true}
		bind:inputValue={searchTerm}
		color="custom"
		divClass="relative overflow-x-auto sm:rounded-lg"
		innerDivClass="p-2 pt-0 pl-0"
		inputClass="bg-gray-50 border border-gray-300 text-gray-900 text-sm rounded-lg focus:ring-blue-500 focus:border-blue-500 block w-80 p-2 pl-10 dark:bg-gray-700 dark:border-gray-600 dark:placeholder-gray-400 dark:text-white dark:focus:ring-primary-500 dark:focus:border-primary-500"
		striped
	>
		<TableHead class="text-left border-b-0 p-2 bg-secondary-700">
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
						? 'bg-secondary-600'
						: 'bg-secondary-700'}"
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
						{formatPath(lock.path)}
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
					class="w-auto text-xs text-primary-400 bg-white dark:bg-secondary-800"
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
		class="w-full p-4 sm:p-4 my-2 max-w-full dark:bg-secondary-600 border-0 overflow-y-hidden shadow-none"
	>
		<h3 class="text-primary-400 text-xl pb-2">Other Locks</h3>
		<Table color="custom" class="mt-3" striped>
			<TableHead class="text-left border-b-0 p-2 bg-secondary-700">
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
							? 'bg-secondary-600'
							: 'bg-secondary-700'}"
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
							{formatPath(lock.path)}
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
						class="w-auto text-xs text-primary-400 bg-white dark:bg-secondary-800"
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
