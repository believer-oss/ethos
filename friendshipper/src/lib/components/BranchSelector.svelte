<script lang="ts">
	import { Button, Dropdown, Input, Spinner, Tooltip } from 'flowbite-svelte';
	import { ArrowLeftOutline, CheckOutline, ChevronDownOutline } from 'flowbite-svelte-icons';
	import { emit } from '@tauri-apps/api/event';
	import { createEventDispatcher, tick } from 'svelte';
	import { listBranches } from '$lib/repo';
	import { appConfig } from '$lib/stores';
	import { isQuickSubmitBranch } from '$lib/branches';
	import { fuzzyFilter } from '$lib/fuzzy';
	import { formatRelativeAge } from '$lib/utils';
	import type { BranchInfo } from '$lib/types';

	export let current: string;
	export let disabled = false;

	const dispatch = createEventDispatcher<{ select: string }>();

	const MAX_ROWS = 100;
	const MAX_SUBJECT_LEN = 60;

	let open = false;
	let wasOpen = false;
	let loading = false;
	let loadFailed = false;
	let branches: BranchInfo[] = [];
	let query = '';
	let highlighted = 0;
	let searchWrapper: HTMLElement;
	let listEl: HTMLElement;
	let requestToken = 0;

	$: showAll = $appConfig?.showAllBranches ?? false;
	// Leaving a Quick Submit branch any way but Sync would strand it on origin.
	$: onQuickSubmitBranch = isQuickSubmitBranch(current);
	$: quickSubmitBranchHint = `Sync to return to '${$appConfig?.targetBranch}' from a Quick Submit branch.`;
	$: filtered = fuzzyFilter(branches, query, (b) => [b.name, b.author, b.subject, b.shortSha]);
	$: rows = filtered.slice(0, MAX_ROWS);
	$: if (highlighted >= rows.length) {
		highlighted = 0;
	}

	const load = async (all: boolean) => {
		requestToken += 1;
		const token = requestToken;
		loading = true;
		loadFailed = false;
		try {
			const result = await listBranches(all);
			if (token !== requestToken) {
				return;
			}
			branches = result.filter((b) => !isQuickSubmitBranch(b.name));
		} catch (e) {
			if (token !== requestToken) {
				return;
			}
			branches = [];
			loadFailed = true;
			await emit('error', e);
		} finally {
			if (token === requestToken) {
				loading = false;
			}
		}
	};

	const onOpened = async () => {
		query = '';
		highlighted = 0;
		await tick();
		searchWrapper?.querySelector('input')?.focus();
	};

	$: if (open !== wasOpen) {
		wasOpen = open;
		if (open) {
			void onOpened();
			void load(showAll);
		} else {
			// Invalidate in-flight requests so late responses are ignored.
			requestToken += 1;
			loading = false;
		}
	}

	// Reload when the setting changes while the dropdown is open.
	let lastShowAll = showAll;
	$: if (showAll !== lastShowAll) {
		lastShowAll = showAll;
		if (open) {
			void load(showAll);
		}
	}

	const select = (branch: string) => {
		open = false;
		if (branch !== current) {
			dispatch('select', branch);
		}
	};

	const scrollToHighlighted = async () => {
		await tick();
		listEl?.querySelector(`[data-row="${highlighted}"]`)?.scrollIntoView({ block: 'nearest' });
	};

	const onKeydown = (e: KeyboardEvent) => {
		if (rows.length === 0) {
			return;
		}
		if (e.key === 'ArrowDown') {
			e.preventDefault();
			highlighted = (highlighted + 1) % rows.length;
			void scrollToHighlighted();
		} else if (e.key === 'ArrowUp') {
			e.preventDefault();
			highlighted = (highlighted - 1 + rows.length) % rows.length;
			void scrollToHighlighted();
		} else if (e.key === 'Enter') {
			e.preventDefault();
			select(rows[highlighted].name);
		}
	};

	const resetHighlight = () => {
		highlighted = 0;
	};

	const setHighlight = (i: number) => {
		highlighted = i;
	};

	const truncate = (s: string) =>
		s.length > MAX_SUBJECT_LEN ? `${s.slice(0, MAX_SUBJECT_LEN)}…` : s;
</script>

<div class="flex flex-row items-center gap-2">
	<Button
		id="branch-selector-button"
		size="xs"
		class="bg-secondary-800 dark:bg-space-950 text-white gap-1"
		disabled={disabled || onQuickSubmitBranch}
	>
		<span class="font-semibold">{current === '' ? '(detached)' : current}</span>
		<ChevronDownOutline class="w-3 h-3" />
	</Button>
	{#if onQuickSubmitBranch}
		<Tooltip triggeredBy="#branch-selector-button">{quickSubmitBranchHint}</Tooltip>
	{/if}
	<Dropdown
		bind:open
		triggeredBy="#branch-selector-button"
		placement="bottom-start"
		class="w-[34rem] max-h-96 overflow-y-auto bg-secondary-800 dark:bg-space-950 text-white"
	>
		<div class="p-2" bind:this={searchWrapper}>
			<Input
				type="text"
				size="sm"
				placeholder="Search branches"
				class="text-white bg-secondary-700 dark:bg-space-900 border-gray-400"
				bind:value={query}
				on:input={resetHighlight}
				on:keydown={onKeydown}
			/>
		</div>
		<div class="flex flex-col" bind:this={listEl}>
			{#if loading}
				<div class="flex flex-row items-center gap-2 px-3 py-2 text-sm text-gray-400">
					<Spinner size="4" /> Loading branches…
				</div>
			{/if}
			{#if loadFailed}
				<p class="px-3 py-2 text-sm text-red-400">Could not load branches</p>
			{:else if !loading && rows.length === 0}
				<p class="px-3 py-2 text-sm text-gray-400">No branches found</p>
			{/if}
			{#each rows as b, i (b.name)}
				<button
					type="button"
					data-row={i}
					title={b.message}
					class="flex flex-row items-center gap-2 px-3 py-2 text-left hover:bg-secondary-700 dark:hover:bg-space-900 {i ===
					highlighted
						? 'bg-secondary-700 dark:bg-space-900'
						: ''}"
					on:click={() => {
						select(b.name);
					}}
					on:mouseenter={() => {
						setHighlight(i);
					}}
				>
					<div class="flex flex-col min-w-0 grow">
						<span class="font-semibold text-sm truncate">{b.name}</span>
						<span class="text-xs text-gray-400 truncate">
							{b.shortSha} · {formatRelativeAge(Date.parse(b.commitDate) / 1000)} · {b.author} · {truncate(
								b.subject
							)}
						</span>
					</div>
					{#if b.name === current}
						<CheckOutline class="w-4 h-4 shrink-0 text-primary-400" />
					{/if}
				</button>
			{/each}
			{#if filtered.length > MAX_ROWS}
				<p class="px-3 py-2 text-xs text-gray-400">
					Showing first {MAX_ROWS} — keep typing to narrow
				</p>
			{/if}
		</div>
	</Dropdown>
	<Button
		id="branch-reset-button"
		size="xs"
		class="bg-secondary-800 dark:bg-space-950 text-white"
		disabled={current === $appConfig?.targetBranch || disabled || onQuickSubmitBranch}
		on:click={() => dispatch('select', $appConfig.targetBranch)}
	>
		<ArrowLeftOutline class="w-3 h-3" />
	</Button>
	<Tooltip triggeredBy="#branch-reset-button"
		>{onQuickSubmitBranch
			? quickSubmitBranchHint
			: `Return to ${$appConfig?.targetBranch}`}</Tooltip
	>
</div>
