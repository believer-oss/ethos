<script lang="ts">
	import {
		Button,
		Card,
		Spinner,
		Table,
		TableBody,
		TableBodyCell,
		TableBodyRow,
		TableHead,
		TableHeadCell,
		Tooltip
	} from 'flowbite-svelte';
	import { ClipboardOutline, InfoCircleOutline } from 'flowbite-svelte-icons';
	import { emit } from '@tauri-apps/api/event';
	import type { FileHistoryRevision } from '$lib/types';
	import CommitInfoModal from '$lib/components/CommitInfoModal.svelte';

	export let filePath: string | null = null;
	export let displayName: string = '';
	export let revisions: FileHistoryRevision[] = [];
	export let loading: boolean = false;

	let modalOpen = false;
	let modalSha: string | null = null;

	const openInfo = (sha: string) => {
		modalSha = sha;
		modalOpen = true;
	};

	const getActionClass = (action: string): string => {
		switch (action) {
			case 'modified':
				return 'text-yellow-300';
			case 'add':
				return 'text-lime-500';
			case 'delete':
				return 'text-red-700';
			case 'rename':
			case 'copy':
				return 'text-sky-400';
			default:
				return 'text-gray-300';
		}
	};

	const formatDate = (iso: string): string => {
		const d = new Date(iso);
		if (Number.isNaN(d.getTime())) return iso;
		return d.toLocaleString();
	};

	const copySha = async (sha: string) => {
		try {
			await navigator.clipboard.writeText(sha);
			await emit('success', 'SHA copied');
		} catch {
			// ignore
		}
	};
</script>

<Card
	class="w-full relative p-4 sm:p-4 max-w-full bg-secondary-700 dark:bg-space-900 h-full overflow-y-hidden border-0 shadow-none"
>
	<div class="flex items-center gap-2 pb-2">
		<h3 class="text-primary-400 text-xl">History</h3>
		<span class="text-xs text-gray-400 font-italic">({revisions.length})</span>
		{#if loading}
			<Spinner size="4" />
		{/if}
	</div>
	{#if filePath}
		<div class="pb-2">
			<div class="text-xs text-gray-400 truncate" title={filePath}>
				{filePath}
			</div>
			{#if displayName && displayName !== filePath}
				<div class="text-sm text-primary-400 font-medium truncate" title={displayName}>
					{displayName}
				</div>
			{/if}
		</div>
	{/if}
	<div class="overflow-y-auto pr-1 h-full">
		{#if !filePath}
			<p class="text-gray-300 p-2">Select a file to see its history.</p>
		{:else if !loading && revisions.length === 0}
			<p class="text-gray-300 p-2">No history for this file yet.</p>
		{:else}
			<Table color="custom" striped={true}>
				<TableHead class="w-full border-b-0 p-2 bg-secondary-800 dark:bg-space-950">
					<TableHeadCell class="p-1">#</TableHeadCell>
					<TableHeadCell class="p-1 w-8" />
					<TableHeadCell class="p-1">SHA</TableHeadCell>
					<TableHeadCell class="p-1">Date</TableHeadCell>
					<TableHeadCell class="p-1">Author</TableHeadCell>
					<TableHeadCell class="p-1">Action</TableHeadCell>
					<TableHeadCell class="p-1">Message</TableHeadCell>
				</TableHead>
				<TableBody>
					{#each revisions as rev, i (rev.commitId)}
						<TableBodyRow
							class="text-left border-b-0 {i % 2 === 0
								? 'bg-secondary-800 dark:bg-space-950'
								: 'bg-secondary-700 dark:bg-space-900'}"
						>
							<TableBodyCell class="p-1 whitespace-nowrap font-medium text-gray-300"
								>{rev.revisionNumber}</TableBodyCell
							>
							<TableBodyCell tdClass="p-1 w-8">
								<Button
									outline
									size="xs"
									class="p-1 border-0 focus-within:ring-0 dark:focus-within:ring-0"
									on:click={() => {
										openInfo(rev.commitId);
									}}
								>
									<InfoCircleOutline class="w-4 h-4" />
								</Button>
								<Tooltip
									class="w-auto bg-secondary-600 dark:bg-space-800 font-semibold shadow-2xl"
									placement="right">Show commit details</Tooltip
								>
							</TableBodyCell>
							<TableBodyCell class="p-1 whitespace-nowrap font-mono text-xs">
								<button
									type="button"
									class="flex items-center gap-1 text-primary-400 hover:underline"
									on:click={() => copySha(rev.commitId)}
								>
									{rev.shortCommitId}
									<ClipboardOutline class="w-3 h-3" />
								</button>
								<Tooltip
									class="w-auto bg-secondary-600 dark:bg-space-800 font-semibold shadow-2xl"
									placement="top">Click to copy full SHA</Tooltip
								>
							</TableBodyCell>
							<TableBodyCell class="p-1 whitespace-nowrap text-xs text-gray-300"
								>{formatDate(rev.date)}</TableBodyCell
							>
							<TableBodyCell class="p-1 whitespace-nowrap font-medium text-gray-200"
								>{rev.userName}</TableBodyCell
							>
							<TableBodyCell class="p-1 whitespace-nowrap font-medium {getActionClass(rev.action)}"
								>{rev.action}</TableBodyCell
							>
							<TableBodyCell class="p-1 font-medium text-gray-200 truncate max-w-md"
								>{rev.description}</TableBodyCell
							>
						</TableBodyRow>
					{/each}
				</TableBody>
			</Table>
		{/if}
	</div>
</Card>

<CommitInfoModal bind:open={modalOpen} sha={modalSha} />
