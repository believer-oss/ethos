<script lang="ts">
	import {
		Alert,
		Button,
		Card,
		Modal,
		Spinner,
		Table,
		TableBody,
		TableBodyCell,
		TableBodyRow,
		TableHead,
		TableHeadCell,
		Tooltip
	} from 'flowbite-svelte';
	import {
		BackwardStepOutline,
		ClipboardOutline,
		InfoCircleOutline,
		InfoCircleSolid
	} from 'flowbite-svelte-icons';
	import { emit } from '@tauri-apps/api/event';
	import { restoreFileToRevision } from '$lib/repo';
	import type { FileHistoryRevision } from '$lib/types';
	import CommitInfoModal from '$lib/components/CommitInfoModal.svelte';

	export let filePath: string | null = null;
	export let displayName: string = '';
	export let revisions: FileHistoryRevision[] = [];
	export let loading: boolean = false;
	// Fired after a successful revert so the host can refresh status/listings.
	export let onReverted: (() => void | Promise<void>) | null = null;

	let modalOpen = false;
	let modalSha: string | null = null;

	let revertTarget: FileHistoryRevision | null = null;
	let revertInProgress = false;

	const openInfo = (sha: string) => {
		modalSha = sha;
		modalOpen = true;
	};

	const requestRevert = (rev: FileHistoryRevision) => {
		revertTarget = rev;
	};

	const cancelRevert = () => {
		revertTarget = null;
	};

	const confirmRevert = async () => {
		if (!filePath || !revertTarget) return;
		revertInProgress = true;
		try {
			await restoreFileToRevision({
				path: filePath,
				sha: revertTarget.commitId
			});
			await emit('success', `Reverted ${filePath} to ${revertTarget.shortCommitId}.`);
			revertTarget = null;
			// Post-revert refresh is best-effort. The revert itself already succeeded, so a
			// transient failure in the host's refresh callback should not surface as a second
			// error toast on top of the success toast.
			if (onReverted) {
				try {
					await onReverted();
				} catch (refreshErr) {
					// Intentional: don't clobber the success toast with a refresh error, but log
					// so failures are debuggable if someone reports missing state after revert.
					console.warn('post-revert refresh failed', refreshErr);
				}
			}
		} catch (e) {
			await emit('error', e);
		} finally {
			revertInProgress = false;
		}
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
							<TableBodyCell tdClass="p-1 w-8">
								<Button
									outline
									size="xs"
									class="p-1 border-0 focus-within:ring-0 dark:focus-within:ring-0"
									on:click={() => {
										requestRevert(rev);
									}}
								>
									<BackwardStepOutline class="w-4 h-4 text-primary-400" />
								</Button>
								<Tooltip
									class="w-auto bg-secondary-600 dark:bg-space-800 font-semibold shadow-2xl"
									placement="right">Revert file to this version</Tooltip
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

<Modal
	open={revertTarget !== null}
	size="md"
	color="none"
	class="bg-secondary-700 dark:bg-space-900"
	bodyClass="!border-t-0 flex-1 overflow-y-auto overscroll-contain"
	backdropClass="fixed inset-0 z-40 bg-gray-900 bg-opacity-50 dark:bg-opacity-80"
	dialogClass="fixed top-0 start-0 end-0 h-modal md:inset-0 md:h-full z-50 w-full p-4 pb-12 flex"
	dismissable={false}
	on:close={cancelRevert}
>
	<svelte:fragment slot="header">
		<h3 class="text-primary-400 text-xl">Revert file to revision</h3>
	</svelte:fragment>

	{#if revertTarget}
		<div class="flex flex-col gap-3 text-sm text-gray-200">
			<p>Revert the following file to this earlier revision?</p>
			<div class="bg-secondary-800 dark:bg-space-950 p-3 rounded-md text-xs">
				<div class="text-gray-400">File</div>
				<div class="font-mono break-all text-gray-200 pb-2">{filePath}</div>
				{#if displayName && displayName !== filePath}
					<div class="text-gray-400">Asset</div>
					<div class="text-primary-400 font-medium pb-2">{displayName}</div>
				{/if}
				<div class="text-gray-400">Revision</div>
				<div class="font-mono text-gray-200">
					#{revertTarget.revisionNumber} · {revertTarget.shortCommitId} · {revertTarget.userName}
				</div>
				<div class="text-gray-300 pt-1 italic truncate">{revertTarget.description}</div>
			</div>
			<Alert class="bg-secondary-800 dark:bg-space-950 py-2 text-white dark:text-white">
				<InfoCircleSolid slot="icon" class="w-4 h-4" />
				A snapshot of the current version is saved before reverting. The operation will fail if Unreal
				Editor is running — close the editor first.
			</Alert>
		</div>
	{/if}

	<svelte:fragment slot="footer">
		<div class="flex justify-end gap-2 w-full">
			<Button size="sm" color="alternative" disabled={revertInProgress} on:click={cancelRevert}>
				Cancel
			</Button>
			<Button size="sm" color="primary" disabled={revertInProgress} on:click={confirmRevert}>
				{#if revertInProgress}
					<Spinner size="4" class="mr-2" />
					Reverting…
				{:else}
					Revert
				{/if}
			</Button>
		</div>
	</svelte:fragment>
</Modal>
