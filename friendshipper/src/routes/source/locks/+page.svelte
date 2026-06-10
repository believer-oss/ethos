<script lang="ts">
	import {
		Button,
		Card,
		Checkbox,
		Modal,
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

	let showOtherLocks = false;
	let allowReleaseOtherLocks = false;
	let searchTerm = '';

	let showProgressModal: boolean = false;

	// unlock preview
	let showUnlockPreview = false;
	let unlockPreviewOurs: Lock[] = [];
	let unlockPreviewTheirs: Lock[] = [];
	let unlockPreviewSkipped = 0;

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
			item.path.toLowerCase().includes(search) ||
			(item.display_name ?? '').toLowerCase().includes(search)
		);
	});
	$: filteredTheirs = $sortedTheirs.filter((item): boolean => {
		const search = searchTerm.toLowerCase();
		return (
			item.path.toLowerCase().includes(search) ||
			(item.display_name ?? '').toLowerCase().includes(search) ||
			(item.owner?.name ?? '').toLowerCase().includes(search)
		);
	});
	// Derived from the filtered list so the button only releases locks the
	// user can currently see.
	$: unmodifiedLockedFiles = filteredOurs.filter(
		(lock) => !$allModifiedFiles.find((file) => file.path === lock.path)
	);

	// Selection persists across filter changes by design — users build a
	// release list over multiple searches and review it in the unlock preview.
	// Paths that no longer exist in either lock list are pruned, though, so
	// the selection can't accumulate ghosts after releases or refreshes. The
	// guarded assignment keeps this from re-triggering itself.
	$: {
		const live = new Set([...$sortedOurs, ...$sortedTheirs].map((lock) => lock.path));
		const pruned = selectedForRelease.filter((path) => live.has(path));
		if (pruned.length !== selectedForRelease.length) {
			selectedForRelease = pruned;
		}
	}

	// Derived from the selection rather than tracked imperatively so it can't
	// drift when the selection is cleared or rebuilt. Gated on the release
	// toggle so the red warning only shows when others' locks would actually
	// be freed — with the toggle off, the preview excludes them entirely.
	$: numOthersSelected = allowReleaseOtherLocks
		? selectedForRelease.filter((path) => $sortedTheirs.some((lock) => lock.path === path)).length
		: 0;

	const handleRelease = (e: Event, path: string) => {
		if ((e.target as HTMLInputElement).checked) {
			selectedForRelease = [...selectedForRelease, path];
		} else {
			selectedForRelease = selectedForRelease.filter((item) => item !== path);
		}
	};

	// Select-all only operates on the rows currently visible through the
	// search filter, so a filtered view can't silently select hidden locks.
	const handleReleaseAllTheirs = (e: Event) => {
		if (!allowReleaseOtherLocks) return;

		const paths = filteredTheirs.map((lock) => lock.path);
		if ((e.target as HTMLInputElement).checked) {
			selectedForRelease = [...new Set([...selectedForRelease, ...paths])];
		} else {
			selectedForRelease = selectedForRelease.filter((path) => !paths.includes(path));
		}
	};

	const handleReleaseAllOurs = (e: Event) => {
		const paths = filteredOurs.map((lock) => lock.path);
		if ((e.target as HTMLInputElement).checked) {
			selectedForRelease = [...new Set([...selectedForRelease, ...paths])];
		} else {
			selectedForRelease = selectedForRelease.filter((path) => !paths.includes(path));
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

	// Every release flows through a preview modal: the selection can be built
	// across multiple searches (and may include rows hidden by the current
	// filter), so the preview is where the user sees exactly which locks will
	// be freed before anything happens.
	const openUnlockPreview = (paths: string[]) => {
		// Resolve against the live lock lists so the preview reflects what will
		// actually be freed right now. Paths that are no longer locked — or
		// others' locks while the release toggle is off — are skipped by the
		// backend, so they're excluded here and surfaced as a skip count.
		unlockPreviewOurs = $sortedOurs.filter((lock) => paths.includes(lock.path));
		unlockPreviewTheirs = allowReleaseOtherLocks
			? $sortedTheirs.filter((lock) => paths.includes(lock.path))
			: [];
		unlockPreviewSkipped = paths.length - unlockPreviewOurs.length - unlockPreviewTheirs.length;
		showUnlockPreview = true;
	};

	const handleReleaseSelected = () => {
		if (selectedForRelease.length === 0) return;
		openUnlockPreview(selectedForRelease);
	};

	const handleReleaseUnmodified = () => {
		if (unmodifiedLockedFiles.length === 0) return;
		openUnlockPreview(unmodifiedLockedFiles.map((lock) => lock.path));
	};

	const handleConfirmUnlock = async () => {
		showUnlockPreview = false;

		const oursPaths = unlockPreviewOurs.map((lock) => lock.path);
		const theirsPaths = unlockPreviewTheirs.map((lock) => lock.path);
		if (oursPaths.length === 0 && theirsPaths.length === 0) return;

		loading = true;
		showProgressModal = true;
		try {
			// Two separate calls so force never applies to our own paths: if one
			// of our locks changed hands while the preview sat open, the
			// non-force call drops it instead of stealing the new owner's lock.
			// Force is scoped to exactly the locks the user reviewed in the red
			// others' section.
			if (oursPaths.length > 0) {
				await releaseLocks(oursPaths, false);
			}
			if (theirsPaths.length > 0) {
				await releaseLocks(theirsPaths, true);
			}
		} catch (e) {
			await emit('error', e);
		}
		// Released paths drop out of the selection via the liveness prune once
		// the refresh lands; the rest of the user's selection is preserved.
		await refreshLocks();
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
			>Release Selected (<span class="px-0.5">{selectedForRelease.length}</span>)
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
				<Checkbox
					checked={filteredOurs.length > 0 &&
						filteredOurs.every((lock) => selectedForRelease.includes(lock.path))}
					on:change={handleReleaseAllOurs}
				/>
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
								handleRelease(e, lock.path);
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
					<Checkbox
						disabled={!allowReleaseOtherLocks}
						checked={filteredTheirs.length > 0 &&
							filteredTheirs.every((lock) => selectedForRelease.includes(lock.path))}
						on:change={handleReleaseAllTheirs}
					/>
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
									handleRelease(e, lock.path);
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

<Modal
	open={showUnlockPreview}
	dismissable={true}
	on:close={() => {
		showUnlockPreview = false;
	}}
	class="bg-secondary-700 dark:bg-space-900"
	backdropClass="fixed mt-8 inset-0 z-40 bg-gray-900 bg-opacity-50 dark:bg-opacity-80"
	dialogClass="fixed mt-8 top-0 start-0 end-0 h-modal md:inset-0 md:h-full z-50 w-full p-4 pb-12 flex"
	size="md"
>
	<div class="flex flex-col gap-3">
		<h3 class="text-lg font-semibold text-white">Unlock Preview</h3>
		<p class="text-sm text-gray-400">
			{unlockPreviewOurs.length + unlockPreviewTheirs.length} lock{unlockPreviewOurs.length +
				unlockPreviewTheirs.length ===
			1
				? ''
				: 's'} will be freed.
		</p>
		{#if unlockPreviewOurs.length > 0}
			<p class="text-sm text-gray-300">Your locks ({unlockPreviewOurs.length})</p>
			<div
				class="bg-secondary-800 dark:bg-space-950 p-2 max-h-48 overflow-y-auto rounded text-nowrap"
			>
				{#each unlockPreviewOurs as lock}
					<div class="flex gap-2 items-center" role="listitem">
						<span class="truncate text-gray-300" title={lock.path}>{getLockDisplayName(lock)}</span>
					</div>
				{/each}
			</div>
		{/if}
		{#if unlockPreviewTheirs.length > 0}
			<p class="text-sm font-semibold text-red-500">
				Others' locks ({unlockPreviewTheirs.length}) — these belong to other users!
			</p>
			<div
				class="bg-secondary-800 dark:bg-space-950 p-2 max-h-48 overflow-y-auto rounded text-nowrap"
			>
				{#each unlockPreviewTheirs as lock}
					<div class="flex gap-2 items-center justify-between" role="listitem">
						<span class="truncate text-red-500" title={lock.path}>{getLockDisplayName(lock)}</span>
						<span class="text-xs text-gray-400 shrink-0">{lock.owner?.name}</span>
					</div>
				{/each}
			</div>
		{/if}
		{#if unlockPreviewSkipped > 0}
			<p class="text-xs text-gray-400">
				{unlockPreviewSkipped} selected lock{unlockPreviewSkipped === 1 ? '' : 's'} will be skipped (no
				longer locked, or releasing others' locks is disabled).
			</p>
		{/if}
		<div class="flex justify-end gap-2">
			<Button
				size="sm"
				color="alternative"
				on:click={() => {
					showUnlockPreview = false;
				}}>Cancel</Button
			>
			<Button
				size="sm"
				color={unlockPreviewTheirs.length > 0 ? 'red' : 'primary'}
				disabled={unlockPreviewOurs.length + unlockPreviewTheirs.length === 0}
				on:click={handleConfirmUnlock}
			>
				Release {unlockPreviewOurs.length + unlockPreviewTheirs.length} Lock{unlockPreviewOurs.length +
					unlockPreviewTheirs.length ===
				1
					? ''
					: 's'}
			</Button>
		</div>
	</div>
</Modal>

<ProgressModal bind:showModal={showProgressModal} title="Updating locks" />
