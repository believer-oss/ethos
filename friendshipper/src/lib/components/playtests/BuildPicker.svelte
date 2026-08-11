<script lang="ts">
	import {
		ChevronDownOutline,
		ChevronLeftOutline,
		ChevronRightOutline
	} from 'flowbite-svelte-icons';
	import { tick } from 'svelte';
	import type { ArtifactEntry, CommitWorkflowInfo } from '$lib/types';
	import { cleanBranchName, formatRelativeAge, isTrunkBuild } from '$lib/utils';

	export let versions: ArtifactEntry[];
	export let selectedSha: string;
	export let workflowMap: Map<string, CommitWorkflowInfo>;
	export let trunkBranch: string;
	export let currentUserDisplayName: string;
	export let originalSha: string | null = null;
	// Rendered in the panel footer; beside the summary button a long message pushes it out of view.
	export let onManualEntry: (() => void) | null = null;

	// Sentinel key for the unknown group; `cleanBranchName` never produces this name.
	const UNKNOWN_SECTION = '__unknown__';

	interface Grouped {
		trunk: ArtifactEntry[];
		branches: Map<string, ArtifactEntry[]>; // key = cleaned branch name, case-sensitive
		unknown: ArtifactEntry[];
	}

	type BuildTone = 'trunk' | 'branch' | 'unknown';

	interface SummaryState {
		label: string;
		tone: string;
	}

	let expanded = false;
	let activePane: 'main' | 'branches' = 'main';
	let expandedBranches: Set<string> = new Set();

	const groupVersions = (
		entries: ArtifactEntry[],
		wfMap: Map<string, CommitWorkflowInfo>,
		trunk: string
	): Grouped => {
		const trunkList: ArtifactEntry[] = [];
		const branchMap = new Map<string, ArtifactEntry[]>();
		const unknownList: ArtifactEntry[] = [];

		for (const entry of entries) {
			const branch = wfMap.get(entry.commit)?.branch;
			if (!branch) {
				unknownList.push(entry);
			} else if (isTrunkBuild(branch, trunk)) {
				trunkList.push(entry);
			} else {
				const branchName = cleanBranchName(branch);
				const existing = branchMap.get(branchName);
				if (existing) {
					existing.push(entry);
				} else {
					branchMap.set(branchName, [entry]);
				}
			}
		}

		const byRecencyThenSha = (a: ArtifactEntry, b: ArtifactEntry): number =>
			b.lastModified - a.lastModified || (a.commit < b.commit ? -1 : 1);

		trunkList.sort(byRecencyThenSha);
		unknownList.sort(byRecencyThenSha);
		for (const list of branchMap.values()) {
			list.sort(byRecencyThenSha);
		}

		return { trunk: trunkList, branches: branchMap, unknown: unknownList };
	};

	// `commit` is `Option<String>` in Rust but `string` in types.ts; a throw inside the keyed
	// `{#each}` would blank the whole panel rather than skip one row.
	const shortSha = (sha: string | null | undefined): string => (sha ?? '').substring(0, 8);

	// Both sides must be non-empty — `'' === ''` would badge every branch "yours".
	const isMine = (
		entries: ArtifactEntry[],
		wfMap: Map<string, CommitWorkflowInfo>,
		user: string
	): boolean => {
		const normalizedUser = user.trim().toLowerCase();
		if (!normalizedUser) return false;
		return entries.some((e) => {
			const pusher = wfMap.get(e.commit)?.pusher?.trim().toLowerCase();
			return !!pusher && pusher === normalizedUser;
		});
	};

	const orderedBranchNames = (
		branchMap: Map<string, ArtifactEntry[]>,
		wfMap: Map<string, CommitWorkflowInfo>,
		user: string
	): string[] =>
		Array.from(branchMap.entries())
			.sort(([aName, aEntries], [bName, bEntries]) => {
				const aMine = isMine(aEntries, wfMap, user);
				const bMine = isMine(bEntries, wfMap, user);
				if (aMine !== bMine) return aMine ? -1 : 1;

				// Entry arrays are already sorted newest-first by groupVersions.
				const aRecency = aEntries[0].lastModified;
				const bRecency = bEntries[0].lastModified;
				if (aRecency !== bRecency) return bRecency - aRecency;

				return aName.toLowerCase().localeCompare(bName.toLowerCase());
			})
			.map(([branchName]) => branchName);

	// Always via `isTrunkBuild`, so this and PlaytestModal's default selection cannot drift apart.
	const classifyBuild = (
		sha: string,
		wfMap: Map<string, CommitWorkflowInfo>,
		trunk: string
	): BuildTone => {
		const branch = wfMap.get(sha)?.branch;
		if (!branch) return 'unknown';
		return isTrunkBuild(branch, trunk) ? 'trunk' : 'branch';
	};

	const toneClass = (tone: BuildTone): string => {
		if (tone === 'trunk') return 'text-green-400';
		if (tone === 'branch') return 'text-blue-400';
		return 'text-gray-400';
	};

	const summaryState = (
		entries: ArtifactEntry[],
		sha: string,
		wfMap: Map<string, CommitWorkflowInfo>,
		trunk: string
	): SummaryState => {
		// Sha before emptiness: a pinned selection must still show when the build list fails to load.
		if (sha) {
			if (!entries.some((e) => e.commit === sha)) {
				return { label: `Not in list: ${shortSha(sha)}`, tone: 'text-amber-400' };
			}
			return { label: shortSha(sha), tone: toneClass(classifyBuild(sha, wfMap, trunk)) };
		}
		if (entries.length === 0) return { label: 'No builds available', tone: 'text-gray-400' };
		return { label: 'No build selected', tone: 'text-gray-400' };
	};

	// `formatRelativeAge` returns an absolute date past ~30 days, which "is {age} old" would mangle.
	const RELATIVE_AGE_PATTERN = /^\d+[mhd]$/;

	const trunkAnchor = (newest: ArtifactEntry | undefined, trunk: string): string => {
		if (!newest) return `no builds on ${trunk}`;
		const age = formatRelativeAge(newest.lastModified);
		if (age === 'just now') return `${trunk} built just now`;
		if (RELATIVE_AGE_PATTERN.test(age)) return `${trunk} is ${age} old`;
		return `${trunk} last built ${age}`;
	};

	$: grouped = groupVersions(versions, workflowMap, trunkBranch);
	$: branchOrder = orderedBranchNames(grouped.branches, workflowMap, currentUserDisplayName);
	$: selectedEntry = versions.find((v) => v.commit === selectedSha);
	$: originalEntry = versions.find((v) => v.commit === originalSha);
	$: summary = summaryState(versions, selectedSha, workflowMap, trunkBranch);
	// Zero exactly when there is nothing at all behind the branch rail.
	$: branchSectionCount = grouped.branches.size + (grouped.unknown.length > 0 ? 1 : 0);
	$: mainRailAnchor = trunkAnchor(grouped.trunk[0], trunkBranch);

	const collapsePanel = () => {
		expanded = false;
		activePane = 'main';
		expandedBranches = new Set();
	};

	// Reseeded on every open, so the user's own sections start expanded each time.
	const expandPanel = () => {
		expanded = true;
		expandedBranches = new Set(
			Array.from(grouped.branches.entries())
				.filter(([, entries]) => isMine(entries, workflowMap, currentUserDisplayName))
				.map(([branchName]) => branchName)
		);
	};

	const handleManualEntry = () => {
		collapsePanel();
		onManualEntry?.();
	};

	const toggleExpanded = () => {
		if (expanded) {
			collapsePanel();
		} else {
			expandPanel();
		}
	};

	// Swapping panes unmounts the rail that was just clicked, dropping focus to document.body.
	// Move focus to the rail replacing it.
	let mainRailEl: HTMLButtonElement | null = null;
	let branchRailEl: HTMLButtonElement | null = null;

	// Named handlers: an inline arrow returning an assignment trips `no-return-assign`.
	const showBranchesPane = () => {
		activePane = 'branches';
		void tick().then(() => {
			mainRailEl?.focus();
		});
	};

	const showMainPane = () => {
		activePane = 'main';
		void tick().then(() => {
			branchRailEl?.focus();
		});
	};

	const toggleBranch = (branchName: string) => {
		const next = new Set(expandedBranches);
		if (next.has(branchName)) {
			next.delete(branchName);
		} else {
			next.add(branchName);
		}
		expandedBranches = next; // reassign — Svelte does not track Set mutation
	};

	const selectEntry = (sha: string) => {
		selectedSha = sha;
		collapsePanel();
	};

	const selectOriginal = () => {
		if (originalSha) selectEntry(originalSha);
	};

	const messageFor = (sha: string): string => workflowMap.get(sha)?.message ?? '';

	// Capture on window, not bubble on our root: swapping panes drops focus to document.body, so a
	// root handler stops firing. Capture also runs before flowbite Modal's Escape handler, which
	// would otherwise close the whole dialog. Left untouched while collapsed.
	const handleWindowKeydown = (event: KeyboardEvent) => {
		if (!expanded || event.key !== 'Escape') return;
		event.preventDefault();
		event.stopPropagation();
		collapsePanel();
	};
</script>

<svelte:window on:keydown|capture={handleWindowKeydown} />

<div class="w-full">
	<!-- Not disabled when the list is empty: the panel must still open to reach the manual-entry
	     footer. No aria-label either - it would override the button's text and hide the sha. -->
	<button
		type="button"
		aria-expanded={expanded}
		class="flex w-full flex-row items-center gap-2 rounded-lg border border-secondary-600 px-2.5 py-1.5 text-left text-xs dark:border-space-800 bg-secondary-700 dark:bg-space-900"
		on:click={toggleExpanded}
	>
		<span class="shrink-0 font-mono {summary.tone}">{summary.label}</span>
		{#if selectedEntry}
			<span class="min-w-0 flex-1 truncate text-gray-300">{messageFor(selectedEntry.commit)}</span>
			<span class="w-12 shrink-0 text-right text-gray-400">
				{formatRelativeAge(selectedEntry.lastModified)}
			</span>
		{:else}
			<span class="min-w-0 flex-1" />
		{/if}
		<ChevronDownOutline size="xs" class="shrink-0 text-gray-400" />
	</button>

	{#if expanded}
		<div
			class="mt-1 flex flex-row overflow-hidden rounded-lg border border-secondary-600 dark:border-space-800 bg-secondary-800 dark:bg-space-950"
		>
			{#if activePane === 'main'}
				<div class="max-h-64 min-w-0 flex-1 overflow-y-auto p-1">
					{#if originalSha !== null && originalSha !== selectedSha}
						<button
							type="button"
							class="mb-1 flex w-full flex-row items-center gap-2 rounded border-b border-secondary-600 px-2 py-1 text-left text-xs dark:border-space-800 hover:bg-secondary-700 dark:hover:bg-space-900"
							on:click={selectOriginal}
						>
							<span class="shrink-0 rounded bg-space-800 px-1 text-[10px] uppercase text-gray-300">
								Current
							</span>
							<span class="shrink-0 font-mono text-gray-200">{shortSha(originalSha)}</span>
							<span class="min-w-0 flex-1 truncate text-gray-400">{messageFor(originalSha)}</span>
							<span class="w-12 shrink-0 text-right text-gray-500">
								{originalEntry ? formatRelativeAge(originalEntry.lastModified) : ''}
							</span>
						</button>
					{/if}

					{#if grouped.trunk.length === 0}
						<div class="px-2 py-1 text-xs italic text-gray-500">
							No builds on {trunkBranch} yet.
						</div>
					{:else}
						{#each grouped.trunk as entry (entry.key)}
							{@const isSelected = entry.commit === selectedSha}
							<!-- The ring marks the selection: the selected background matches every row's own
							     hover background, so it alone vanishes under the pointer. -->
							<button
								type="button"
								aria-current={isSelected ? 'true' : undefined}
								class="flex w-full flex-row items-center gap-2 rounded px-2 py-1 text-left text-xs hover:bg-secondary-700 dark:hover:bg-space-900"
								class:bg-secondary-700={isSelected}
								class:dark:bg-space-900={isSelected}
								class:ring-1={isSelected}
								class:ring-inset={isSelected}
								class:ring-primary-400={isSelected}
								on:click={() => {
									selectEntry(entry.commit);
								}}
							>
								<span class="shrink-0 font-mono text-green-400">{shortSha(entry.commit)}</span>
								<span class="min-w-0 flex-1 truncate text-gray-300">{messageFor(entry.commit)}</span
								>
								<span class="w-12 shrink-0 text-right text-gray-400">
									{formatRelativeAge(entry.lastModified)}
								</span>
							</button>
						{/each}
					{/if}
				</div>

				<button
					bind:this={branchRailEl}
					type="button"
					aria-label="Show branch builds ({branchSectionCount})"
					class="flex w-[34px] shrink-0 flex-col items-center justify-center gap-1 border-l border-secondary-600 py-2 dark:border-space-800 hover:bg-secondary-700 dark:hover:bg-space-900"
					on:click={showBranchesPane}
				>
					<ChevronRightOutline size="xs" class="text-blue-400" />
					<span class="rail-label text-[10px] text-blue-400">Branches</span>
					<span class="text-[10px] text-gray-400">{branchSectionCount}</span>
				</button>
			{:else}
				<button
					bind:this={mainRailEl}
					type="button"
					aria-label="Show {trunkBranch} builds — {mainRailAnchor}"
					class="flex w-[34px] shrink-0 flex-col items-center justify-center gap-1 border-r border-secondary-600 py-2 dark:border-space-800 hover:bg-secondary-700 dark:hover:bg-space-900"
					on:click={showMainPane}
				>
					<ChevronLeftOutline size="xs" class="text-green-400" />
					<span class="rail-label text-[10px] text-green-400">{mainRailAnchor}</span>
				</button>

				<div class="max-h-64 min-w-0 flex-1 overflow-y-auto p-1">
					{#if grouped.branches.size === 0 && grouped.unknown.length === 0}
						<div class="px-2 py-1 text-xs italic text-gray-500">No branch builds found.</div>
					{:else}
						{#each branchOrder as branchName (branchName)}
							{@const entries = grouped.branches.get(branchName) ?? []}
							{@const mine = isMine(entries, workflowMap, currentUserDisplayName)}
							<button
								type="button"
								aria-expanded={expandedBranches.has(branchName)}
								class="flex w-full flex-row items-center gap-2 rounded px-2 py-1 text-left text-xs hover:bg-secondary-700 dark:hover:bg-space-900"
								on:click={() => {
									toggleBranch(branchName);
								}}
							>
								<span class="shrink-0 text-gray-400">
									{expandedBranches.has(branchName) ? '−' : '+'}
								</span>
								<span class="min-w-0 truncate font-medium text-blue-400">{branchName}</span>
								{#if mine}
									<span
										class="shrink-0 rounded border border-primary-400/40 px-1 text-[9px] uppercase tracking-wide text-primary-400"
									>
										yours
									</span>
								{:else}
									{@const pusher = workflowMap.get(entries[0]?.commit ?? '')?.pusher}
									{#if pusher}
										<span class="shrink-0 text-[10px] text-gray-400">@{pusher}</span>
									{/if}
								{/if}
								<span class="ml-auto shrink-0 text-[10px] text-gray-400">{entries.length}</span>
							</button>
							{#if expandedBranches.has(branchName)}
								<!-- Indented behind a hairline so builds read as belonging to their branch header. -->
								<div class="mb-1 ml-3 border-l border-secondary-600 pl-1 dark:border-space-700">
									{#each entries as entry (entry.key)}
										{@const isSelected = entry.commit === selectedSha}
										<button
											type="button"
											aria-current={isSelected ? 'true' : undefined}
											class="flex w-full flex-row items-center gap-2 rounded px-2 py-1 text-left text-xs hover:bg-secondary-700 dark:hover:bg-space-900"
											class:bg-secondary-700={isSelected}
											class:dark:bg-space-900={isSelected}
											class:ring-1={isSelected}
											class:ring-inset={isSelected}
											class:ring-primary-400={isSelected}
											on:click={() => {
												selectEntry(entry.commit);
											}}
										>
											<span class="shrink-0 font-mono text-blue-400">
												{shortSha(entry.commit)}
											</span>
											<span class="min-w-0 flex-1 truncate text-gray-300">
												{messageFor(entry.commit)}
											</span>
											<span class="w-12 shrink-0 text-right text-gray-400">
												{formatRelativeAge(entry.lastModified)}
											</span>
										</button>
									{/each}
								</div>
							{/if}
						{/each}

						{#if grouped.unknown.length > 0}
							<button
								type="button"
								aria-expanded={expandedBranches.has(UNKNOWN_SECTION)}
								class="flex w-full flex-row items-center gap-2 rounded px-2 py-1 text-left text-xs hover:bg-secondary-700 dark:hover:bg-space-900"
								on:click={() => {
									toggleBranch(UNKNOWN_SECTION);
								}}
							>
								<span class="shrink-0 text-gray-400">
									{expandedBranches.has(UNKNOWN_SECTION) ? '−' : '+'}
								</span>
								<span class="min-w-0 truncate font-medium text-gray-400">No branch found</span>
								<span class="ml-auto shrink-0 text-[10px] text-gray-400">
									{grouped.unknown.length}
								</span>
							</button>
							{#if expandedBranches.has(UNKNOWN_SECTION)}
								<div class="mb-1 ml-3 border-l border-secondary-600 pl-1 dark:border-space-700">
									<p class="px-2 pb-1 text-[10px] leading-tight text-gray-500">
										These builds have no branch info attached — nothing is wrong with them.
									</p>
									{#each grouped.unknown as entry (entry.key)}
										{@const isSelected = entry.commit === selectedSha}
										<button
											type="button"
											aria-current={isSelected ? 'true' : undefined}
											class="flex w-full flex-row items-center gap-2 rounded px-2 py-1 text-left text-xs hover:bg-secondary-700 dark:hover:bg-space-900"
											class:bg-secondary-700={isSelected}
											class:dark:bg-space-900={isSelected}
											class:ring-1={isSelected}
											class:ring-inset={isSelected}
											class:ring-primary-400={isSelected}
											on:click={() => {
												selectEntry(entry.commit);
											}}
										>
											<span class="shrink-0 font-mono text-gray-400">
												{shortSha(entry.commit)}
											</span>
											<span class="min-w-0 flex-1 truncate text-gray-300">
												{messageFor(entry.commit)}
											</span>
											<span class="w-12 shrink-0 text-right text-gray-400">
												{formatRelativeAge(entry.lastModified)}
											</span>
										</button>
									{/each}
								</div>
							{/if}
						{/if}
					{/if}
				</div>
			{/if}
		</div>

		{#if onManualEntry}
			<div class="mt-1 flex flex-row justify-end">
				<button
					type="button"
					class="rounded px-1 text-[10px] text-primary-400 underline underline-offset-2 hover:text-primary-300"
					on:click={handleManualEntry}
				>
					Enter a commit manually
				</button>
			</div>
		{/if}
	{/if}
</div>

<style>
	/* Only vertical text in the product; a plain CSS property in a scoped block is more portable
	   across WebKitGTK (Linux) and WebView2 (Windows) than a Tailwind arbitrary-value class. */
	.rail-label {
		writing-mode: vertical-rl;
		text-orientation: mixed;
		white-space: nowrap;
	}
</style>
