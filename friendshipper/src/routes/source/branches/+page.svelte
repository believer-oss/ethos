<script lang="ts">
	import {
		Alert,
		Badge,
		Button,
		Card,
		Spinner,
		TabItem,
		Table,
		TableBody,
		TableBodyCell,
		TableBodyRow,
		TableHead,
		TableHeadCell,
		Tabs,
		Tooltip
	} from 'flowbite-svelte';
	import {
		ExclamationCircleSolid,
		InfoCircleSolid,
		PlusOutline,
		RefreshOutline,
		TrashBinOutline
	} from 'flowbite-svelte-icons';
	import { emit } from '@tauri-apps/api/event';
	import { onDestroy, onMount } from 'svelte';
	import { CommitTable, ProgressModal } from '@ethos/core';
	import { appConfig, repoConfig, repoStatus } from '$lib/stores';
	import {
		branchActionsDisabled,
		branchLifecycle,
		createPrBlockers,
		EDITOR_OPEN,
		isFeatureBranch,
		isProtectedBranch,
		isQuickSubmitBranch,
		localSyncBlocker,
		performSwitch,
		prAction,
		rebaseBlockReason,
		rebaseViewFor,
		refreshAfterBranchChange,
		sha7,
		shortSha
	} from '$lib/branches';
	import {
		createBranchPullRequest,
		enqueueBranchPullRequest,
		getBranchAnalysis,
		getEditorBinariesStatus,
		getRebasePreflight,
		getRebaseState,
		getRepoStatus,
		showCommitFiles,
		startRebase,
		triggerEditorRebuild
	} from '$lib/repo';
	import { openUrl } from '$lib/utils';
	import type {
		BranchAnalysis,
		EditorBinariesStatus,
		FileTouch,
		Nullable,
		RebaseFinishResponse,
		RebasePreflight,
		RebaseState,
		TauriError
	} from '$lib/types';
	import BranchSelector from '$lib/components/BranchSelector.svelte';
	import BranchCleanupModal from '$lib/components/BranchCleanupModal.svelte';
	import BranchPullRequestModal from '$lib/components/BranchPullRequestModal.svelte';
	import CommitInfoModal from '$lib/components/CommitInfoModal.svelte';
	import CreateBranchModal from '$lib/components/CreateBranchModal.svelte';
	import RebaseConflictResolver from '$lib/components/RebaseConflictResolver.svelte';
	import RebaseRecoveryView from '$lib/components/RebaseRecoveryView.svelte';
	import RebaseWarningModal from '$lib/components/RebaseWarningModal.svelte';

	let busy = false;
	let showProgressModal = false;
	let progressModalTitle = '';
	let createOpen = false;
	let cleanupOpen = false;

	const infoAlertClass =
		'bg-secondary-700 dark:bg-space-900 border border-primary-500 text-white dark:text-white';
	const warningAlertClass =
		'bg-yellow-900 dark:bg-yellow-900 border border-yellow-700 text-yellow-100 dark:text-yellow-100';
	const errorAlertClass =
		'bg-red-900 dark:bg-red-900 border border-red-700 text-red-100 dark:text-red-100';
	const successAlertClass =
		'bg-lime-900 dark:bg-lime-900 border border-lime-700 text-lime-100 dark:text-lime-100';

	$: branch = $repoStatus?.branch ?? '';

	let rebaseState: Nullable<RebaseState> = null;
	let rebaseStateError: Nullable<string> = null;
	let rebaseStateLoading = false;
	let editorBinaries: Nullable<EditorBinariesStatus> = null;
	let editorBinariesFailed = false;
	let editorBinariesRequest = 0;
	let finishResult: Nullable<RebaseFinishResponse> = null;
	// The branch the shown finish result belongs to.
	let finishedBranch = '';
	let editorBuildTriggered = false;
	let preflight: Nullable<RebasePreflight> = null;
	let rebaseWarningOpen = false;
	let resolverOpen = false;

	$: rebaseView = rebaseViewFor(rebaseState);
	$: rebaseActive = rebaseView !== 'none';
	// Not loaded yet, or the last load failed: never treated as idle.
	$: rebaseUnknown = rebaseState === null || rebaseStateError !== null;
	// Switching and creating stay allowed in stale; dismissing it is housekeeping.
	$: editorOpen = !!$repoStatus?.editorRunning;
	$: branchingBlocked =
		editorOpen || rebaseUnknown || (rebaseActive && rebaseState?.state !== 'stale');
	let rebaseGateReason: Nullable<string> = null;
	$: if (editorOpen) {
		rebaseGateReason = EDITOR_OPEN;
	} else if (rebaseUnknown) {
		rebaseGateReason =
			rebaseStateError !== null
				? "Couldn't read the rebase state. Retry."
				: 'Loading the rebase state…';
	} else {
		rebaseGateReason = rebaseActive ? 'Finish or dismiss the current rebase first.' : null;
	}
	$: resolverOpen = rebaseView === 'resolver';
	// A finish result never outlives its branch, so its editor build can't go to another one.
	$: if (finishResult && branch !== finishedBranch) {
		finishResult = null;
	}

	const errorMessage = (e: unknown): string => {
		const err = e as TauriError;
		return err?.message ? err.message : String(e);
	};

	// A failure keeps the last known state for the views, but gates the page.
	const loadRebaseState = async () => {
		rebaseStateLoading = true;
		try {
			rebaseState = await getRebaseState();
			rebaseStateError = null;
		} catch (e) {
			rebaseStateError = errorMessage(e);
		} finally {
			rebaseStateLoading = false;
		}
	};

	// Only the newest request's result is shown.
	const loadEditorBinaries = async () => {
		editorBinariesRequest += 1;
		const request = editorBinariesRequest;
		try {
			const s = await getEditorBinariesStatus();
			if (request === editorBinariesRequest) {
				editorBinaries = s;
				editorBinariesFailed = false;
			}
		} catch (_) {
			if (request === editorBinariesRequest) {
				editorBinaries = null;
				editorBinariesFailed = true;
			}
		}
	};

	onMount(() => {
		void loadRebaseState();
		void loadEditorBinaries();
	});

	let editorChecking = false;
	const recheckEditor = async () => {
		editorChecking = true;
		try {
			repoStatus.set(await getRepoStatus());
		} catch (e) {
			await emit('error', e);
		} finally {
			editorChecking = false;
		}
	};

	const openCreate = () => {
		createOpen = true;
	};

	const handleBranchSelected = async (target: string) => {
		if (target === $repoStatus?.branch || branchingBlocked) return;
		busy = true;
		showProgressModal = true;
		progressModalTitle = `Switching to ${target}`;
		try {
			await performSwitch(target);
		} finally {
			showProgressModal = false;
			busy = false;
			void loadRebaseState();
		}
	};

	const MAX_LISTED_CONFLICTS = 20;

	let analysis: Nullable<BranchAnalysis> = null;
	let analysisError: Nullable<string> = null;
	// Branch the shown analysis or error belongs to.
	let analysisFor = '';
	let analyzing = false;
	let rerunQueued = false;
	let prBusy = false;
	let prModalOpen = false;
	let destroyed = false;

	$: gitUnsupported = analysis?.gitSupported === false;
	$: cleanupBlocked = branch === '' || !!$repoStatus?.detachedHead || gitUnsupported;
	let cleanupTooltip = '';
	$: if (rebaseGateReason !== null) {
		cleanupTooltip = rebaseGateReason;
	} else if (gitUnsupported) {
		cleanupTooltip = 'Update git to use Branch Management.';
	} else if (cleanupBlocked) {
		cleanupTooltip = 'Check out a branch to clean up branches.';
	} else {
		cleanupTooltip = 'Review and delete local f11r-branch-* branches that were deleted on origin.';
	}

	const openCleanup = () => {
		if (busy || editorOpen || cleanupBlocked || rebaseUnknown || rebaseActive) return;
		cleanupOpen = true;
	};

	let commitInfoModalOpen = false;
	let commitInfoSha: string | null = null;

	onDestroy(() => {
		destroyed = true;
	});

	// One analysis at a time; a trigger while one runs queues exactly one more run.
	const runAnalysis = async (): Promise<void> => {
		const current = $repoStatus?.branch ?? '';
		if (analysisFor !== current) {
			analysis = null;
			analysisError = null;
			analysisFor = current;
		}
		if (analyzing) {
			rerunQueued = true;
			return;
		}
		if (
			$branchActionsDisabled ||
			!$repoStatus ||
			$repoStatus.branch === '' ||
			$repoStatus.detachedHead
		) {
			analysis = null;
			analysisError = null;
			branchLifecycle.set(null);
			return;
		}
		const requested = $repoStatus.branch;
		analyzing = true;
		try {
			const a = await getBranchAnalysis();
			// A result for a branch that is no longer current is dropped; the queued rerun replaces it.
			if (a.branch === requested && requested === $repoStatus?.branch) {
				analysis = a;
				analysisError = null;
				void loadEditorBinaries();
			}
		} catch (e) {
			if (requested === $repoStatus?.branch) {
				analysis = null;
				analysisError = errorMessage(e);
			}
		} finally {
			analyzing = false;
		}
		branchLifecycle.set(
			analysis && analysis.gitSupported
				? {
						branch: analysis.branch,
						merged: analysis.pr?.state === 'MERGED',
						gone: analysis.upstream === 'gone'
				  }
				: null
		);
		if (rerunQueued && !destroyed) {
			rerunQueued = false;
			void runAnalysis();
		}
	};

	// HEAD moves on switch, create, branch submit, Sync and rebase; no timers or polling.
	$: analysisKey = `${$repoStatus?.branch ?? ''}|${
		$repoStatus?.commitHead ?? ''
	}|${$branchActionsDisabled}`;
	const onAnalysisKey = (_key: string) => {
		void runAnalysis();
	};
	$: onAnalysisKey(analysisKey);

	$: target = $appConfig.targetBranch ?? '';
	$: showManagement = branch !== '' && !$repoStatus?.detachedHead && !$branchActionsDisabled;
	$: merged = analysis?.pr?.state === 'MERGED';
	$: gone = analysis?.upstream === 'gone';
	$: action = analysis ? prAction(analysis) : 'none';
	$: blockers = analysis ? createPrBlockers(analysis) : [];
	$: prDisabled =
		blockers.length > 0 ||
		analyzing ||
		prBusy ||
		busy ||
		$branchActionsDisabled ||
		rebaseUnknown ||
		rebaseActive;
	$: rebaseReason = rebaseBlockReason(
		$repoStatus,
		analysis,
		rebaseUnknown ? null : rebaseState,
		busy || analyzing || prBusy
	);

	const syncText = (a: BranchAnalysis): string => {
		if (a.localSync === 'unknown') {
			if (a.upstream === 'notPushed') return 'Branch has not been pushed to origin';
			if (a.upstream === 'gone') return 'Deleted on origin';
			return 'Unknown';
		}
		return localSyncBlocker(a) ?? 'In sync';
	};

	const mergeableText = (mergeable: string): string => {
		switch (mergeable) {
			case 'MERGEABLE':
				return 'Mergeable';
			case 'CONFLICTING':
				return 'Conflicting';
			case 'UNKNOWN':
				return 'Mergeability unknown';
			default:
				return mergeable;
		}
	};

	const touchText = (t: Nullable<FileTouch>): string =>
		t ? `${t.sha} · ${t.author} · ${new Date(t.date).toLocaleString()}` : '—';

	const openPrUrl = () => {
		if (analysis?.pr) {
			void openUrl(analysis.pr.url);
		}
	};

	const showCommitInfo = (sha: string) => {
		commitInfoSha = sha;
		commitInfoModalOpen = true;
	};

	const editorBinariesText = (s: Nullable<EditorBinariesStatus>, failed: boolean): string => {
		if (failed) return 'Unknown';
		if (!s) return '—';
		switch (s.state) {
			case 'match':
				return `Up to date (built at ${shortSha(s.dllCommit)})`;
			case 'mismatch': {
				const c = s.latestCppCommit;
				const changed = c ? `${shortSha(c.sha)} "${c.subject}" changed C++` : 'C++ changed since';
				return `Out of date: built at ${shortSha(
					s.dllCommit
				)}, but ${changed}. Trigger an editor build ([ci:editor]) to update.`;
			}
			default:
				return "No editor binaries found for this branch's history";
		}
	};

	const handleReturnToTarget = async () => {
		if (target === '' || branchingBlocked) return;
		busy = true;
		showProgressModal = true;
		progressModalTitle = `Switching to ${target}`;
		try {
			await performSwitch(target);
		} finally {
			showProgressModal = false;
			busy = false;
			void loadRebaseState();
		}
	};

	const openRebaseWarning = async () => {
		if (busy || rebaseReason !== null) return;
		busy = true;
		try {
			preflight = await getRebasePreflight();
			rebaseWarningOpen = true;
		} catch (e) {
			await emit('error', e);
		} finally {
			busy = false;
		}
	};

	// The progress modal stays up until the status has refreshed.
	const handleStartRebase = async () => {
		if (busy) return;
		busy = true;
		showProgressModal = true;
		progressModalTitle = `Rebasing ${branch}`;
		try {
			const response = await startRebase();
			await refreshAfterBranchChange();
			rebaseState = response;
			rebaseStateError = null;
		} catch (e) {
			await emit('error', e);
			await refreshAfterBranchChange();
			await loadRebaseState();
		} finally {
			showProgressModal = false;
			busy = false;
		}
	};

	const handleRebaseProgress = (e: CustomEvent<{ title: string }>) => {
		busy = true;
		showProgressModal = true;
		progressModalTitle = e.detail.title;
	};

	const handleRebaseProgressDone = () => {
		showProgressModal = false;
		busy = false;
	};

	// The view has already refreshed the status, so `branch` is the pushed branch.
	const handleRebaseFinished = async (e: CustomEvent<RebaseFinishResponse>) => {
		finishedBranch = branch;
		finishResult = e.detail;
		editorBuildTriggered = false;
		await loadRebaseState();
		// Pushing leaves HEAD unchanged, so the analysis key does not rerun it.
		void runAnalysis();
	};

	const dismissFinishResult = () => {
		finishResult = null;
	};

	const handleTriggerEditorBuild = async () => {
		if (busy) return;
		busy = true;
		showProgressModal = true;
		progressModalTitle = 'Triggering editor build';
		try {
			const commit = await triggerEditorRebuild();
			await refreshAfterBranchChange();
			editorBuildTriggered = true;
			await emit('success', `Pushed [ci:editor] ${sha7(commit.sha)}`);
		} catch (e) {
			await emit('error', e);
			await refreshAfterBranchChange();
		} finally {
			await loadRebaseState();
			showProgressModal = false;
			busy = false;
		}
	};

	const openPrModal = () => {
		if (prDisabled || action === 'none') return;
		prModalOpen = true;
	};

	const runPrAction = async (title: string, run: () => Promise<void>) => {
		if (prBusy || action === 'none') return;
		prBusy = true;
		prModalOpen = false;
		showProgressModal = true;
		progressModalTitle = title;
		try {
			await run();
		} catch (e) {
			await emit('error', e);
		} finally {
			showProgressModal = false;
			prBusy = false;
			void runAnalysis();
			void loadRebaseState();
		}
	};

	const handleCreatePr = (title: string) =>
		runPrAction('Creating pull request', async () => {
			const r = await createBranchPullRequest({ title });
			if (!r.created) {
				await emit('success', `PR #${r.number} is already open`);
				return;
			}
			await emit('success', `Created PR #${r.number}`);
			void openUrl(r.url);
		});

	const handleEnqueuePr = () =>
		runPrAction('Adding to merge queue', async () => {
			const r = await enqueueBranchPullRequest();
			await emit(
				'success',
				r.alreadyQueued
					? `PR #${r.number} is already in the merge queue`
					: `Adding PR #${r.number} to the merge queue…`
			);
		});
</script>

{#if editorOpen}
	<Alert class="{errorAlertClass} mb-2">
		<ExclamationCircleSolid slot="icon" class="w-5 h-5 shrink-0 text-red-300" />
		<div class="flex flex-row items-center gap-2 flex-wrap">
			<span>{EDITOR_OPEN}</span>
			<Button size="xs" color="alternative" disabled={editorChecking} on:click={recheckEditor}>
				Check again
			</Button>
		</div>
	</Alert>
{/if}
<fieldset class="flex flex-col gap-2 w-full min-w-0" disabled={editorOpen}>
	<Card
		class="w-full p-4 sm:p-4 max-w-full bg-secondary-700 dark:bg-space-900 border-0 shadow-none"
	>
		<div class="flex flex-row items-center gap-2">
			<div id="branch-select">
				<BranchSelector
					current={branch}
					disabled={busy || branchingBlocked}
					on:select={(e) => handleBranchSelected(e.detail)}
				/>
			</div>
			<Button id="branch-new" size="xs" disabled={busy || branchingBlocked} on:click={openCreate}>
				<PlusOutline class="w-4 h-4 me-2" />
				New branch
			</Button>
			{#if branchingBlocked && rebaseGateReason !== null}
				<Tooltip
					triggeredBy="#branch-select, #branch-new"
					class="z-50 w-[22rem] text-xs text-primary-400 bg-secondary-600 dark:bg-space-800 shadow-2xl"
					placement="bottom">{rebaseGateReason}</Tooltip
				>
			{/if}
			<Button
				id="branch-cleanup"
				size="xs"
				color="alternative"
				disabled={busy || editorOpen || cleanupBlocked || rebaseUnknown || rebaseActive}
				on:click={openCleanup}
			>
				<TrashBinOutline class="w-4 h-4 me-2" />
				Clean up deleted branches
			</Button>
			<Tooltip
				triggeredBy="#branch-cleanup"
				class="z-50 w-[22rem] text-xs text-primary-400 bg-secondary-600 dark:bg-space-800 shadow-2xl"
				placement="bottom">{cleanupTooltip}</Tooltip
			>
		</div>
	</Card>

	{#if isProtectedBranch(branch, $repoConfig)}
		<Alert class={infoAlertClass}>
			<InfoCircleSolid slot="icon" class="w-5 h-5 shrink-0 text-primary-400" />
			You're on <code>{branch}</code>. Create or select a feature branch to manage it.
		</Alert>
	{:else if isQuickSubmitBranch(branch)}
		<Alert class={infoAlertClass}>
			<InfoCircleSolid slot="icon" class="w-5 h-5 shrink-0 text-primary-400" />
			You're on <code>{branch}</code>, a Quick Submit branch. Create or select a feature branch to
			manage it.
		</Alert>
	{:else if branch && !isFeatureBranch(branch)}
		<Alert class={warningAlertClass}>
			<ExclamationCircleSolid slot="icon" class="w-5 h-5 shrink-0 text-yellow-300" />
			<code>{branch}</code> was not created by Friendshipper. It may be shared or protected differently,
			but actions are allowed.
		</Alert>
	{/if}

	{#if rebaseStateError !== null}
		<Alert class={errorAlertClass}>
			<ExclamationCircleSolid slot="icon" class="w-5 h-5 shrink-0 text-red-300" />
			<div class="flex flex-row items-center gap-2 flex-wrap">
				<span class="break-words">Couldn't read the rebase state: {rebaseStateError}</span>
				<Button size="xs" color="red" disabled={rebaseStateLoading} on:click={loadRebaseState}>
					Retry
				</Button>
			</div>
		</Alert>
	{/if}

	<!-- Outside showManagement: HEAD is detached mid-rebase, and a stale record can name another branch. -->
	{#if rebaseState}
		<RebaseRecoveryView
			bind:rebaseState
			on:changed={loadRebaseState}
			on:finished={handleRebaseFinished}
			on:progress={handleRebaseProgress}
			on:progressdone={handleRebaseProgressDone}
		/>
	{/if}

	{#if finishResult && finishResult.outcome === 'pushed'}
		<Alert class={successAlertClass} dismissable on:close={dismissFinishResult}>
			<div class="flex flex-col gap-2">
				<p><code>{finishedBranch}</code> was rebased and pushed. Collaborators must Sync.</p>
				{#if finishResult.droppedCommits.length > 0}
					<div class="text-sm">
						<p>Not carried over:</p>
						<ul class="pl-4">
							{#each finishResult.droppedCommits as c (c.sha)}
								<li class="truncate" title={c.subject}>
									<span class="font-mono">{sha7(c.sha)}</span>
									{c.subject}
								</li>
							{/each}
						</ul>
					</div>
				{/if}
				{#if finishResult.postStepError}
					<p class="text-sm text-yellow-300 break-words">{finishResult.postStepError}</p>
				{/if}
				{#if finishResult.touchesCpp && !editorBuildTriggered}
					<div>
						<Button size="xs" disabled={busy} on:click={handleTriggerEditorBuild}>
							Trigger editor build
						</Button>
					</div>
				{/if}
			</div>
		</Alert>
	{/if}

	{#if showManagement}
		{#if analysis && analysis.gitSupported && (merged || gone)}
			<Alert class={infoAlertClass}>
				<InfoCircleSolid slot="icon" class="w-5 h-5 shrink-0 text-primary-400" />
				<div class="flex flex-row items-center gap-2 flex-wrap">
					<span>
						{#if merged && gone}
							<code>{analysis.branch}</code> was merged (PR #{analysis.pr?.number}) and deleted on
							origin.
						{:else if merged}
							<code>{analysis.branch}</code> was merged (PR #{analysis.pr?.number}).
						{:else}
							<code>{analysis.branch}</code> no longer exists on origin (it was merged or deleted).
						{/if}
					</span>
					{#if target !== ''}
						<Button
							size="xs"
							disabled={busy || analyzing || prBusy || branchingBlocked}
							on:click={handleReturnToTarget}
						>
							Return to {target}
						</Button>
						{#if !branchingBlocked}
							<span class="text-gray-400">or pick another branch from the branch menu.</span>
						{/if}
					{:else if !branchingBlocked}
						<span class="text-gray-400">Pick another branch from the branch menu.</span>
					{/if}
				</div>
			</Alert>
		{/if}

		<Card
			class="w-full p-4 sm:p-4 max-w-full bg-secondary-700 dark:bg-space-900 border-0 shadow-none"
		>
			<div class="flex flex-row items-center justify-between pb-2">
				<h3 class="text-primary-400 text-xl">
					{analysis && !analysis.gitSupported ? 'Update git to use Branch Management' : 'Status'}
				</h3>
				<Button
					id="branch-refresh"
					class="!p-1.5"
					primary
					disabled={analyzing || busy || prBusy}
					on:click={() => runAnalysis()}
				>
					{#if analyzing}
						<Spinner size="4" />
					{:else}
						<RefreshOutline class="w-4 h-4" />
					{/if}
				</Button>
				<Tooltip
					triggeredBy="#branch-refresh"
					class="z-50 w-auto text-xs text-primary-400 bg-secondary-600 dark:bg-space-800 shadow-2xl"
					placement="left">Refresh (fetches from origin and GitHub)</Tooltip
				>
			</div>
			{#if analysisError}
				<p class="text-sm text-red-400 break-words">{analysisError}</p>
			{:else if !analysis}
				<div class="flex justify-center p-2"><Spinner size="6" /></div>
			{:else if !analysis.gitSupported}
				<p class="text-sm text-gray-300">
					Detected git {analysis.gitVersion}; 2.38 or newer is required.
				</p>
			{:else}
				<dl class="grid grid-cols-[max-content_1fr] gap-x-4 gap-y-1 text-sm text-gray-300">
					<dt class="font-semibold text-white">Ahead / behind {analysis.trunkBranch}</dt>
					<dd>{analysis.commitsAhead} ahead, {analysis.commitsBehind} behind</dd>
					<dt class="font-semibold text-white">Sync with origin</dt>
					<dd>{syncText(analysis)}</dd>
					<dt class="font-semibold text-white">Rebase base</dt>
					<dd class:font-mono={analysis.mergeBase !== null}>
						{analysis.mergeBase ? analysis.mergeBase.substring(0, 8) : '—'}
					</dd>
					<dt class="font-semibold text-white">Editor binaries</dt>
					<dd class="break-words">{editorBinariesText(editorBinaries, editorBinariesFailed)}</dd>
					<dt class="font-semibold text-white">Conflicts (predicted)</dt>
					<dd>
						{#if analysis.commitsAhead === 0}
							No changes to merge
						{:else if analysis.predictedConflicts.length === 0}
							None predicted
						{:else}
							<details>
								<summary class="cursor-pointer text-red-400">
									{analysis.predictedConflicts.length} file(s) changed on both sides — will conflict
								</summary>
								<ul class="font-mono text-xs pl-4">
									{#each analysis.predictedConflicts.slice(0, MAX_LISTED_CONFLICTS) as path}
										<li class="break-all">{path}</li>
									{/each}
								</ul>
								{#if analysis.predictedConflicts.length > MAX_LISTED_CONFLICTS}
									<p class="pl-4 text-gray-400">
										…and {analysis.predictedConflicts.length - MAX_LISTED_CONFLICTS} more
									</p>
								{/if}
							</details>
						{/if}
					</dd>
					<dt class="font-semibold text-white">Pull request</dt>
					<dd>
						{#if analysis.prError !== null}
							GitHub unavailable: {analysis.prError}
						{:else if analysis.pr === null}
							No pull request
						{:else if analysis.pr.state === 'MERGED'}
							Merged (#{analysis.pr.number})
						{:else}
							<button type="button" class="text-primary-400 hover:underline" on:click={openPrUrl}
								>#{analysis.pr.number}</button
							>
							· {mergeableText(analysis.pr.mergeable)} ·
							{#if analysis.pr.mergeQueueEntry}
								In merge queue (position {analysis.pr.mergeQueueEntry.position}, {analysis.pr
									.mergeQueueEntry.state})
							{:else}
								Not queued
							{/if}
						{/if}
					</dd>
				</dl>
			{/if}
		</Card>

		{#if analysis && analysis.gitSupported && !analysisError}
			<div class="flex gap-2 items-center">
				{#if action !== 'none'}
					<Button id="branch-pr-action" size="xs" disabled={prDisabled} on:click={openPrModal}>
						{action === 'create' ? 'Create PR' : 'Add to merge queue'}
					</Button>
					<Tooltip
						triggeredBy="#branch-pr-action"
						class="z-50 w-[22rem] text-xs text-primary-400 bg-secondary-600 dark:bg-space-800 shadow-2xl"
						placement="right"
						>{rebaseGateReason ??
							blockers[0] ??
							(action === 'create'
								? `Creates a pull request into ${analysis.trunkBranch}.`
								: `Adds PR #${analysis.pr?.number} to the merge queue.`)}</Tooltip
					>
				{/if}
				{#if analysis.pr}
					<Button id="branch-open-pr" size="xs" color="alternative" on:click={openPrUrl}>
						Open PR
					</Button>
					<Tooltip
						triggeredBy="#branch-open-pr"
						class="z-50 w-[22rem] text-xs text-primary-400 bg-secondary-600 dark:bg-space-800 shadow-2xl"
						placement="right">Opens PR #{analysis.pr.number} on GitHub in your browser.</Tooltip
					>
				{/if}
				<Button
					id="branch-rebase"
					size="xs"
					disabled={rebaseReason !== null}
					on:click={openRebaseWarning}
				>
					Rebase onto latest {analysis.trunkBranch}
				</Button>
				<Tooltip
					triggeredBy="#branch-rebase"
					class="z-50 w-[22rem] text-xs text-primary-400 bg-secondary-600 dark:bg-space-800 shadow-2xl"
					placement="right"
					>{rebaseReason ??
						`Replays this branch's commits on top of the latest ${analysis.trunkBranch}.`}</Tooltip
				>
			</div>

			<Card
				class="w-full p-4 sm:p-4 max-w-full min-h-0 h-[24rem] bg-secondary-700 dark:bg-space-900 border-0 shadow-none"
			>
				<h3 class="text-primary-400 text-xl">Changes on branch</h3>
				<p class="pb-2 text-xs text-gray-400">
					Pushed changes compared with {analysis.trunkBranch}; unpushed commits appear after you
					submit.
				</p>
				<Tabs
					style="underline"
					divider={false}
					contentClass="bg-secondary-700 dark:bg-space-900 h-full overflow-y-auto"
				>
					<TabItem
						open
						title="Lockable files (assets & DLLs) ({analysis.lockableFileCount})"
						class="bg-secondary-700 dark:bg-space-900"
					>
						{#if analysis.lockableFiles.length === 0}
							<p class="p-2 text-sm text-gray-300">No lockable files changed on this branch.</p>
						{:else}
							{#if analysis.lockableFileCount > analysis.lockableFiles.length}
								<p class="p-2 text-sm text-gray-400">
									Showing the first {analysis.lockableFiles.length} of {analysis.lockableFileCount}
								</p>
							{/if}
							<Table color="custom" striped>
								<TableHead class="text-left border-b-0 p-2 bg-secondary-800 dark:bg-space-950">
									<TableHeadCell class="p-2">File</TableHeadCell>
									<TableHeadCell class="p-2">Last branch commit</TableHeadCell>
									<TableHeadCell class="p-2">Changed on {analysis.trunkBranch}</TableHeadCell>
									<TableHeadCell class="p-2" />
								</TableHead>
								<TableBody>
									{#each analysis.lockableFiles as row, index}
										<TableBodyRow
											class="text-left border-b-0 p-2 {index % 2 === 0
												? 'bg-secondary-700 dark:bg-space-900'
												: 'bg-secondary-800 dark:bg-space-950'}"
										>
											<TableBodyCell class="p-2 break-all whitespace-normal" title={row.path}
												>{row.displayName || row.path}</TableBodyCell
											>
											<TableBodyCell class="p-2">{touchText(row.branchCommit)}</TableBodyCell>
											<TableBodyCell class="p-2">
												{#if !row.changedOnMain}
													—
												{:else if row.mainCommit}
													Yes — {row.mainCommit.author} ({row.mainCommit.sha})
												{:else}
													Yes
												{/if}
											</TableBodyCell>
											<TableBodyCell class="p-2">
												{#if row.changedOnMain}
													<Badge color="red">Will conflict at rebase</Badge>
												{/if}
											</TableBodyCell>
										</TableBodyRow>
									{/each}
								</TableBody>
							</Table>
						{/if}
					</TabItem>
					<TabItem title="Commits ({analysis.commitsAhead})">
						{#if analysis.commitsAhead > analysis.commits.length}
							<p class="p-2 text-sm text-gray-400">
								Showing the newest {analysis.commits.length} of {analysis.commitsAhead}
							</p>
						{/if}
						<CommitTable
							commits={analysis.commits}
							showFilesHandler={showCommitFiles}
							onShowCommitInfo={showCommitInfo}
						/>
					</TabItem>
				</Tabs>
			</Card>
		{/if}
	{/if}
</fieldset>

<CreateBranchModal bind:open={createOpen} on:created={loadRebaseState} />

<BranchCleanupModal
	bind:open={cleanupOpen}
	{busy}
	on:cleaned={() => {
		void runAnalysis();
		void loadRebaseState();
	}}
/>

{#if analysis && action !== 'none'}
	<BranchPullRequestModal
		bind:open={prModalOpen}
		mode={action}
		branch={analysis.branch}
		trunk={analysis.trunkBranch}
		commitCount={analysis.commitsAhead}
		commits={analysis.commits}
		prNumber={analysis.pr?.number ?? null}
		busy={prBusy || analyzing}
		on:create={(e) => handleCreatePr(e.detail.title)}
		on:enqueue={handleEnqueuePr}
	/>
{/if}

<CommitInfoModal bind:open={commitInfoModalOpen} sha={commitInfoSha} />

{#if analysis && preflight}
	<RebaseWarningModal
		bind:showModal={rebaseWarningOpen}
		branch={analysis.branch}
		trunk={analysis.trunkBranch}
		predictedConflicts={analysis.predictedConflicts}
		{preflight}
		on:start={handleStartRebase}
	/>
{/if}

<!-- Stays mounted while any state is loaded, so a request that ends the rebase can finish. -->
{#if rebaseState}
	<RebaseConflictResolver
		bind:showModal={resolverOpen}
		bind:rebaseState
		on:changed={loadRebaseState}
		on:progress={handleRebaseProgress}
		on:progressdone={handleRebaseProgressDone}
	/>
{/if}

<ProgressModal showModal={showProgressModal} title={progressModalTitle} />
