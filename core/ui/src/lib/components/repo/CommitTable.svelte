<script lang="ts">
	import {
		Button,
		Spinner,
		Table,
		TableBody,
		TableBodyCell,
		TableBodyRow,
		TableHead,
		TableHeadCell,
		Tooltip
	} from 'flowbite-svelte';
	import { ChevronDownSolid, ChevronRightSolid, ChevronUpSolid } from 'flowbite-svelte-icons';
	import type { Commit, CommitFileInfo, Nullable } from '$lib/types/index.js';

	export let commits: Commit[];
	export let latestLocalCommit: Nullable<Commit> = null;

	export let showFilesHandler: (commit: string, stash: boolean) => Promise<CommitFileInfo[]>;

	// commit file details
	let expandedCommit = '';
	let loadingCommitFiles = false;
	let commitFiles: CommitFileInfo[] = [];

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

<Table color="custom" striped={true}>
	<TableHead class="text-left border-b-0 p-2 bg-secondary-800 dark:bg-space-950">
		<TableHeadCell class="pl-8">SHA</TableHeadCell>
		<TableHeadCell>Message</TableHeadCell>
		<TableHeadCell>Timestamp</TableHeadCell>
		<TableHeadCell>Author</TableHeadCell>
		<TableHeadCell />
	</TableHead>
	<TableBody>
		{#each commits as commit, index}
			<TableBodyRow
				class="text-left border-b-0 p-2 {index % 2 === 0
					? 'bg-secondary-700 dark:bg-space-900'
					: 'bg-secondary-800 dark:bg-space-950'}"
			>
				<TableBodyCell
					class="flex items-center py-2 pl-8 {isCommitLatestLocal(commit.sha)
						? 'font-bold'
						: 'font-light'}"
				>
					{#if isCommitLatestLocal(commit.sha)}
						<ChevronRightSolid class="w-3 h-3 mr-2 -ml-6" />
					{/if}
					{commit.sha}</TableBodyCell
				>
				<TableBodyCell
					id="sha-{commit.sha}"
					class="py-2 break-normal overflow-ellipsis overflow-hidden whitespace-nowrap w-3/4 max-w-[22vw]"
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
							<ChevronDownSolid size="xs" />
						{:else}
							<ChevronUpSolid size="xs" />
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
										{file.file}<br />
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
</Table>
