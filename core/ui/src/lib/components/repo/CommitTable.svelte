<script lang="ts">
	import {
		Button,
		Spinner,
		TableSearch,
		TableBody,
		TableBodyCell,
		TableBodyRow,
		TableHead,
		TableHeadCell,
		Tooltip
	} from 'flowbite-svelte';
	import { ChevronDownOutline, ChevronRightOutline, ChevronUpOutline } from 'flowbite-svelte-icons';
	import type { Commit, CommitFileInfo, Nullable } from '$lib/types/index.js';

	export let commits: Commit[];
	export let latestLocalCommit: Nullable<Commit> = null;
	export let showBuildStatus: boolean = false;

	export let showFilesHandler: (commit: string, stash: boolean) => Promise<CommitFileInfo[]>;

	let searchTerm = '';

	// commit file details
	let expandedCommit = '';
	let loadingCommitFiles = false;
	let commitFiles: CommitFileInfo[] = [];

	$: filteredCommits = commits.filter((commit) => {
		const searchTerms = searchTerm
			.toLowerCase()
			.split(' ')
			.filter((term) => term.length > 0);

		return searchTerms.every(
			(term) =>
				commit.author.toLowerCase().includes(term) ||
				commit.message.toLowerCase().includes(term) ||
				commit.sha.toLowerCase().includes(term)
		);
	});

	const getFileDisplayName = (file: CommitFileInfo): string => {
		if (file.displayName === '') {
			return file.file;
		}
		return file.displayName;
	};

	const isCommitLatestLocal = (sha: string): boolean => {
		if (commits.length === 0) {
			return false;
		}

		return sha === latestLocalCommit?.sha;
	};

	const setExpandedCommit = async (commit: string) => {
		expandedCommit = commit;

		if (commit === '') {
			commitFiles = [];
			return;
		}

		loadingCommitFiles = true;
		commitFiles = await showFilesHandler(commit, false);
		commitFiles.sort((a, b) => (getFileDisplayName(a) < getFileDisplayName(b) ? -1 : 1));
		loadingCommitFiles = false;
	};

	const getCommitFileTextClass = (action: string): string => {
		if (action === 'M') {
			return 'text-yellow-300';
		}
		if (action === 'D') {
			return 'text-red-700';
		}
		if (action === 'A') {
			return 'text-lime-500';
		}

		return '';
	};
</script>

<TableSearch
	color="custom"
	striped={true}
	placeholder="Search by message, author, or SHA (use spaces to separate terms)"
	divClass="relative overflow-x-auto sm:rounded-lg"
	innerDivClass="p-2 pt-0"
	inputClass="bg-gray-50 border border-gray-300 text-gray-900 text-sm rounded-lg focus:ring-blue-500 focus:border-blue-500 block w-full p-2 pl-10 bg-gray-700 dark:bg-gray-700 border-gray-300 dark:border-gray-300 placeholder-gray-400 dark:placeholder-gray-400 text-white dark:text-white focus:ring-primary-500 dark:focus:ring-primary-500 focus:border-primary-500 dark:focus:border-primary-500"
	bind:inputValue={searchTerm}
>
	<TableHead class="text-left border-b-0 p-2 bg-secondary-800 dark:bg-space-950">
		<TableHeadCell class="p-1" />
		{#if showBuildStatus}
			<TableHeadCell class="p-1" />
		{/if}
		<TableHeadCell class="pl-1">SHA</TableHeadCell>
		<TableHeadCell>Message</TableHeadCell>
		<TableHeadCell>Timestamp</TableHeadCell>
		<TableHeadCell>Author</TableHeadCell>
		<TableHeadCell />
	</TableHead>
	<TableBody>
		{#each filteredCommits as commit, index}
			<TableBodyRow
				class="text-left border-b-0 p-2 {index % 2 === 0
					? 'bg-secondary-700 dark:bg-space-900'
					: 'bg-secondary-800 dark:bg-space-950'}"
			>
				<TableBodyCell class="px-0.5">
					{#if isCommitLatestLocal(commit.sha)}
						<ChevronRightOutline class="w-3 h-3" />
					{/if}
				</TableBodyCell>
				{#if showBuildStatus}
					<TableBodyCell class="px-1 h-full items-center">
						{#if commit.status === 'success'}
							<span class="text-xs px-0.5">🟢</span>
							<Tooltip
								class="w-auto bg-secondary-600 dark:bg-space-800 font-semibold shadow-2xl"
								placement="bottom"
								>Commit has successful build
							</Tooltip>
						{:else if commit.status === 'pending'}
							<span class="text-xs px-0.5">🟡</span>
							<Tooltip
								class="w-auto bg-secondary-600 dark:bg-space-800 font-semibold shadow-2xl"
								placement="bottom"
								>Commit has build in progress
							</Tooltip>
						{:else if commit.status === 'error' || commit.status === 'failure'}
							<span class="text-xs px-0.5">🔴</span>
							<Tooltip
								class="w-auto bg-secondary-600 dark:bg-space-800 font-semibold shadow-2xl"
								placement="bottom"
								>Commit build failed
							</Tooltip>
						{:else}
							<div class="w-3 h-3" />
							<Tooltip
								class="w-auto bg-secondary-600 dark:bg-space-800 font-semibold shadow-2xl"
								placement="bottom"
								>Commit has no build
							</Tooltip>
						{/if}
					</TableBodyCell>
				{/if}
				<TableBodyCell
					class="h-full items-center pl-1 py-2 pr-4 {isCommitLatestLocal(commit.sha)
						? 'font-bold'
						: 'font-light'}"
				>
					<code>{commit.sha}</code></TableBodyCell
				>
				<TableBodyCell
					id="sha-{commit.sha}"
					class="py-2 break-normal overflow-ellipsis overflow-hidden whitespace-nowrap w-3/4 max-w-[20vw]"
					><span
						class:font-bold={isCommitLatestLocal(commit.sha)}
						class:font-light={!isCommitLatestLocal(commit.sha)}
						class:text-primary-400={commit.local}
						class:text-gray-400={!commit.local}>{commit.message}</span
					>
				</TableBodyCell>
				<TableBodyCell class="py-2 {isCommitLatestLocal(commit.sha) ? 'font-bold' : 'font-light'}"
					>{commit.timestamp}</TableBodyCell
				>
				<TableBodyCell class="py-2 {isCommitLatestLocal(commit.sha) ? 'font-bold' : 'font-light'}"
					>{commit.author}</TableBodyCell
				>
				<TableBodyCell class="py-2">
					<Button
						size="xs"
						color="primary"
						on:click={() =>
							expandedCommit === commit.sha ? setExpandedCommit('') : setExpandedCommit(commit.sha)}
					>
						{#if expandedCommit === commit.sha}
							<ChevronDownOutline size="xs" />
						{:else}
							<ChevronUpOutline size="xs" />
						{/if}
					</Button>
				</TableBodyCell>
			</TableBodyRow>
			<Tooltip
				triggeredBy="#sha-{commit.sha}"
				class="w-auto bg-secondary-700 dark:bg-space-900 font-semibold shadow-2xl"
				placement="bottom"
				>{commit.message}
			</Tooltip>
			{#if expandedCommit === commit.sha}
				<TableBodyRow
					class="text-left border-b-0 p-2 {index % 2 === 0
						? 'bg-secondary-700 dark:bg-space-900'
						: 'bg-secondary-800 dark:bg-space-950'}"
				>
					<td />
					<td colspan="4" class="border-0">
						<div class="w-full pb-4 px-6">
							<p class="text-white">Commit Files</p>
							{#if loadingCommitFiles}
								<Spinner class="w-4 h-4" />
							{:else}
								{#each commitFiles as file}
									<span class={getCommitFileTextClass(file.action)}>
										{getFileDisplayName(file)}<br />
									</span>
								{/each}
							{/if}
						</div>
					</td>
				</TableBodyRow>
			{/if}
		{:else}
			<TableBodyRow class="text-left border-b-0 p-2 bg-secondary-700">
				<TableBodyCell class="py-2">No commits yet! (We may still be loading.)</TableBodyCell>
			</TableBodyRow>
		{/each}
	</TableBody>
</TableSearch>
