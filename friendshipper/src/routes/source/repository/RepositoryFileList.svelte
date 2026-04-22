<script lang="ts">
	import {
		Button,
		Card,
		Input,
		Table,
		TableBody,
		TableBodyCell,
		TableBodyRow,
		TableHead,
		TableHeadCell,
		Tooltip
	} from 'flowbite-svelte';
	import {
		ChevronSortOutline,
		CloseCircleSolid,
		FileCopySolid,
		FolderOpenOutline,
		FolderSolid,
		InfoCircleSolid,
		PenSolid,
		PlusOutline
	} from 'flowbite-svelte-icons';
	import { onMount, tick } from 'svelte';
	import type { RepoDirectoryEntry, RepoFileState } from '$lib/types';

	export let path: string = '';
	export let entries: RepoDirectoryEntry[] = [];
	export let loading: boolean = false;
	export let selectedPath: string | null = null;
	export let onNavigateDirectory: (entry: RepoDirectoryEntry) => void;
	export let onSelectFile: (entry: RepoDirectoryEntry) => void;
	export let onOpenDirectory: (entry: RepoDirectoryEntry) => void;

	let searchInput = '';

	enum SortKey {
		Name = 'name',
		State = 'state'
	}
	let sortDirection = 1;
	let sortKey: SortKey = SortKey.Name;

	const changeSortDirection = (newKey: SortKey) => {
		if (newKey === sortKey) {
			sortDirection = -sortDirection;
		} else {
			sortDirection = 1;
			sortKey = newKey;
		}
	};

	const stateOrder: Record<RepoFileState, number> = {
		conflicted: 0,
		deleted: 1,
		modified: 2,
		added: 3,
		untracked: 4,
		outOfDate: 5,
		unmodified: 6
	};

	const compareEntries = (a: RepoDirectoryEntry, b: RepoDirectoryEntry): number => {
		if (a.kind !== b.kind) {
			return a.kind === 'directory' ? -1 : 1;
		}
		if (sortKey === SortKey.State && a.kind === 'file' && b.kind === 'file') {
			const diff = stateOrder[a.state] - stateOrder[b.state];
			if (diff !== 0) {
				return sortDirection * diff;
			}
		}
		return sortDirection * a.name.toLowerCase().localeCompare(b.name.toLowerCase());
	};

	$: visibleEntries = (
		searchInput === ''
			? [...entries]
			: entries.filter((e) => e.name.toLowerCase().includes(searchInput.toLowerCase()))
	).sort(compareEntries);

	const getStateTextClass = (state: RepoFileState): string => {
		switch (state) {
			case 'added':
				return 'text-lime-500 dark:text-lime-500';
			case 'modified':
				return 'text-yellow-300 dark:text-yellow-300';
			case 'deleted':
				return 'text-red-700 dark:text-red-700';
			case 'conflicted':
				return 'text-red-700 dark:text-red-700';
			case 'untracked':
				return 'text-sky-400 dark:text-sky-400';
			case 'outOfDate':
				return 'text-primary-400 dark:text-primary-400';
			default:
				return '';
		}
	};

	const getStateLabel = (state: RepoFileState): string => {
		switch (state) {
			case 'outOfDate':
				return 'Out of date (remote has newer changes)';
			case 'untracked':
				return 'Untracked (new file, not in git yet)';
			case 'modified':
				return 'Modified';
			case 'added':
				return 'Added (staged)';
			case 'deleted':
				return 'Deleted';
			case 'conflicted':
				return 'Conflicted';
			default:
				return 'Unmodified';
		}
	};

	// Scroll preservation — mirror ModifiedFilesCard pattern, keyed per path.
	let scrollContainerRef: HTMLElement;
	const scrollStorageKey = (p: string) => `repo-browser-scroll:${p}`;
	let isRestoringScroll = false;
	let prevPath = path;

	const saveScrollPosition = () => {
		if (isRestoringScroll || !scrollContainerRef) return;
		const rows = scrollContainerRef.querySelectorAll('[data-entry-path]');
		const containerTop = scrollContainerRef.getBoundingClientRect().top;
		for (const row of rows) {
			const rect = row.getBoundingClientRect();
			const relativeTop = rect.top - containerTop;
			if (relativeTop >= 0 && relativeTop < scrollContainerRef.clientHeight) {
				const entryPath = row.getAttribute('data-entry-path');
				if (entryPath) {
					sessionStorage.setItem(
						scrollStorageKey(path),
						JSON.stringify({ entryPath, offset: relativeTop })
					);
					break;
				}
			}
		}
	};

	const restoreScrollPosition = async () => {
		await tick();
		if (!scrollContainerRef) return;
		const saved = sessionStorage.getItem(scrollStorageKey(path));
		if (!saved) {
			scrollContainerRef.scrollTop = 0;
			return;
		}
		try {
			const { entryPath, offset } = JSON.parse(saved);
			const rows = scrollContainerRef.querySelectorAll('[data-entry-path]');
			let targetRow: Element | null = null;
			for (const row of rows) {
				if (row.getAttribute('data-entry-path') === entryPath) {
					targetRow = row;
					break;
				}
			}
			if (targetRow) {
				isRestoringScroll = true;
				const containerTop = scrollContainerRef.getBoundingClientRect().top;
				const rowTop = targetRow.getBoundingClientRect().top;
				const currentOffset = rowTop - containerTop;
				scrollContainerRef.scrollTop += currentOffset - offset;
				setTimeout(() => {
					isRestoringScroll = false;
				}, 100);
			}
		} catch {
			// ignore
		}
	};

	$: if (path !== prevPath) {
		prevPath = path;
		searchInput = '';
		setTimeout(() => {
			void restoreScrollPosition();
		}, 50);
	}

	onMount(() => {
		if (scrollContainerRef) {
			scrollContainerRef.addEventListener('scroll', saveScrollPosition);
			setTimeout(() => {
				void restoreScrollPosition();
			}, 100);
		}
		return () => {
			if (scrollContainerRef) {
				scrollContainerRef.removeEventListener('scroll', saveScrollPosition);
			}
		};
	});
</script>

<Card
	class="w-full relative p-4 sm:p-4 max-w-full bg-secondary-700 dark:bg-space-900 h-full overflow-y-hidden border-0 shadow-none"
>
	<div class="flex justify-between items-center gap-2 pb-2">
		<div class="flex gap-2 items-center">
			<h3 class="text-primary-400 text-xl">Files</h3>
			<span class="text-xs text-gray-400 font-italic">({visibleEntries.length})</span>
			{#if loading}
				<span class="text-xs text-gray-400 font-italic">loading…</span>
			{/if}
		</div>
	</div>
	<div class="flex gap-2 pb-1">
		<Input
			class="w-full h-8 text-white bg-secondary-800 dark:bg-space-950"
			bind:value={searchInput}
			placeholder="Filter this folder"
		/>
	</div>
	<div bind:this={scrollContainerRef} class="overflow-y-auto pr-1 h-full">
		<Table color="custom" striped={true}>
			<TableHead class="w-full border-b-0 p-2 bg-secondary-800 dark:bg-space-950">
				<TableHeadCell class="p-1 w-10" />
				<TableHeadCell class="p-1 w-8" />
				<TableHeadCell
					class="p-1 cursor-pointer"
					on:click={() => {
						changeSortDirection(SortKey.State);
					}}
				>
					<div class="flex items-center">
						<ChevronSortOutline class="text-gray-400" size="xs" />
					</div>
				</TableHeadCell>
				<TableHeadCell
					class="p-1 cursor-pointer"
					on:click={() => {
						changeSortDirection(SortKey.Name);
					}}
				>
					<div class="flex items-center gap-2">
						Name
						<ChevronSortOutline class="text-gray-400" size="xs" />
					</div>
				</TableHeadCell>
			</TableHead>
			<TableBody>
				{#each visibleEntries as entry, i (entry.path)}
					{@const isSelected = selectedPath === entry.path}
					{@const stripeClass =
						i % 2 === 0
							? 'bg-secondary-800 dark:bg-space-950'
							: 'bg-secondary-700 dark:bg-space-900'}
					{@const rowClass = isSelected ? 'bg-primary-800 dark:bg-primary-900' : stripeClass}
					<TableBodyRow
						data-entry-path={entry.path}
						class="text-left border-b-0 cursor-pointer {rowClass}"
						on:click={() => {
							if (entry.kind === 'directory') {
								onNavigateDirectory(entry);
							} else {
								onSelectFile(entry);
							}
						}}
					>
						<TableBodyCell tdClass="p-1 w-10">
							<Button
								outline
								size="xs"
								class="p-1 border-0 focus-within:ring-0 dark:focus-within:ring-0"
								on:click={(e) => {
									e.stopPropagation();
									onOpenDirectory(entry);
								}}
							>
								<FolderOpenOutline class="w-4 h-4" />
							</Button>
							<Tooltip
								class="w-auto bg-secondary-600 dark:bg-space-800 font-semibold shadow-2xl"
								placement="right">Open in file explorer</Tooltip
							>
						</TableBodyCell>
						<TableBodyCell tdClass="p-1 w-8">
							{#if entry.kind === 'directory'}
								<FolderSolid class="w-4 h-4 text-primary-400" />
							{:else}
								<FileCopySolid class="w-4 h-4 text-gray-400" />
							{/if}
						</TableBodyCell>
						<TableBodyCell tdClass="p-1 w-8">
							{#if entry.kind === 'file'}
								{#if entry.state === 'added'}
									<PlusOutline class="w-4 h-4 text-lime-500" />
									<Tooltip
										class="w-auto bg-secondary-600 dark:bg-space-800 font-semibold shadow-2xl"
										placement="right">{getStateLabel(entry.state)}</Tooltip
									>
								{:else if entry.state === 'modified'}
									<PenSolid class="w-4 h-4 text-yellow-300" />
									<Tooltip
										class="w-auto bg-secondary-600 dark:bg-space-800 font-semibold shadow-2xl"
										placement="right">{getStateLabel(entry.state)}</Tooltip
									>
								{:else if entry.state === 'deleted'}
									<CloseCircleSolid class="w-4 h-4 text-red-700" />
									<Tooltip
										class="w-auto bg-secondary-600 dark:bg-space-800 font-semibold shadow-2xl"
										placement="right">{getStateLabel(entry.state)}</Tooltip
									>
								{:else if entry.state === 'conflicted'}
									<FileCopySolid class="w-4 h-4 text-red-700" />
									<Tooltip
										class="w-auto bg-secondary-600 dark:bg-space-800 font-semibold shadow-2xl"
										placement="right">{getStateLabel(entry.state)}</Tooltip
									>
								{:else if entry.state === 'untracked'}
									<PlusOutline class="w-4 h-4 text-sky-400" />
									<Tooltip
										class="w-auto bg-secondary-600 dark:bg-space-800 font-semibold shadow-2xl"
										placement="right">{getStateLabel(entry.state)}</Tooltip
									>
								{:else if entry.state === 'outOfDate'}
									<InfoCircleSolid class="w-4 h-4 text-primary-400" />
									<Tooltip
										class="w-auto bg-secondary-600 dark:bg-space-800 font-semibold shadow-2xl"
										placement="right">{getStateLabel(entry.state)}</Tooltip
									>
								{/if}
							{/if}
						</TableBodyCell>
						<TableBodyCell
							class="p-1 whitespace-nowrap font-medium {getStateTextClass(entry.state)}"
						>
							{entry.name}
						</TableBodyCell>
					</TableBodyRow>
				{:else}
					<TableBodyRow class="text-center border-b-0 bg-secondary-700 dark:bg-space-900">
						<TableBodyCell class="p-2" colspan="4">
							<p class="text-gray-300">
								{loading ? 'Loading…' : 'This folder is empty.'}
							</p>
						</TableBodyCell>
					</TableBodyRow>
				{/each}
			</TableBody>
		</Table>
	</div>
</Card>
