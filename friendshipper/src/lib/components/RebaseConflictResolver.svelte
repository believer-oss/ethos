<script lang="ts">
	import { Button, Modal } from 'flowbite-svelte';
	import { emit } from '@tauri-apps/api/event';
	import { createEventDispatcher } from 'svelte';
	import {
		abortGuidedRebase,
		continueRebase,
		getRebaseState,
		openRebaseConflictFile,
		resolveRebaseConflict,
		unresolveRebaseConflict
	} from '$lib/repo';
	import {
		branchSideText,
		canContinue,
		EDITOR_OPEN,
		mainSideText,
		refreshAfterBranchChange,
		resolutionLabel,
		shortSha,
		undoNeedsConfirm
	} from '$lib/branches';
	import { repoStatus } from '$lib/stores';
	import type { ConflictFile, RebaseKeep, RebaseState } from '$lib/types';
	import ConfirmModal from '$lib/components/ConfirmModal.svelte';
	import RebaseContinueConfirm from '$lib/components/RebaseContinueConfirm.svelte';

	export let showModal: boolean;
	export let rebaseState: RebaseState;

	// Mirror the backend's STEP_INCOMPLETE and UNRESOLVABLE_STEP texts.
	const STEP_INCOMPLETE =
		'Git stopped on this commit without a conflict Friendshipper can resolve. Abort to restore your branch.';
	const UNRESOLVABLE_STEP =
		"This commit has a conflict Friendshipper can't resolve. Abort the rebase.";
	const MAX_LISTED_EDITS = 10;

	const dispatch = createEventDispatcher<{
		progress: { title: string };
		progressdone: null;
		changed: null;
	}>();

	// Disables every action while a request runs.
	let inFlight = false;
	let continueOpen = false;
	let abortOpen = false;
	let undoOpen = false;
	let undoFile: ConflictFile | null = null;
	let keepOpen = false;
	let keepPending: { path: string; choice: RebaseKeep } | null = null;
	// Paths opened in the text editor during this step; Keep would overwrite their edits.
	let openedPaths = new Set<string>();
	let openedFor = '';

	$: commit = rebaseState.currentCommit;
	$: hasUnsupported = rebaseState.conflicts.some((c) => c.kind === 'unsupported');
	$: hasSupported = rebaseState.conflicts.some((c) => c.kind !== 'unsupported');
	$: editorOpen = !!$repoStatus?.editorRunning;
	$: rowsLocked = inFlight || hasUnsupported || editorOpen;
	$: backupLabel = rebaseState.backupSha ? shortSha(rebaseState.backupSha) : 'its backup';
	$: abortTitle = `Abort the rebase? ${rebaseState.branch} returns to ${backupLabel} and all resolutions are discarded.`;
	$: editedFiles = rebaseState.unstagedEdits.slice(0, MAX_LISTED_EDITS);
	$: stepKey = rebaseState.currentCommit?.sha ?? '';
	$: if (stepKey !== openedFor) {
		openedFor = stepKey;
		openedPaths = new Set();
	}
	$: keepTitle = keepPending
		? `Your edits to ${keepPending.path} will be replaced with ${
				keepPending.choice === 'main' ? "main's" : "branch's"
		  } version.`
		: '';

	// The rebase is over (or was never active): hand control back to the page.
	$: if (showModal && rebaseState.state !== 'inProgress') {
		showModal = false;
		dispatch('changed', null);
	}

	// The backend may have moved even when a request failed.
	const refetchState = async () => {
		try {
			rebaseState = await getRebaseState();
		} catch (e) {
			await emit('error', e);
		}
	};

	// Runs a conflict-level request; its response replaces the state.
	const act = async (call: () => Promise<RebaseState>) => {
		if (inFlight) return;
		inFlight = true;
		try {
			rebaseState = await call();
		} catch (e) {
			await emit('error', e);
			await refetchState();
		} finally {
			inFlight = false;
		}
	};

	// Runs a request that moves HEAD. The progress modal stays up until the status has refreshed.
	const move = async (title: string, call: () => Promise<RebaseState>) => {
		if (inFlight) return;
		inFlight = true;
		dispatch('progress', { title });
		try {
			rebaseState = await call();
		} catch (e) {
			await emit('error', e);
		}
		await refreshAfterBranchChange();
		await refetchState();
		dispatch('progressdone', null);
		inFlight = false;
	};

	const resolve = (path: string, choice: RebaseKeep) => {
		void act(() => resolveRebaseConflict({ path, keep: choice }));
	};

	const keep = (path: string, choice: RebaseKeep) => () => {
		if (inFlight) return;
		if (choice !== 'manual' && openedPaths.has(path)) {
			keepPending = { path, choice };
			keepOpen = true;
		} else {
			resolve(path, choice);
		}
	};

	const onKeepAnswer = (yes: boolean) => {
		if (yes && keepPending) resolve(keepPending.path, keepPending.choice);
		keepPending = null;
	};

	const openFile = (path: string) => () => {
		void act(async () => {
			await openRebaseConflictFile({ path });
			openedPaths = new Set(openedPaths).add(path);
			return rebaseState;
		});
	};

	const undo = (path: string) => {
		void act(() => unresolveRebaseConflict({ path }));
	};

	// Modify/delete conflicts have no markers to restore.
	const undoConfirmText = (f: ConflictFile): string => {
		const restored =
			f.kind === 'content' ? 'the conflict markers are restored' : 'the conflict is restored';
		return undoNeedsConfirm(f)
			? `Undo your manual merge of ${f.path}? Your edits to this file are discarded and ${restored}.`
			: `Undo your choice for ${f.path}? Your edits to this file are discarded and ${restored}.`;
	};

	$: undoTitle = undoFile ? undoConfirmText(undoFile) : '';

	// Refetches first, so edits made after the resolution are seen.
	const askUndo = (f: ConflictFile) => async () => {
		if (inFlight) return;
		inFlight = true;
		await refetchState();
		inFlight = false;
		const current = rebaseState.conflicts.find((c) => c.path === f.path);
		if (rebaseState.state !== 'inProgress' || !current?.resolved) return;
		if (undoNeedsConfirm(current) || rebaseState.unstagedEdits.includes(current.path)) {
			undoFile = current;
			undoOpen = true;
		} else {
			undo(current.path);
		}
	};

	const onUndoAnswer = (yes: boolean) => {
		if (yes && undoFile) undo(undoFile.path);
	};

	const askContinue = () => {
		if (inFlight || !canContinue(rebaseState)) return;
		continueOpen = true;
	};

	const onContinueConfirmed = () => {
		continueOpen = false;
		void move('Replaying commits', continueRebase);
	};

	// Refetches first, so the list of edits that abort discards is current.
	const askAbort = async () => {
		if (inFlight) return;
		inFlight = true;
		await refetchState();
		inFlight = false;
		if (rebaseState.state === 'inProgress') abortOpen = true;
	};

	const onAbortConfirmed = () => {
		abortOpen = false;
		void move('Aborting rebase', abortGuidedRebase);
	};
</script>

<Modal
	defaultClass="bg-secondary-700 dark:bg-space-900 overflow-y-auto"
	bodyClass="!border-t-0"
	backdropClass="fixed mt-8 inset-0 z-40 bg-gray-900 bg-opacity-50 dark:bg-opacity-80"
	dialogClass="fixed mt-8 top-0 start-0 end-0 h-modal md:inset-0 md:h-full z-50 w-full p-4 pb-12 flex"
	dismissable={false}
	size="xl"
	bind:open={showModal}
>
	<p class="text-xl text-primary-400">
		Replaying commit {rebaseState.step} of {rebaseState.totalSteps}{#if commit}
			— {commit.subject} by {commit.author}{/if}
	</p>

	{#if editorOpen}
		<p class="text-sm text-red-300">{EDITOR_OPEN}</p>
	{/if}
	{#if rebaseState.stepIncomplete}
		<p class="text-sm text-yellow-300">{STEP_INCOMPLETE}</p>
	{:else}
		{#if hasUnsupported && hasSupported}
			<p class="text-sm text-yellow-300">{UNRESOLVABLE_STEP}</p>
		{/if}
		{#if editedFiles.length > 0}
			<div class="bg-yellow-900/20 border border-yellow-700 p-2 rounded text-sm text-yellow-300">
				<p>
					Edited but not staged: <span class="font-mono text-xs break-all"
						>{editedFiles.join(', ')}{rebaseState.unstagedEdits.length > MAX_LISTED_EDITS
							? '…'
							: ''}</span
					>. Continue refuses these until they are fixed, and aborting discards the edits.
				</p>
			</div>
		{/if}
		<div class="flex flex-col gap-3 max-h-[60vh] overflow-y-auto" role="list">
			{#each rebaseState.conflicts as f (f.path)}
				<div
					class="flex flex-col gap-2 bg-secondary-800 dark:bg-space-950 p-3 rounded"
					role="listitem"
				>
					<div class="flex flex-col">
						<span class="text-white break-all">{f.displayName}</span>
						<code class="text-xs text-gray-400 break-all">{f.path}</code>
					</div>
					{#if f.kind === 'unsupported'}
						<p class="text-sm text-yellow-300">{f.unsupportedReason ?? ''}</p>
						<p class="text-sm text-gray-300">Abort the rebase to get back to where you started.</p>
					{:else if f.resolved}
						<div class="flex flex-row items-center justify-between gap-2">
							<span class="text-sm text-lime-400">{resolutionLabel(f)}</span>
							<Button size="xs" color="alternative" disabled={rowsLocked} on:click={askUndo(f)}>
								Undo
							</Button>
						</div>
					{:else}
						<div class="grid grid-cols-1 md:grid-cols-2 gap-2">
							<div class="flex flex-col gap-2 bg-secondary-700 dark:bg-space-900 p-2 rounded">
								<p class="text-xs font-semibold text-primary-400">Main's version</p>
								<p class="text-sm text-gray-300 break-words">{mainSideText(f)}</p>
								<Button
									size="xs"
									class="self-start"
									disabled={rowsLocked}
									on:click={keep(f.path, 'main')}
								>
									Keep this version
								</Button>
							</div>
							<div class="flex flex-col gap-2 bg-secondary-700 dark:bg-space-900 p-2 rounded">
								<p class="text-xs font-semibold text-primary-400">Branch's version</p>
								<p class="text-sm text-gray-300 break-words">{branchSideText(f)}</p>
								<Button
									size="xs"
									class="self-start"
									disabled={rowsLocked}
									on:click={keep(f.path, 'branch')}
								>
									Keep this version
								</Button>
							</div>
						</div>
						{#if f.kind === 'content' && f.isText}
							<div class="flex flex-row gap-2 flex-wrap">
								<Button
									size="xs"
									color="alternative"
									disabled={rowsLocked}
									on:click={openFile(f.path)}
								>
									Open in text editor
								</Button>
								<Button
									size="xs"
									color="alternative"
									disabled={rowsLocked}
									on:click={keep(f.path, 'manual')}
								>
									Mark resolved
								</Button>
							</div>
						{/if}
					{/if}
				</div>
			{/each}
		</div>
	{/if}

	<div class="flex flex-row-reverse gap-2">
		{#if !rebaseState.stepIncomplete}
			<Button
				size="sm"
				disabled={inFlight || editorOpen || !canContinue(rebaseState)}
				on:click={askContinue}
			>
				Continue
			</Button>
		{/if}
		<Button size="sm" color="red" disabled={inFlight || editorOpen} on:click={askAbort}
			>Abort</Button
		>
	</div>
</Modal>

<RebaseContinueConfirm
	bind:showModal={continueOpen}
	step={rebaseState.step}
	totalSteps={rebaseState.totalSteps}
	currentCommit={rebaseState.currentCommit}
	conflicts={rebaseState.conflicts}
	on:confirm={onContinueConfirmed}
/>

<ConfirmModal bind:showModal={undoOpen} title={undoTitle} resultCallback={onUndoAnswer} stacked />

<ConfirmModal bind:showModal={keepOpen} title={keepTitle} resultCallback={onKeepAnswer} stacked />

<Modal
	defaultClass="bg-secondary-700 dark:bg-space-900 overflow-y-auto"
	bodyClass="!border-t-0"
	backdropClass="fixed mt-8 inset-0 z-[60] bg-gray-900 bg-opacity-50 dark:bg-opacity-80"
	dialogClass="fixed mt-8 top-0 start-0 end-0 h-modal md:inset-0 md:h-full z-[70] w-full p-4 pb-12 flex"
	dismissable={false}
	size="sm"
	bind:open={abortOpen}
>
	<p class="text-xl text-primary-400">{abortTitle}</p>
	{#if rebaseState.unstagedEdits.length > 0}
		<div class="flex flex-col gap-1 text-sm text-yellow-300">
			<p>These edits that are not staged will be discarded:</p>
			<ul class="font-mono text-xs pl-4 max-h-40 overflow-y-auto">
				{#each rebaseState.unstagedEdits as path}
					<li class="break-all">{path}</li>
				{/each}
			</ul>
		</div>
	{/if}
	<div class="flex flex-row-reverse gap-2">
		<Button class="bg-red-700 dark:bg-red-700" on:click={onAbortConfirmed}>Yes</Button>
		<Button
			class="bg-lime-700 dark:bg-lime-700"
			on:click={() => {
				abortOpen = false;
			}}>No</Button
		>
	</div>
</Modal>
