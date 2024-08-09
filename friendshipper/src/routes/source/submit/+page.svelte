<script lang="ts">
	import {
		Badge,
		Button,
		ButtonGroup,
		Card,
		Input,
		Label,
		Modal,
		Select,
		Spinner,
		TabItem,
		Table,
		TableBody,
		TableBodyCell,
		TableBodyRow,
		TableHead,
		TableHeadCell,
		Tabs,
		Textarea,
		Tooltip
	} from 'flowbite-svelte';
	import { LinkOutline, QuestionCircleOutline, RefreshOutline } from 'flowbite-svelte-icons';
	import { onDestroy, onMount } from 'svelte';
	import { emit } from '@tauri-apps/api/event';
	import { open } from '@tauri-apps/api/shell';
	import { sendNotification } from '@tauri-apps/api/notification';
	import { type CommitFileInfo, ModifiedFilesCard, ProgressModal } from '@ethos/core';
	import { get } from 'svelte/store';
	import type {
		GitHubPullRequest,
		Nullable,
		PushRequest,
		RepoStatus,
		RevertFilesRequest,
		Snapshot
	} from '$lib/types';
	import {
		acquireLocks,
		deleteSnapshot,
		getCommitFileTextClass,
		getPullRequests,
		getRepoStatus,
		listSnapshots,
		quickSubmit,
		restoreSnapshot,
		revertFiles,
		saveSnapshot,
		showCommitFiles
	} from '$lib/repo';
	import {
		allModifiedFiles,
		appConfig,
		commitMessage,
		repoConfig,
		repoStatus,
		selectedFiles
	} from '$lib/stores';
	import { openUrl } from '$lib/utils';

	let loading = false;
	let fetchingPulls = false;
	const quickSubmitting = false;
	let promptForPAT = false;
	let preferencesOpen = false;

	// commit inputs
	let tempCommitType = '';
	let tempCommitScope = '';
	let tempCommitMessage = '';
	let commitMessageValid = false;

	// commit file details
	let expandedCommit = '';
	let loadingCommitFiles = false;
	let commitFiles: CommitFileInfo[] = [];

	// progress modal
	let showProgressModal = false;
	let progressModalTitle = '';

	let selectAll = false;
	let pulls: GitHubPullRequest[] = [];

	let loadingSnapshots = false;
	let snapshots: Snapshot[] = [];

	const validateCommitMessage = (): boolean => {
		const message = get(commitMessage);
		if (typeof message === 'string') {
			return message !== '';
		}

		return message.type !== '' && message.scope !== '' && message.message !== '';
	};

	const unsubscribeRepoStatus = repoStatus.subscribe((inRepoStatus: Nullable<RepoStatus>) => {
		$selectedFiles = $selectedFiles.filter(
			(file) =>
				inRepoStatus?.modifiedFiles.some((f) => f.path === file.path) ||
				inRepoStatus?.untrackedFiles.some((f) => f.path === file.path)
		);
	});

	$: canSubmit = $selectedFiles.length > 0 && get(commitMessage) !== '' && commitMessageValid;

	const handleOpenDirectory = async (path: string) => {
		const parent = path.split('/').slice(0, -1).join('/');

		const fullPath = `${$appConfig.repoPath}/${parent}`;

		await openUrl(fullPath);
	};

	const refreshFiles = async (triggerLoading: boolean) => {
		if (triggerLoading) {
			loading = true;
		}

		try {
			$repoStatus = await getRepoStatus();
		} catch (e) {
			await emit('error', e);
		}

		if (triggerLoading) {
			loading = false;
		}
	};

	const refreshPulls = async () => {
		fetchingPulls = true;
		try {
			const newPulls = await getPullRequests(10);

			// check if any pull requests have been merged
			const currentMergedPulls = pulls.filter((pull) => pull.state === 'MERGED');
			const newMergedPulls = newPulls.filter((pull) => pull.state === 'MERGED');
			const mergedPulls = newMergedPulls.filter(
				(pull) => !currentMergedPulls.some((p) => p.number === pull.number)
			);

			if (mergedPulls.length > 0 && pulls.length > 0) {
				sendNotification({
					title: 'Friendshipper',
					body: 'Quick Submit changes merged!',
					icon: '/assets/icon.png'
				});
			}

			pulls = newPulls;
		} catch (e) {
			await emit('error', e);
		}
		fetchingPulls = false;
	};

	const refreshSnapshots = async () => {
		loadingSnapshots = true;
		try {
			snapshots = await listSnapshots();
		} catch (e) {
			await emit('error', e);
		}
		loadingSnapshots = false;
	};

	const handleRestoreSnapshot = async (commit: string) => {
		loadingSnapshots = true;
		showProgressModal = true;
		progressModalTitle = 'Restoring snapshot';

		try {
			await restoreSnapshot(commit);

			$selectedFiles = [];
			selectAll = false;

			await refreshFiles(true);

			await emit('success', 'Snapshot restored!');
		} catch (e) {
			await emit('error', e);
		}

		loadingSnapshots = false;
		showProgressModal = false;
	};

	const handleDeleteSnapshot = async (commit: string) => {
		loadingSnapshots = true;

		try {
			await deleteSnapshot(commit);

			$selectedFiles = [];
			selectAll = false;

			await refreshSnapshots();

			await emit('success', 'Snapshot deleted!');
		} catch (e) {
			await emit('error', e);
		}

		loadingSnapshots = false;
	};

	const setExpandedCommit = async (commit: string) => {
		expandedCommit = commit;

		if (commit === '') {
			commitFiles = [];
			return;
		}

		loadingCommitFiles = true;
		commitFiles = await showCommitFiles(commit, true);
		loadingCommitFiles = false;
	};

	const handleRevertFiles = async () => {
		loading = true;
		showProgressModal = true;
		progressModalTitle = 'Reverting files';

		await refreshFiles(false);

		const req: RevertFilesRequest = {
			files: $selectedFiles.map((file) => file.path),
			skipEngineCheck: false
		};

		try {
			await revertFiles(req);

			$repoStatus = await getRepoStatus();

			$selectedFiles = [];
			selectAll = false;
		} catch (e) {
			await emit('error', e);
		}

		await refreshFiles(false);

		loading = false;
		showProgressModal = false;
	};

	const handleSaveSnapshot = async () => {
		loading = true;
		showProgressModal = true;
		progressModalTitle = 'Saving snapshot';

		await refreshFiles(false);

		try {
			const currentCommitMessage = get(commitMessage);
			const message =
				typeof currentCommitMessage === 'string'
					? currentCommitMessage
					: `${currentCommitMessage.type}(${currentCommitMessage.scope}): ${currentCommitMessage.message}`;

			await saveSnapshot(
				message.length > 0 ? message : 'No message provided',
				$selectedFiles.map((file) => file.path)
			);

			$selectedFiles = [];
			selectAll = false;

			await emit('success', 'Snapshot saved!');

			await refreshSnapshots();
		} catch (e) {
			await emit('error', e);
		}

		loading = false;
		showProgressModal = false;
	};

	const handleQuickSubmit = async () => {
		loading = true;
		showProgressModal = true;
		progressModalTitle = 'Opening pull request';

		await refreshFiles(false);

		const message = get(commitMessage);

		const req: PushRequest = {
			commitMessage:
				typeof message === 'string'
					? message
					: `${message.type}(${message.scope}): ${message.message}`,
			files: $selectedFiles.map((file) => file.path)
		};

		try {
			await quickSubmit(req);

			$commitMessage = '';

			// note that we don't reset tempCommitType because the UI already has a value selected
			tempCommitScope = '';
			tempCommitMessage = '';

			$selectedFiles = [];
			selectAll = false;

			await refreshPulls();

			await emit('success', 'Pull request opened!');
		} catch (e) {
			await emit('error', e);
		}

		// refresh files after quick submit, whether it was successful or not
		await refreshFiles(true);

		showProgressModal = false;
		loading = false;
	};

	const handleOpenPreferences = async () => {
		promptForPAT = false;
		preferencesOpen = true;
		await emit('open-preferences');
	};
	const getStatusBadgeText = (pull: GitHubPullRequest): string => {
		if (pull.state === 'OPEN') {
			if (pull.mergeable === 'CONFLICTING') {
				return 'Conflicts';
			}

			if (pull.mergeQueueEntry !== null) {
				if (
					pull.mergeQueueEntry.state === 'QUEUED' ||
					pull.mergeQueueEntry.state === 'AWAITING_CHECKS'
				) {
					return 'Queued';
				}

				if (pull.mergeQueueEntry.state === 'UNMERGEABLE') {
					return 'Unmergeable';
				}
			}
			return 'Open';
		}
		if (pull.state === 'MERGED') {
			return 'Merged';
		}
		if (pull.state === 'CLOSED') {
			return 'Closed';
		}

		return '';
	};

	const getStatusBadgeClass = (pull: GitHubPullRequest): string => {
		if (pull.state === 'OPEN') {
			if (pull.mergeable === 'CONFLICTING') {
				return 'bg-red-700 dark:bg-red-700';
			}

			if (pull.mergeQueueEntry !== null) {
				if (
					pull.mergeQueueEntry.state === 'QUEUED' ||
					pull.mergeQueueEntry.state === 'AWAITING_CHECKS'
				) {
					return 'bg-yellow-500 dark:bg-yellow-500 animate-pulse';
				}

				if (pull.mergeQueueEntry.state === 'UNMERGEABLE') {
					return 'bg-red-700 dark:bg-red-700';
				}
			}
		}
		if (pull.state === 'MERGED') {
			return 'bg-lime-600 dark:bg-lime-600';
		}
		if (pull.state === 'CLOSED') {
			return 'bg-red-800 dark:bg-red-800';
		}

		return 'bg-primary-500 dark:bg-primary-500';
	};

	const refreshLocks = async () => {
		loading = true;
		try {
			repoStatus.set(await getRepoStatus());
		} catch (e) {
			await emit('error', e);
		}
		loading = false;
	};

	const handleLockSelected = async () => {
		loading = true;
		showProgressModal = true;
		progressModalTitle = 'Locking files';

		try {
			const selectedPaths = $selectedFiles.map((file) => file.path);
			await acquireLocks(selectedPaths, false);
			await emit('success', 'Files locked!');
			await refreshLocks();
		} catch (e) {
			await emit('error', e);
		}

		loading = false;
		showProgressModal = false;
	};

	onMount(() => {
		void refreshFiles(true);
		void refreshSnapshots();

		const currentCommitMessage = get(commitMessage);
		if (typeof currentCommitMessage === 'string') {
			tempCommitMessage = currentCommitMessage;
		} else {
			tempCommitType = currentCommitMessage.type;
			tempCommitScope = currentCommitMessage.scope;
			tempCommitMessage = currentCommitMessage.message;
		}

		if ($appConfig.githubPAT === '') {
			if (!preferencesOpen) {
				promptForPAT = true;
			}
		} else {
			void refreshPulls();
		}

		const interval = setInterval(() => {
			if (!quickSubmitting && !loadingSnapshots) {
				void refreshSnapshots();
			}
		}, 10000);

		const pullsInterval = setInterval(() => {
			if (!fetchingPulls && !preferencesOpen) {
				if ($appConfig.githubPAT !== '') {
					void refreshPulls();
				} else {
					promptForPAT = true;
				}
			}
		}, 10000);

		return () => {
			clearInterval(interval);
			clearInterval(pullsInterval);
		};
	});

	onDestroy(() => {
		unsubscribeRepoStatus();
	});
</script>

<div class="flex items-center justify-between gap-2">
	<div class="flex items-center gap-2">
		<p class="text-2xl my-2 text-primary-400 dark:text-primary-400">Submit Changes</p>
		<Button disabled={loading} class="!p-1.5" primary on:click={() => refreshFiles(true)}>
			{#if loading}
				<Spinner size="4" />
			{:else}
				<RefreshOutline class="w-4 h-4" />
			{/if}
		</Button>
	</div>
</div>
<div class="flex flex-row h-full gap-2 overflow-auto">
	<div class="flex flex-col gap-2 w-full h-full overflow-x-auto">
		<ModifiedFilesCard
			disabled={loading}
			bind:selectedFiles={$selectedFiles}
			bind:selectAll
			modifiedFiles={$allModifiedFiles}
			onOpenDirectory={handleOpenDirectory}
			onRevertFiles={handleRevertFiles}
			onSaveSnapshot={handleSaveSnapshot}
			onLockSelected={handleLockSelected}
		/>
	</div>
	<div class="flex flex-col h-full gap-2 w-full max-w-[32rem]">
		<Card
			class="w-full h-full p-4 sm:p-4 max-w-full max-h-16 bg-secondary-700 dark:bg-space-900 border-0 shadow-none"
		>
			<div class="flex flex-row items-center justify-between gap-2">
				<p class="font-semibold text-sm text-gray-400">
					On branch: <span class="font-normal text-primary-400">{$repoStatus?.branch}</span>
				</p>
			</div>
		</Card>
		<Card
			class="w-full p-4 sm:p-4 max-w-full h-full bg-secondary-700 dark:bg-space-900 border-0 shadow-none"
		>
			<div class="flex flex-col w-full h-full gap-2">
				<div
					class="flex gap-2 w-full items-center"
					class:justify-between={$repoConfig?.commitGuidelinesUrl}
				>
					<Label for="commit-message" class="text-white w-full">Commit Message</Label>
					{#if $repoConfig?.commitGuidelinesUrl}
						<div class="flex flex-row w-full align-middle justify-end">
							<ButtonGroup class="space-x-px">
								<Button
									outline
									size="xs"
									color="primary"
									class="py-1"
									on:click={async () => {
										if ($repoConfig?.commitGuidelinesUrl) {
											await open($repoConfig.commitGuidelinesUrl);
										}
									}}
									>Commit Guidelines
									<LinkOutline class="w-6 pl-2 align-middle" />
								</Button>
							</ButtonGroup>
						</div>
					{/if}
				</div>
				{#if $repoConfig?.useConventionalCommits}
					<div class="flex gap-2 w-full">
						<Select
							bind:value={tempCommitType}
							placeholder="Choose commit type"
							on:change={() => {
								$commitMessage = {
									type: tempCommitType,
									scope: tempCommitScope,
									message: tempCommitMessage
								};
								commitMessageValid = validateCommitMessage();
							}}
							class="text-white bg-secondary-800 dark:bg-space-950"
						>
							{#each $repoConfig?.conventionalCommitsAllowedTypes as type}
								<option value={type}>{type}</option>
							{/each}
						</Select>
						<Input
							type="text"
							bind:value={tempCommitScope}
							on:keyup={() => {
								$commitMessage = {
									type: tempCommitType,
									scope: tempCommitScope,
									message: tempCommitMessage
								};
								commitMessageValid = validateCommitMessage();
							}}
							class="text-white bg-secondary-800 dark:bg-space-950"
							placeholder="Scope (required)"
						/>
					</div>
				{/if}
				<Textarea
					id="commit-message"
					placeholder="Message (required)"
					bind:value={tempCommitMessage}
					on:keyup={() => {
						if ($repoConfig?.useConventionalCommits) {
							$commitMessage = {
								type: tempCommitType,
								scope: tempCommitScope,
								message: tempCommitMessage
							};
						} else {
							$commitMessage = tempCommitMessage;
						}

						commitMessageValid = validateCommitMessage();
					}}
					class="text-white bg-secondary-800 dark:bg-space-950 min-h-[4rem] h-full border-gray-400"
				/>
				<div class="flex flex-row w-full align-middle justify-end">
					<ButtonGroup class="space-x-px">
						<Button
							id="quick-submit"
							color="primary"
							disabled={!canSubmit}
							on:click={handleQuickSubmit}
							>Quick Submit
							<QuestionCircleOutline class="w-6 pl-2 align-middle" />
						</Button>
					</ButtonGroup>
				</div>
			</div>
		</Card>
	</div>
</div>
<Card
	class="w-full p-4 mt-2 sm:p-4 max-w-full min-h-[16rem] bg-secondary-700 dark:bg-space-900 border-0 shadow-none"
>
	<Tabs
		style="underline"
		divider={false}
		contentClass="bg-secondary-700 dark:bg-space-900 h-full overflow-y-auto"
	>
		<TabItem open title="My Submits ({pulls.length})" class="bg-secondary-700 dark:bg-space-900">
			<Table color="custom" striped>
				<TableHead class="text-left border-b-0 p-2 bg-secondary-800 dark:bg-space-950">
					<TableHeadCell class="p-2">Number</TableHeadCell>
					<TableHeadCell class="p-2">Title</TableHeadCell>
					<TableHeadCell class="p-2">Status</TableHeadCell>
					<TableHeadCell class="p-2">Created/Merged At</TableHeadCell>
				</TableHead>
				<TableBody>
					{#each pulls as pull, index}
						<TableBodyRow
							class="text-left border-b-0 p-2 {index % 2 === 0
								? 'bg-secondary-700 dark:bg-space-900'
								: 'bg-secondary-800 dark:bg-space-950'}"
						>
							<TableBodyCell id="pr-{index}" class="p-2">
								<Button
									size="sm"
									class="p-2 py-0 flex gap-1 border-none bg-blue-500 dark:bg-blue-500 hover:bg-blue-600 dark:hover:bg-blue-600 border-r-2"
									on:click={() => open(pull.permalink)}
								>
									<LinkOutline class="w-3 h-3" />
									{pull.number}
								</Button>
							</TableBodyCell>
							<TableBodyCell class="p-2">
								{pull.title}
							</TableBodyCell>
							<TableBodyCell class="p-2"
								><Badge class="text-white dark:text-white w-full {getStatusBadgeClass(pull)}"
									>{getStatusBadgeText(pull)}</Badge
								></TableBodyCell
							>
							<TableBodyCell class="p-2"
								>{new Date(
									pull.mergedAt !== null ? pull.mergedAt : pull.createdAt
								).toLocaleString()}</TableBodyCell
							>
						</TableBodyRow>
					{:else}
						<TableBodyRow>
							<TableBodyCell class="p-2" />
							<TableBodyCell class="p-2">You have no open pull requests.</TableBodyCell>
						</TableBodyRow>
					{/each}
				</TableBody>
			</Table>
		</TabItem>
		<TabItem title="Snapshots ({snapshots.length})">
			<Table color="custom" striped>
				<TableHead
					align="center"
					class="text-center border-b-0 p-2 bg-secondary-800 dark:bg-space-950"
				>
					<TableHeadCell class="p-2">Timestamp</TableHeadCell>
					<TableHeadCell class="p-2">Commit</TableHeadCell>
					<TableHeadCell class="p-2 text-center">Message</TableHeadCell>
					<TableHeadCell class="p-2 text-center">Actions</TableHeadCell>
				</TableHead>
				<TableBody>
					{#each snapshots as snapshot, index}
						<TableBodyRow
							align="center"
							class="text-left border-b-0 p-2 {index % 2 === 0
								? 'bg-secondary-800 dark:bg-space-900'
								: 'bg-secondary-700 dark:bg-space-950'}"
						>
							<TableBodyCell class="p-2 w-16">
								{new Date(snapshot.timestamp).toLocaleString()}
							</TableBodyCell>
							<TableBodyCell class="p-2 w-8">
								<code>{snapshot.commit.substring(0, 7)}</code>
							</TableBodyCell>
							<TableBodyCell class="p-2 text-center max-w-[20rem] truncate">
								{snapshot.message}
							</TableBodyCell>
							<TableBodyCell class="flex justify-center p-2">
								<ButtonGroup class="space-x-px">
									<Button
										disabled={loadingSnapshots}
										color="primary"
										size="xs"
										on:click={async () => {
											await handleRestoreSnapshot(snapshot.commit);
										}}>Restore</Button
									>
									<Button
										size="xs"
										color="primary"
										on:click={() =>
											expandedCommit === snapshot.commit
												? setExpandedCommit('')
												: setExpandedCommit(snapshot.commit)}
									>
										{#if expandedCommit === snapshot.commit}
											Hide Files
										{:else}
											Show Files
										{/if}
									</Button>
									<Button
										size="xs"
										color="red"
										on:click={() => handleDeleteSnapshot(snapshot.commit)}
									>
										Delete
									</Button>
								</ButtonGroup>
							</TableBodyCell>
						</TableBodyRow>
						{#if expandedCommit === snapshot.commit}
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
						<TableBodyRow>
							<TableBodyCell class="p-2" />
							<TableBodyCell class="p-2">You have no snapshots.</TableBodyCell>
						</TableBodyRow>
					{/each}
				</TableBody>
			</Table>
		</TabItem>
	</Tabs>
</Card>
<Tooltip
	triggeredBy="#quick-submit"
	class="w-auto bg-secondary-700 dark:bg-space-900 font-semibold shadow-2xl"
	placement="bottom"
	><p>
		<span class="text-primary-400">Quick Submit</span> allows you to submit changes without syncing
		latest from <span class="font-mono text-primary-400">main</span>.<br /><br />
		This will open a pull request on GitHub and automatically merge it, putting your changes into the
		merge queue. Because of this, you may need to wait for other builds in the merge queue to finish
		before your changes will appear on
		<span class="font-mono text-primary-400">main</span>.<br /><br />
		You may only <span class="text-primary-400">Quick Submit</span> when on
		<span class="text-primary-400">main</span>.
	</p>
</Tooltip>

<Modal
	open={promptForPAT}
	dismissable={false}
	class="bg-secondary-700 dark:bg-space-900"
	backdropClass="fixed mt-8 inset-0 z-40 bg-gray-900 bg-opacity-50 dark:bg-opacity-80"
	dialogClass="fixed mt-8 top-0 start-0 end-0 h-modal md:inset-0 md:h-full z-50 w-full p-4 pb-12 flex"
>
	<div class="flex items-center justify-between gap-2">
		<span>Looks like you haven't provided a GitHub Personal Access Token yet!</span>
		<Button size="xs" on:click={handleOpenPreferences}>Open Preferences</Button>
	</div>
</Modal>

<ProgressModal showModal={showProgressModal} title={progressModalTitle} />
