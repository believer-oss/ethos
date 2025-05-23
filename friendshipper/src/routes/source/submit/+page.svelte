<script lang="ts">
	import {
		Badge,
		Button,
		ButtonGroup,
		Card,
		Dropdown,
		DropdownItem,
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
	import {
		LinkOutline,
		QuestionCircleOutline,
		RefreshOutline,
		FileCodeSolid,
		ChevronDownOutline
	} from 'flowbite-svelte-icons';
	import { onDestroy, onMount } from 'svelte';
	import { emit } from '@tauri-apps/api/event';
	import { open } from '@tauri-apps/plugin-shell';
	import { sendNotification } from '@tauri-apps/plugin-notification';
	import {
		type ChangeSet,
		type CommitFileInfo,
		type ModifiedFile,
		ModifiedFilesCard,
		ProgressModal
	} from '@ethos/core';
	import { get } from 'svelte/store';
	import { Menu, MenuItem } from '@tauri-apps/api/menu';
	import {
		ConflictStrategy,
		type GitHubPullRequest,
		type Nullable,
		type PushRequest,
		type RepoStatus,
		type RevertFilesRequest,
		type Snapshot
	} from '$lib/types';
	import {
		acquireLocks,
		deleteSnapshot,
		forceDownloadDlls,
		forceDownloadEngine,
		generateSln,
		getCommitFileTextClass,
		getPullRequests,
		getRepoStatus,
		listSnapshots,
		openProject,
		openSln,
		quickSubmit,
		reinstallGitHooks,
		restoreSnapshot,
		revertFiles,
		saveChangeSet,
		saveSnapshot,
		showCommitFiles,
		syncEngineCommitWithUproject,
		syncLatest,
		syncUprojectWithEngineCommit
	} from '$lib/repo';
	import {
		activeProjectConfig,
		allModifiedFiles,
		appConfig,
		changeSets,
		commitMessage,
		repoConfig,
		repoStatus,
		selectedFiles
	} from '$lib/stores';
	import { openUrl } from '$lib/utils';
	import UnrealEngineLogoNoCircle from '$lib/icons/UnrealEngineLogoNoCircle.svelte';
	import { openUrlForPath } from '$lib/engine';

	let loading = false;
	let fetchingPulls = false;
	let quickSubmitting = false;
	let syncing = false;
	let promptForPAT = false;
	let promptRevertUProject = false;
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

	$: conflictsDetected =
		($repoStatus?.conflicts.length ?? 0) > 0 &&
		$appConfig.conflictStrategy === ConflictStrategy.Error;
	$: canSync = !quickSubmitting && !syncing;

	const onModifiedFileRightClick = async (e: MouseEvent, file: ModifiedFile) => {
		e.preventDefault();

		const menuPromise = Menu.new({
			items: [
				{
					id: 'copy-file-path',
					text: 'Copy File Path',
					action: () => {
						void navigator.clipboard.writeText(file.path);
					}
				}
			]
		});

		const menu = await menuPromise;
		if (file.displayName !== file.path) {
			await menu.append(
				await MenuItem.new({
					id: 'copy-friendly-path',
					text: 'Copy Friendly Path',
					action: () => {
						void navigator.clipboard.writeText(file.displayName);
					}
				})
			);
		}

		if (file.url) {
			await menu.append(
				await MenuItem.new({
					id: 'open-file-in-editor',
					text: 'Open File in Editor',
					action: () => {
						void openUrlForPath(file.path).catch((error) => {
							void emit('error', error);
						});
					}
				})
			);
		}
		await menu.popup();
	};

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

		// If a user is pulling their own game DLLs, they likely are not an engineer and should not be
		// making changes to the .uproject.
		if ($appConfig.pullDlls && $appConfig.engineType === 'Prebuilt') {
			const files = inRepoStatus?.modifiedFiles ?? [];
			for (const file of files) {
				if (file.path === $repoConfig?.uprojectPath) {
					promptRevertUProject = true;
				}
			}
		}
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
			const newPulls = await getPullRequests(100);

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
		syncing = true;
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
		syncing = false;
	};

	const handleDeleteSnapshot = async (commit: string) => {
		loadingSnapshots = true;
		syncing = true;
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
		syncing = false;
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
		syncing = true;
		showProgressModal = true;
		progressModalTitle = 'Reverting files';

		await refreshFiles(false);

		const req: RevertFilesRequest = {
			files: $selectedFiles.map((file) => file.path),
			skipEngineCheck: false,
			takeSnapshot: true
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
		syncing = false;
	};

	const handleSaveSnapshot = async () => {
		if (loading) {
			await emit('error', 'Please wait for the current operation to complete.');
			return;
		}

		loading = true;
		syncing = true;
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
		syncing = false;
	};

	const handleQuickSubmit = async () => {
		loading = true;
		quickSubmitting = true;
		showProgressModal = true;
		progressModalTitle = 'Opening pull request';

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
		progressModalTitle = 'Refreshing files';
		await refreshFiles(true);

		showProgressModal = false;
		loading = false;
		quickSubmitting = false;
	};

	const handleSaveChangesets = async (newChangesets: ChangeSet[]) => {
		$changeSets = newChangesets;
		if ($activeProjectConfig === null) {
			await emit('error', 'No active project found, unable to save changesets to file.');
			return;
		}
		await saveChangeSet($changeSets);
	};

	const handleSyncClicked = async () => {
		try {
			loading = true;
			syncing = true;
			showProgressModal = true;
			progressModalTitle = 'Pulling latest with git';

			await syncLatest();
			await refreshFiles(true);

			if (!$appConfig.pullDlls) {
				progressModalTitle = 'Generating projects';
				await generateSln();
			} else if ($appConfig.openUprojectAfterSync) {
				progressModalTitle = 'Launching Unreal Engine';
				await openProject();
			}

			await emit('success', 'Sync complete!');
		} catch (e) {
			await emit('error', e);
		}

		showProgressModal = false;
		loading = false;
		syncing = false;
	};

	const handleOpenUprojectClicked = async () => {
		try {
			loading = true;
			showProgressModal = true;
			progressModalTitle = 'Launching Unreal Engine';
			await openProject();
		} catch (e) {
			await emit('error', e);
		}

		loading = false;
		showProgressModal = false;
	};

	const handleOpenSolutionClicked = async () => {
		try {
			loading = true;
			showProgressModal = true;
			progressModalTitle = 'Opening Solution';
			await openSln();
		} catch (e) {
			await emit('error', e);
		}

		loading = false;
		showProgressModal = false;
	};

	const handleGenerateProjectFiles = async () => {
		try {
			loading = true;
			showProgressModal = true;
			progressModalTitle = 'Generating project files';
			await generateSln();
			await emit('success', 'Visual Studio projects generated successfully');
		} catch (e) {
			await emit('error', e);
		}

		loading = false;
		showProgressModal = false;
	};

	const handleForceDownloadGameDllsClicked = async () => {
		try {
			loading = true;
			showProgressModal = true;
			progressModalTitle = 'Downloading game DLLs';
			await forceDownloadDlls();
		} catch (e) {
			await emit('error', e);
		}

		loading = false;
		showProgressModal = false;
	};

	const handleForceDownloadEngineClicked = async () => {
		try {
			loading = true;
			showProgressModal = true;
			progressModalTitle = 'Downloading engine DLLs';
			await forceDownloadEngine();
		} catch (e) {
			await emit('error', e);
		}

		loading = false;
		showProgressModal = false;
	};

	const handleSyncUprojectWithEngineRepo = async () => {
		try {
			loading = true;
			showProgressModal = true;
			progressModalTitle = 'Syncing uproject with engine commit...';
			const commit = await syncUprojectWithEngineCommit();
			await emit('success', `UProject EngineAssociation synced to commit ${commit}`);
		} catch (e) {
			await emit('error', e);
		}

		loading = false;
		showProgressModal = false;
	};

	const handleSyncEngineRepoWithUproject = async () => {
		try {
			loading = true;
			showProgressModal = true;
			progressModalTitle = 'Syncing uproject with engine commit...';
			const commit = await syncEngineCommitWithUproject();
			await emit('success', `Engine repo synced to commit ${commit}`);
		} catch (e) {
			await emit('error', e);
		}

		loading = false;
		showProgressModal = false;
	};

	const handleReinstallGitHooksClicked = async () => {
		try {
			loading = true;
			showProgressModal = true;
			progressModalTitle = 'Reinstalling Git hooks';
			await reinstallGitHooks();
			await emit('success', 'Git hooks installed successfully.');
		} catch (e) {
			await emit('error', e);
		}

		loading = false;
		showProgressModal = false;
	};

	const handleOpenPreferences = async () => {
		promptForPAT = false;
		preferencesOpen = true;
		await emit('open-preferences');
	};

	const handleRevertUproject = async () => {
		promptRevertUProject = false;

		loading = true;
		syncing = true;
		showProgressModal = true;
		progressModalTitle = `Reverting ${$repoConfig?.uprojectPath}`;

		const req: RevertFilesRequest = {
			files: [$repoConfig?.uprojectPath],
			skipEngineCheck: false,
			takeSnapshot: true
		};

		try {
			await revertFiles(req);

			const SelectedIndex = $selectedFiles.indexOf(req.files[0]);
			if (SelectedIndex > -1) {
				$selectedFiles.splice(SelectedIndex, 1);
			}
			selectAll = false;
		} catch (e) {
			await emit('error', e);
		}

		await refreshFiles(false);

		loading = false;
		promptRevertUProject = false;
		showProgressModal = false;
		syncing = false;
	};

	const handleCloseRevertUproject = () => {
		promptRevertUProject = false;
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
	<div class="flex items-center gap-2 justify-between">
		<p class="text-2xl my-2 text-primary-400 dark:text-primary-400">Submit Changes</p>
		<Button disabled={loading} class="!p-1.5" primary on:click={() => refreshFiles(true)}>
			{#if loading}
				<Spinner size="4" />
			{:else}
				<RefreshOutline class="w-4 h-4" />
			{/if}
		</Button>
	</div>
	<ButtonGroup size="xs" class="space-x-px">
		<Button
			size="xs"
			color="primary"
			disabled={loading || conflictsDetected || !canSync}
			on:click={async () => handleSyncClicked()}
		>
			<RefreshOutline class="w-3 h-3 mr-2" />
			Sync
		</Button>
		{#if conflictsDetected}
			<Tooltip
				class="ml-2 w-36 text-sm text-primary-400 bg-secondary-800 dark:bg-space-950"
				placement="bottom"
				>Conflicts detected!
			</Tooltip>
		{/if}
		<Button
			size="xs"
			color="primary"
			disabled={!canSync}
			on:click={async () => handleOpenUprojectClicked()}
		>
			<UnrealEngineLogoNoCircle class="w-3 h-3 mr-2" />
			Open Editor
		</Button>
		{#if !$appConfig.pullDlls}
			<Button
				size="xs"
				color="primary"
				disabled={!canSync}
				on:click={async () => handleOpenSolutionClicked()}
			>
				<FileCodeSolid class="w-3.5 h-3.5 mr-2" />
				Open .sln
			</Button>
		{/if}
		<Button size="xs" color="primary" id="advancedDropdown" disabled={!canSync}>
			<ChevronDownOutline size="xs" />
		</Button>
	</ButtonGroup>
	<Dropdown placement="bottom-start" triggeredBy="#advancedDropdown">
		{#if $appConfig.pullDlls}
			<DropdownItem class="text-xs" on:click={handleForceDownloadGameDllsClicked}
				>Redownload game DLLs</DropdownItem
			>
			<Tooltip class="text-xs w-[22rem]" placement="left"
				>Downloads game DLLs for your current commit and installs them into the game repo. Use if
				you are getting incompatible binaries errors.</Tooltip
			>
		{:else}
			<DropdownItem class="text-xs" on:click={handleGenerateProjectFiles}
				>Generate project files</DropdownItem
			>
			<Tooltip class="text-xs w-[22rem]" placement="left"
				>Generates Visual Studio solution and project files for the uproject.</Tooltip
			>
		{/if}
		{#if $appConfig.engineType === 'Prebuilt'}
			<DropdownItem class="text-xs" on:click={handleForceDownloadEngineClicked}
				>Redownload engine</DropdownItem
			>
			<Tooltip class="text-xs w-[22rem]" placement="left"
				>Redownloads the entire engine archive. Use if you suspect you have a corrupt engine
				install.</Tooltip
			>
		{:else}
			<DropdownItem class="text-xs" on:click={handleSyncUprojectWithEngineRepo}
				>Sync UProject with engine commit</DropdownItem
			>
			<Tooltip class="text-xs w-[22rem]" placement="left"
				>Updates the EngineAssociation item in the .uproject to reflect the current engine commit.</Tooltip
			>
			<DropdownItem class="text-xs" on:click={handleSyncEngineRepoWithUproject}
				>Sync engine commit with UProject</DropdownItem
			>
			<Tooltip class="text-xs w-[22rem]" placement="left"
				>Updates the engine commit to the version currently set in the .uproject's EngineAssociation
				item.</Tooltip
			>
		{/if}
		<DropdownItem class="text-xs" on:click={handleReinstallGitHooksClicked}
			>Reinstall Git hooks</DropdownItem
		>
		<Tooltip class="text-xs w-[22rem]" placement="left"
			>For engineers. Helps iterate on the git hooks workflow.</Tooltip
		>
	</Dropdown>
</div>
<div class="flex flex-row h-full gap-2 overflow-auto">
	<div class="flex flex-col gap-2 w-full h-full overflow-x-auto">
		{#key $allModifiedFiles}
			<ModifiedFilesCard
				disabled={loading}
				bind:selectedFiles={$selectedFiles}
				bind:selectAll
				bind:changeSets={$changeSets}
				onChangesetsSaved={handleSaveChangesets}
				modifiedFiles={$allModifiedFiles}
				onOpenDirectory={handleOpenDirectory}
				onRevertFiles={handleRevertFiles}
				onSaveSnapshot={handleSaveSnapshot}
				onLockSelected={handleLockSelected}
				onRightClick={onModifiedFileRightClick}
			/>
		{/key}
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
	class="w-full p-4 mt-2 sm:p-4 max-w-full min-h-[20rem] h-[20rem] bg-secondary-700 dark:bg-space-900 border-0 shadow-none"
>
	<Tabs
		style="underline"
		divider={false}
		contentClass="bg-secondary-700 dark:bg-space-900 h-full overflow-y-auto"
	>
		<TabItem open title="My Commits ({pulls.length})" class="bg-secondary-700 dark:bg-space-900">
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
								{#each pull.commits.nodes as node}
									<span
										>{node.commit.message.length > 80
											? `${node.commit.message.substring(0, 80)}...`
											: node.commit.message}</span
									>
									<br />
								{/each}
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

<Modal
	open={promptRevertUProject}
	dismissable={false}
	class="bg-secondary-700 dark:bg-space-900"
	backdropClass="fixed mt-8 inset-0 z-40 bg-gray-900 bg-opacity-50 dark:bg-opacity-80"
	dialogClass="fixed mt-8 top-0 start-0 end-0 h-modal md:inset-0 md:h-full z-50 w-full p-4 pb-12 flex"
>
	<div class="flex items-center justify-between gap-2">
		<span
			>You have modifications to the uproject. It is STRONGLY recommended to revert this file to
			remain on a correct engine version.</span
		>
		<Button size="xs" color="green" on:click={handleRevertUproject}>Revert</Button>
		<Button size="xs" color="red" on:click={handleCloseRevertUproject}>Keep Changes</Button>
	</div>
</Modal>

<ProgressModal showModal={showProgressModal} title={progressModalTitle} />
