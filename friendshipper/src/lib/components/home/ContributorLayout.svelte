<script lang="ts">
	import {
		Badge,
		Button,
		ButtonGroup,
		Card,
		Spinner,
		Table,
		TableBody,
		TableBodyCell,
		TableBodyRow,
		TableHead,
		TableHeadCell
	} from 'flowbite-svelte';
	import { LinkOutline, RefreshOutline } from 'flowbite-svelte-icons';
	import { type Nullable, ProgressModal } from '@ethos/core';
	import { onMount } from 'svelte';
	import { emit, listen } from '@tauri-apps/api/event';
	import { get } from 'svelte/store';
	import {
		generateSln,
		getMergeQueue,
		openProject,
		syncLatest,
		getRepoStatus,
		getBranchComparison
	} from '$lib/repo';
	import type {
		ArtifactEntry,
		GameServerResult,
		MergeQueue,
		MergeQueueEntry,
		Commit,
		SyncClientRequest,
		Playtest
	} from '$lib/types';
	import { LaunchMode } from '$lib/types';
	import PlaytestCard from '$lib/components/playtests/PlaytestCard.svelte';
	import ServerlessPlaytestsCard from '$lib/components/playtests/ServerlessPlaytestsCard.svelte';
	import {
		allModifiedFiles,
		appConfig,
		backgroundSyncInProgress,
		builds,
		currentSyncedVersion,
		nextPlaytest,
		playtests,
		repoConfig,
		repoStatus
	} from '$lib/stores';
	import { getPlaytestGroupForUser, getPlaytests } from '$lib/playtests';
	import { cancelDownload, getBuild, getBuilds, syncClient } from '$lib/builds';
	import { getServers } from '$lib/gameServers';
	import UnrealEngineLogoNoCircle from '$lib/icons/UnrealEngineLogoNoCircle.svelte';
	import { handleError } from '$lib/utils';

	const getDisplayedPlaytests = (pts: Playtest[]): Playtest[] => {
		if (pts.length === 0) {
			return [];
		}

		// Sort by start time (most recent first)
		const sorted = [...pts].sort((a, b) => {
			const timeA = new Date(a.spec.startTime).getTime();
			const timeB = new Date(b.spec.startTime).getTime();
			return timeB - timeA;
		});

		const mostRecent = sorted[0];

		// Check if most recent playtest has game servers enabled
		const hasGameServers = !mostRecent.spec.disableGameServers;

		if (hasGameServers) {
			// If most recent has game servers, show only that one
			return [mostRecent];
		}
		// If most recent is serverless, show up to 3 serverless playtests
		const serverless = sorted.filter((p) => p.spec.disableGameServers === true);
		return serverless.slice(0, 3);
	};

	let loadingMergeQueue = false;
	let loadingPlaytests = false;
	let loadingRepoStatus = false;
	let loadingBranchComparison = false;
	let openingProject = false;
	let mergeQueue: Nullable<MergeQueue> = null;
	let branchComparison: Nullable<Commit[]> = null;
	let syncing = false;
	let progressModalText = '';
	let progressModalCancellable = false;

	$: targetBranchConfig = $repoConfig?.targetBranches.find(
		(branchConfig) => branchConfig.name === $appConfig?.targetBranch
	);

	$: shouldShowBranchComparison = $appConfig?.primaryBranch && $appConfig?.contentBranch;

	$: displayedPlaytests = getDisplayedPlaytests($playtests);

	const handleSyncCancelled = async () => {
		try {
			await cancelDownload();
		} catch (e) {
			await emit('error', e);
		}

		syncing = false;
		progressModalCancellable = false;
	};

	const refreshPlaytests = async () => {
		try {
			loadingPlaytests = true;
			$playtests = await getPlaytests();
		} catch (e) {
			await handleError(e);
		} finally {
			loadingPlaytests = false;
		}
	};

	const refreshRepo = async () => {
		try {
			loadingRepoStatus = true;
			$repoStatus = await getRepoStatus();
		} catch (e) {
			await emit('error', e);
		}
		loadingRepoStatus = false;
	};

	const refreshMergeQueue = async () => {
		try {
			loadingMergeQueue = true;
			if (targetBranchConfig?.usesMergeQueue) {
				mergeQueue = await getMergeQueue();
			}
		} catch (e) {
			await emit('error', e);
		}
		loadingMergeQueue = false;
	};

	const refreshBranchComparison = async () => {
		try {
			loadingBranchComparison = true;
			branchComparison = await getBranchComparison(50);
		} catch (e) {
			await emit('error', e);
		}
		loadingBranchComparison = false;
	};

	const refresh = async () => {
		try {
			// We don't need to refresh the repo because the root layout component will do that
			const promises = [refreshPlaytests(), refreshMergeQueue()];
			if (shouldShowBranchComparison) {
				promises.push(refreshBranchComparison());
			}
			await Promise.all(promises);
		} catch (e) {
			await emit('error', e);
		}
	};

	const shouldDisableLaunchButton = (): boolean => {
		const pt = get(nextPlaytest);
		if (pt) {
			const playtestAssignment = getPlaytestGroupForUser($nextPlaytest, $appConfig.userDisplayName);
			if (playtestAssignment && playtestAssignment.serverRef) {
				return false;
			}
		}

		return true;
	};

	const getMainButtonText = (): string => {
		const pt = get(nextPlaytest);
		if (pt) {
			const playtestAssignment = getPlaytestGroupForUser($nextPlaytest, $appConfig.userDisplayName);
			if (playtestAssignment && playtestAssignment.serverRef) {
				return `Sync & Join Playtest (${playtestAssignment.serverRef.name})`;
			}
		}

		return 'No playtest to join!';
	};

	const getMergeQueueEntryBadgeClass = (node: MergeQueueEntry): string => {
		if (node.state === 'AWAITING_CHECKS' || node.state === 'QUEUED') {
			return 'bg-yellow-500 dark:bg-yellow-500 animate-pulse';
		}

		if (node.state === 'UNMERGEABLE') {
			return 'bg-red-700 dark:bg-red-700';
		}

		return 'bg-secondary-500 dark:bg-secondary-500';
	};

	const handleSyncClicked = async () => {
		try {
			syncing = true;
			progressModalText = 'Pulling latest with git';
			await syncLatest();

			if (!$appConfig.pullDlls) {
				progressModalText = 'Generating projects';
				await generateSln();
			} else if ($appConfig.openUprojectAfterSync) {
				progressModalText = 'Launching Unreal Engine';
				await openProject();
			}

			await emit('success', 'Sync complete!');
		} catch (e) {
			await emit('error', e);
		}

		syncing = false;
	};

	const handleOpenUprojectClicked = async () => {
		try {
			openingProject = true;
			progressModalText = 'Launching Unreal Engine';
			await openProject();
		} catch (e) {
			await emit('error', e);
		}

		openingProject = false;
	};

	const handleSyncClient = async (entry: Nullable<ArtifactEntry>, server: GameServerResult) => {
		if (!entry) {
			return;
		}

		progressModalCancellable = true;
		syncing = true;
		progressModalText = 'Syncing client...';
		const req: SyncClientRequest = {
			artifactEntry: entry,
			methodPrefix: $builds.methodPrefix,
			launchOptions: {
				name: server.name,
				launchMode: LaunchMode.WithServer
			}
		};

		try {
			if (await syncClient(req)) {
				currentSyncedVersion.set(entry.commit);
			}
		} catch (e) {
			await emit('error', e);
		}

		syncing = false;
		progressModalCancellable = false;
	};

	const handleSyncAndLaunch = async () => {
		const playtest = get(nextPlaytest);
		if (playtest) {
			const playtestAssignment = getPlaytestGroupForUser(playtest, $appConfig.userDisplayName);
			if (playtestAssignment && playtestAssignment.serverRef) {
				const project = playtest.metadata.annotations['believer.dev/project'];
				let entry = await getBuilds(250, project).then((a) =>
					a.entries.find((b) => b.commit === playtest.spec.version)
				);

				if (!entry) {
					entry = await getBuild(playtest.spec.version, project);

					if (!entry) {
						await emit('error', 'No build found for playtest');
						return;
					}
				}

				const updatedServers = await getServers(playtest.spec.version);
				const playtestServer = updatedServers.find(
					(s) => s.name === playtestAssignment.serverRef?.name
				);

				if (playtestServer && entry) {
					if (playtestServer.ready) {
						await handleSyncClient(entry, playtestServer);
					} else {
						await emit('error', 'Playtest server is not ready. Try again shortly.');
					}
				}
			}
		}
	};

	const getCommitMessage = (commit: Commit): string => {
		if (commit != null) {
			if (commit.message != null) {
				const trimmed: string = commit.message.split('\n')[0];
				if (trimmed != null) {
					return trimmed;
				}
			}
		}
		return 'No message';
	};

	const getCommitAuthor = (commit: Commit): string => {
		if (commit != null) {
			if (commit.author != null) {
				if (commit.author.name != null) {
					return commit.author.name;
				}
			}
		}

		return '';
	};

	onMount(() => {
		void refresh();

		const interval = setInterval(() => {
			void refreshMergeQueue();
		}, 10000);

		// Listen for git-refresh events to trigger repo status refresh with animation
		const unlistenGitRefresh = listen('git-refresh', () => {
			void refreshRepo();
		});

		return () => {
			clearInterval(interval);
			void unlistenGitRefresh.then((f) => {
				f();
			});
		};
	});
</script>

<div class="flex flex-col h-full gap-2 pb-20">
	<div class="flex flex-row gap-2">
		{#if displayedPlaytests.length > 0}
			<div class="flex flex-col gap-2 w-full overflow-x-auto overflow-y-hidden flex-grow min-w-0">
				<div class="flex mt-2 items-center gap-2">
					<p class="text-2xl text-primary-400 dark:text-primary-400">Next Playtest</p>
					<Button
						disabled={loadingPlaytests}
						class="!p-1.5"
						primary
						on:click={() => refreshPlaytests()}
					>
						{#if loadingPlaytests}
							<Spinner size="4" />
						{:else}
							<RefreshOutline class="w-4 h-4" />
						{/if}
					</Button>
				</div>

				<div class="flex-grow min-w-0 overflow-hidden">
					{#if displayedPlaytests[0].spec.disableGameServers === true}
						<ServerlessPlaytestsCard
							playtests={displayedPlaytests}
							bind:loading={loadingPlaytests}
							showTitle={false}
						/>
					{:else}
						<PlaytestCard
							playtest={displayedPlaytests[0]}
							bind:loading={loadingPlaytests}
							compact
						/>
					{/if}
				</div>
			</div>
		{:else}
			<div class="flex gap-2 items-center">
				<p class="text-white">There are no playtests!</p>
				<Button size="xs" href="/playtests">Playtests<LinkOutline class="ml-2 h-4 w-4" /></Button>
			</div>
		{/if}
		<div class="flex flex-col gap-2 max-w-[24rem] w-96 flex-shrink-0 h-fit">
			<div class="flex mt-2 items-center gap-2">
				<p class="text-2xl text-primary-400 dark:text-primary-400">Repo Status</p>
				<Button disabled={loadingRepoStatus} class="!p-1.5" primary on:click={refreshRepo}>
					{#if loadingRepoStatus}
						<Spinner size="4" />
					{:else}
						<RefreshOutline class="w-4 h-4" />
					{/if}
				</Button>
			</div>
			<Card
				class="w-full p-4 sm:p-4 max-w-full bg-secondary-700 dark:bg-space-900 border-0 shadow-none"
			>
				<div class="flex flex-col gap-4">
					<div class="flex flex-col gap-1">
						<div class="flex gap-2 items-center">
							<p class="w-60 text-white">Branch:</p>
							<p class="w-60 text-primary-400 dark:text-primary-400">{$repoStatus?.branch}</p>
						</div>
						<div class="flex gap-2 items-center">
							<p class="w-full text-white">
								Commits behind <code>{$appConfig?.targetBranch}</code>:
							</p>
							<p class="w-full text-primary-400 dark:text-primary-400">
								{$repoStatus?.commitsBehindTrunk}
							</p>
						</div>
						<div class="flex gap-2 items-center">
							<p class="w-full text-white">Modified files:</p>
							<p class="w-full text-primary-400 dark:text-primary-400">
								{$allModifiedFiles.length}
							</p>
						</div>
						<div class="flex gap-2 items-center">
							<p class="w-full text-white">Conflicting files:</p>
							<p class="w-full text-primary-400 dark:text-primary-400">
								{$repoStatus?.conflicts.length}
							</p>
						</div>
					</div>
					<div class="flex flex-col gap-2">
						<ButtonGroup size="xs" class="space-x-px w-full">
							<Button color="primary" class="w-full" on:click={handleSyncClicked}>Sync</Button>
							<Button color="primary" href="/source/submit" class="w-full"
								>Submit<LinkOutline class="ml-4 w-4 h-4l" /></Button
							>
						</ButtonGroup>
						<Button
							disabled={openingProject}
							size="xs"
							color="primary"
							on:click={async () => handleOpenUprojectClicked()}
						>
							<UnrealEngineLogoNoCircle class="w-3 h-3 mr-2" />
							Open Editor
						</Button>
					</div>
				</div>
			</Card>
		</div>
	</div>
	<div class="flex flex-col flex-1 overflow-hidden">
		<div class="flex items-center gap-2">
			<p class="text-2xl my-2 text-primary-400 dark:text-primary-400">Merge Queue</p>
			<Button
				disabled={loadingMergeQueue || !targetBranchConfig?.usesMergeQueue}
				class="!p-1.5"
				primary
				on:click={refreshMergeQueue}
			>
				{#if loadingMergeQueue}
					<Spinner size="4" />
				{:else}
					<RefreshOutline class="w-4 h-4" />
				{/if}
			</Button>
		</div>
		<Card
			class="w-full p-4 sm:p-4 max-w-full bg-secondary-700 dark:bg-space-900 border-0 shadow-none"
		>
			{#if targetBranchConfig && targetBranchConfig.usesMergeQueue}
				{#if mergeQueue !== null}
					{#if mergeQueue.entries.nodes.length === 0}
						<p class="text-white">No changes in queue!</p>
					{:else}
						<Table color="custom" divClass="w-full" class="w-full h-full" striped>
							<TableHead class="text-center border-b-0 p-2 bg-secondary-800 dark:bg-space-950">
								<TableHeadCell class="p-2">#</TableHeadCell>
								<TableHeadCell class="p-2">Message</TableHeadCell>
								<TableHeadCell class="p-2">Author</TableHeadCell>
								<TableHeadCell class="p-2">Submitted At</TableHeadCell>
								<TableHeadCell class="p-2">Status</TableHeadCell>
							</TableHead>
							<TableBody>
								{#each mergeQueue.entries.nodes as node, index}
									<TableBodyRow
										class="text-left border-b-0 p-2 {index % 2 === 0
											? 'bg-secondary-700 dark:bg-space-900'
											: 'bg-secondary-800 dark:bg-space-950'}"
									>
										<TableBodyCell id="pr-{index}" class="p-2">
											{index + 1}
										</TableBodyCell>
										<TableBodyCell
											class="p-2 text-primary-400 dark:text-primary-400 break-normal overflow-ellipsis overflow-hidden whitespace-nowrap w-1/2 max-w-[22vw]"
										>
											{getCommitMessage(node.headCommit)}
										</TableBodyCell>
										<TableBodyCell class="p-2 text-center"
											>{getCommitAuthor(node.headCommit)}</TableBodyCell
										>
										<TableBodyCell class="p-2 text-center"
											>{new Date(node.enqueuedAt).toLocaleString()}</TableBodyCell
										>
										<TableBodyCell class="p-2">
											<Badge
												class="text-white dark:text-white w-full {getMergeQueueEntryBadgeClass(
													node
												)}"
												>Queued
											</Badge>
										</TableBodyCell>
									</TableBodyRow>
								{/each}
							</TableBody>
						</Table>
					{/if}
				{/if}
			{:else}
				<p class="text-gray-400">Merge queue is not enabled for selected target branch.</p>
			{/if}
		</Card>

		<!-- Branch Comparison Section - directly underneath merge queue -->
		{#if shouldShowBranchComparison}
			<div class="flex items-center gap-2">
				<p class="text-2xl my-2 text-primary-400 dark:text-primary-400">
					Commits waiting for merge to Content Branch
				</p>
				<Button
					disabled={loadingBranchComparison}
					class="!p-1.5"
					primary
					on:click={refreshBranchComparison}
				>
					{#if loadingBranchComparison}
						<Spinner size="4" />
					{:else}
						<RefreshOutline class="w-4 h-4" />
					{/if}
				</Button>
			</div>
			<Card
				class="w-full p-4 sm:p-4 max-w-full bg-secondary-700 dark:bg-space-900 border-0 shadow-none"
			>
				{#if branchComparison !== null}
					{#if branchComparison.length === 0}
						<p class="text-white">No differences between branches!</p>
					{:else}
						<Table color="custom" divClass="w-full" class="w-full h-full" striped>
							<TableHead class="text-center border-b-0 p-2 bg-secondary-800 dark:bg-space-950">
								<TableHeadCell class="p-2">Commit</TableHeadCell>
								<TableHeadCell class="p-2">Message</TableHeadCell>
								<TableHeadCell class="p-2">Author</TableHeadCell>
								<TableHeadCell class="p-2">Timestamp</TableHeadCell>
							</TableHead>
							<TableBody>
								{#each branchComparison as commit, index}
									<TableBodyRow
										class="text-left border-b-0 p-2 {index % 2 === 0
											? 'bg-secondary-700 dark:bg-space-900'
											: 'bg-secondary-800 dark:bg-space-950'}"
									>
										<TableBodyCell class="p-2 font-mono text-sm">
											{#if commit.status === 'success'}
												<span class="text-xs pr-1">🟢</span>
											{:else if commit.status === 'pending'}
												<span class="text-xs pr-1">🟡</span>
											{:else if commit.status === 'error' || commit.status === 'failure'}
												<span class="text-xs pr-1">🔴</span>
											{:else}
												<span class="text-xs pr-1">⚪</span>
											{/if}
											{commit.sha}
										</TableBodyCell>
										<TableBodyCell
											class="p-2 text-primary-400 dark:text-primary-400 break-normal overflow-ellipsis overflow-hidden whitespace-nowrap w-1/2 max-w-[22vw]"
										>
											{commit.message || 'No message'}
										</TableBodyCell>
										<TableBodyCell class="p-2 text-center">
											{commit.author || 'Unknown'}
										</TableBodyCell>
										<TableBodyCell class="p-2 text-center">
											{commit.timestamp ? new Date(commit.timestamp).toLocaleString() : 'Unknown'}
										</TableBodyCell>
									</TableBodyRow>
								{/each}
							</TableBody>
						</Table>
					{/if}
				{:else}
					<p class="text-gray-400">Loading branch comparison...</p>
				{/if}
			</Card>
		{/if}
	</div>
</div>

{#key $nextPlaytest?.status}
	<Button
		disabled={shouldDisableLaunchButton() || $backgroundSyncInProgress}
		size="xl"
		class="fixed bottom-6 right-6 shadow-2xl"
		on:click={handleSyncAndLaunch}
		>{getMainButtonText()}
	</Button>
{/key}

<ProgressModal
	title={progressModalText}
	bind:showModal={syncing}
	cancellable={progressModalCancellable}
	on:cancel={handleSyncCancelled}
/>
