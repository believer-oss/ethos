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
	import { onDestroy } from 'svelte';
	import { CommitTable, ProgressModal } from '@ethos/core';
	import { appConfig, repoConfig, repoStatus } from '$lib/stores';
	import {
		branchActionsDisabled,
		branchLifecycle,
		createPrBlockers,
		isFeatureBranch,
		isProtectedBranch,
		isQuickSubmitBranch,
		localSyncBlocker,
		performSwitch,
		prAction
	} from '$lib/branches';
	import {
		createBranchPullRequest,
		enqueueBranchPullRequest,
		getBranchAnalysis,
		showCommitFiles
	} from '$lib/repo';
	import { openUrl } from '$lib/utils';
	import type { BranchAnalysis, FileTouch, Nullable, TauriError } from '$lib/types';
	import BranchSelector from '$lib/components/BranchSelector.svelte';
	import BranchCleanupModal from '$lib/components/BranchCleanupModal.svelte';
	import BranchPullRequestModal from '$lib/components/BranchPullRequestModal.svelte';
	import CommitInfoModal from '$lib/components/CommitInfoModal.svelte';
	import CreateBranchModal from '$lib/components/CreateBranchModal.svelte';

	let busy = false;
	let showProgressModal = false;
	let progressModalTitle = '';
	let createOpen = false;
	let cleanupOpen = false;

	const infoAlertClass =
		'bg-secondary-700 dark:bg-space-900 border border-primary-500 text-white dark:text-white';
	const warningAlertClass =
		'bg-yellow-900 dark:bg-yellow-900 border border-yellow-700 text-yellow-100 dark:text-yellow-100';

	$: branch = $repoStatus?.branch ?? '';

	const openCreate = () => {
		createOpen = true;
	};

	const handleBranchSelected = async (target: string) => {
		if (target === $repoStatus?.branch) return;
		busy = true;
		showProgressModal = true;
		progressModalTitle = `Switching to ${target}`;
		try {
			await performSwitch(target);
		} finally {
			showProgressModal = false;
			busy = false;
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
	$: if (gitUnsupported) {
		cleanupTooltip = 'Update git to use Branch Management.';
	} else if (cleanupBlocked) {
		cleanupTooltip = 'Check out a branch to clean up branches.';
	} else {
		cleanupTooltip = 'Review and delete local f11r-branch-* branches that were deleted on origin.';
	}

	const openCleanup = () => {
		if (busy || cleanupBlocked) return;
		cleanupOpen = true;
	};

	let commitInfoModalOpen = false;
	let commitInfoSha: string | null = null;

	onDestroy(() => {
		destroyed = true;
	});

	const errorMessage = (e: unknown): string => {
		const err = e as TauriError;
		return err?.message ? err.message : String(e);
	};

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
	$: prDisabled = blockers.length > 0 || analyzing || prBusy || busy || $branchActionsDisabled;

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

	const handleReturnToTarget = async () => {
		if (target === '') return;
		busy = true;
		showProgressModal = true;
		progressModalTitle = `Switching to ${target}`;
		try {
			await performSwitch(target);
		} finally {
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

<div class="flex flex-col gap-2 w-full">
	<Card
		class="w-full p-4 sm:p-4 max-w-full bg-secondary-700 dark:bg-space-900 border-0 shadow-none"
	>
		<div class="flex flex-row items-center gap-2">
			<BranchSelector
				current={branch}
				disabled={busy}
				on:select={(e) => handleBranchSelected(e.detail)}
			/>
			<Button size="xs" disabled={busy} on:click={openCreate}>
				<PlusOutline class="w-4 h-4 me-2" />
				New branch
			</Button>
			<Button
				id="branch-cleanup"
				size="xs"
				color="alternative"
				disabled={busy || cleanupBlocked}
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

	{#if showManagement}
		{#if analysis && analysis.gitSupported && (merged || gone)}
			<Alert class={infoAlertClass}>
				<InfoCircleSolid slot="icon" class="w-5 h-5 shrink-0 text-primary-400" />
				<div class="flex flex-row items-center gap-2 flex-wrap">
					<span>
						{#if merged}
							<code>{analysis.branch}</code> was merged (PR #{analysis.pr?.number}).
						{:else}
							<code>{analysis.branch}</code> no longer exists on origin (it was merged or deleted).
						{/if}
					</span>
					{#if target !== ''}
						<Button
							size="xs"
							disabled={busy || analyzing || prBusy}
							on:click={handleReturnToTarget}
						>
							Return to {target}
						</Button>
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
						>{blockers[0] ??
							(action === 'create'
								? `Creates a pull request into ${analysis.trunkBranch}.`
								: `Adds PR #${analysis.pr?.number} to the merge queue.`)}</Tooltip
					>
				{/if}
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
</div>

<CreateBranchModal bind:open={createOpen} />

<BranchCleanupModal bind:open={cleanupOpen} {busy} on:cleaned={() => runAnalysis()} />

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

<ProgressModal showModal={showProgressModal} title={progressModalTitle} />
