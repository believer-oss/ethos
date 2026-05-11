<script lang="ts">
	import {
		Badge,
		Button,
		ButtonGroup,
		Card,
		Checkbox,
		Dropdown,
		DropdownItem,
		Input,
		Label,
		Modal,
		Select,
		Spinner,
		Toggle,
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
		ChevronDownOutline,
		PlusOutline,
		PenSolid,
		CloseCircleSolid,
		FileCopySolid
	} from 'flowbite-svelte-icons';
	import { onDestroy, onMount } from 'svelte';
	import { emit } from '@tauri-apps/api/event';
	import { open } from '@tauri-apps/plugin-shell';
	import { open as openDialog, save as saveDialog } from '@tauri-apps/plugin-dialog';
	import { sendNotification } from '@tauri-apps/plugin-notification';
	import {
		type ChangeSet,
		type CommitFileInfo,
		type ModifiedFile,
		ModifiedFileState,
		ModifiedFilesCard,
		ProgressModal,
		SubmitStatus
	} from '@ethos/core';
	import { get } from 'svelte/store';
	import { Menu, MenuItem } from '@tauri-apps/api/menu';
	import {
		type GitHubPullRequest,
		type Nullable,
		type PushRequest,
		type RepoStatus,
		type RevertFilesRequest,
		type Snapshot,
		type SnapshotPreviewEntry,
		type SnapshotPreviewResponse,
		type ZipPreviewEntry,
		type ZipPreviewResponse
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
		importZippedChanges,
		listSnapshots,
		openProject,
		openSln,
		previewImportZip,
		previewSnapshot,
		quickSubmit,
		reinstallGitHooks,
		restoreSnapshot,
		revertFiles,
		saveChangeSet,
		saveSnapshot,
		showCommitFiles,
		syncEngineCommitWithUproject,
		syncLatest,
		syncUprojectWithEngineCommit,
		zipLocalChanges
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
	import { updateAppConfig } from '$lib/config';
	import { openUrl } from '$lib/utils';
	import UnrealEngineLogoNoCircle from '$lib/icons/UnrealEngineLogoNoCircle.svelte';
	import { checkEngineReady, openUrlForPath } from '$lib/engine';
	import FileHistoryModal from '$lib/components/FileHistoryModal.svelte';

	let fileHistoryModalOpen = false;
	let fileHistoryPath: string | null = null;
	const showFileHistory = (path: string) => {
		fileHistoryPath = path;
		fileHistoryModalOpen = true;
	};

	let loading = false;
	let fetchingPulls = false;
	let quickSubmitting = false;
	let syncing = false;
	let promptForPAT = false;
	let promptRevertUProject = false;
	let dismissedUProjectPrompt = false;
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

	// quick submit preview
	let showQuickSubmitPreview = false;

	// zip local changes
	let showZipPreview = false;
	let zipping = false;

	// import zipped changes
	let showImportPreview = false;
	let importPreview: ZipPreviewResponse | null = null;
	let importing = false;
	let importSelective = false;
	let importSelectedPaths: Set<string> = new Set();

	// restore snapshot preview
	let showRestorePreview = false;
	let restorePreview: SnapshotPreviewResponse | null = null;
	let restorePreviewCommit: string | null = null;
	let restoring = false;
	let restoreSelective = false;
	let restoreSelectedPaths: Set<string> = new Set();
	// Reset to false every time the modal opens so users can't accidentally
	// stomp local changes by leaving the toggle on from a previous restore.
	let restoreOverwriteLocal = false;

	// progress modal
	let showProgressModal = false;
	let progressModalTitle = '';

	let selectAll = false;
	let pulls: GitHubPullRequest[] = [];

	let loadingSnapshots = false;
	let snapshots: Snapshot[] = [];

	$: conflictsDetected = ($repoStatus?.conflicts.length ?? 0) > 0;
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
		const allFiles = [
			...(inRepoStatus?.modifiedFiles ?? []),
			...(inRepoStatus?.untrackedFiles ?? [])
		];
		$selectedFiles = $selectedFiles
			.map((file) => allFiles.find((f) => f.path === file.path))
			.filter((file): file is ModifiedFile => file !== undefined);

		// If a user is pulling their own game DLLs, they likely are not an engineer and should not be
		// making changes to the .uproject.
		if ($appConfig.pullDlls && $appConfig.engineType === 'Prebuilt') {
			const files = inRepoStatus?.modifiedFiles ?? [];
			const uprojectModified = files.some((f) => f.path === $repoConfig?.uprojectPath);

			if (uprojectModified && !dismissedUProjectPrompt) {
				promptRevertUProject = true;
			} else if (!uprojectModified) {
				dismissedUProjectPrompt = false;
			}
		}
	});

	$: formattedCommitMessage =
		typeof $commitMessage === 'string'
			? $commitMessage
			: `${$commitMessage.type}(${$commitMessage.scope}): ${$commitMessage.message}`;
	$: hasUnsubmittableFiles = $selectedFiles.some((file) => file.submitStatus !== SubmitStatus.Ok);
	$: canSubmit =
		$selectedFiles.length > 0 &&
		get(commitMessage) !== '' &&
		commitMessageValid &&
		!hasUnsubmittableFiles;

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

	const handleFileReverted = async () => {
		fileHistoryModalOpen = false;
		await refreshFiles(false);
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

	const handleStartRestoreSnapshot = async (commit: string) => {
		try {
			const preview = await previewSnapshot(commit);
			restorePreview = preview;
			restorePreviewCommit = commit;
			restoreSelective = false;
			restoreSelectedPaths = new Set(preview.entries.map((e) => e.path));
			restoreOverwriteLocal = false;
			showRestorePreview = true;
		} catch (e) {
			await emit('error', e);
		}
	};

	const toggleRestoreEntry = (path: string) => {
		if (restoreSelectedPaths.has(path)) {
			restoreSelectedPaths.delete(path);
		} else {
			restoreSelectedPaths.add(path);
		}
		restoreSelectedPaths = new Set(restoreSelectedPaths);
	};

	const setAllRestoreEntries = (checked: boolean) => {
		if (!restorePreview) return;
		restoreSelectedPaths = checked ? new Set(restorePreview.entries.map((e) => e.path)) : new Set();
	};

	const handleConfirmRestoreSnapshot = async () => {
		if (!restorePreview || !restorePreviewCommit) return;

		// Always pass the explicit path list so the backend goes through the
		// preview-driven selective restore path (which honors `overwriteLocal`)
		// instead of the legacy cherry-pick code path, whose conflict guard
		// has the opposite sense and would reject the request whenever the
		// user checked the overwrite box.
		const subset = restoreSelective
			? Array.from(restoreSelectedPaths)
			: restorePreview.entries.map((e) => e.path);

		restoring = true;
		loadingSnapshots = true;
		showProgressModal = true;
		syncing = true;
		progressModalTitle = 'Restoring snapshot';

		try {
			await restoreSnapshot(restorePreviewCommit, subset, restoreOverwriteLocal);

			$selectedFiles = [];
			selectAll = false;
			showRestorePreview = false;
			restorePreview = null;
			restorePreviewCommit = null;
			restoreSelective = false;
			restoreSelectedPaths = new Set();
			restoreOverwriteLocal = false;

			await refreshFiles(true);
			await emit('success', 'Snapshot restored!');
		} catch (e) {
			await emit('error', e);
		}

		loadingSnapshots = false;
		showProgressModal = false;
		syncing = false;
		restoring = false;
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
		try {
			const engineReady = await checkEngineReady();
			if (!engineReady) {
				await emit('error', 'Unreal Editor must be closed before reverting files.');
				return;
			}
		} catch (e) {
			// eslint-disable-next-line no-console
			console.warn('Engine ready check failed, proceeding with revert:', e);
		}

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
		progressModalTitle = 'Submitting changes';

		const message = get(commitMessage);

		const req: PushRequest = {
			commitMessage:
				typeof message === 'string'
					? message
					: `${message.type}(${message.scope}): ${message.message}`,
			files: $selectedFiles.map((file) => file.path)
		};

		let quickSubmitSucceeded = false;

		try {
			await quickSubmit(req);

			// Save last used type and scope to config for next session
			if ($repoConfig?.useConventionalCommits) {
				const updatedConfig = {
					...$appConfig,
					lastQuickSubmitType: tempCommitType,
					lastQuickSubmitScope: tempCommitScope
				};
				await updateAppConfig(updatedConfig).catch((e) => {
					// eslint-disable-next-line no-console
					console.warn('Failed to persist quick submit preferences:', e);
				});
				$appConfig = updatedConfig;
			}

			$commitMessage = '';

			// note that we don't reset tempCommitType or tempCommitScope because the UI already has values selected
			tempCommitMessage = '';

			$selectedFiles = [];
			selectAll = false;

			await refreshPulls();

			quickSubmitSucceeded = true;
		} catch (e) {
			await emit('error', e);
		}

		// refresh files after quick submit, whether it was successful or not
		progressModalTitle = 'Refreshing files';
		await refreshFiles(true);

		if (quickSubmitSucceeded) {
			await emit('success', 'Changes submitted successfully!');
		}

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

			const result = await syncLatest();

			if (result.alreadyUpToDate) {
				await emit('success', 'Already up to date!');
			} else {
				await refreshFiles(true);

				if (!$appConfig.pullDlls) {
					progressModalTitle = 'Generating projects';
					await generateSln();
				} else if ($appConfig.openUprojectAfterSync) {
					progressModalTitle = 'Launching Unreal Engine';
					await openProject();
				}

				await emit('success', 'Sync complete!');
			}
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

	const handleZipLocalChanges = async () => {
		if ($selectedFiles.length === 0) return;

		const defaultName = `friendshipper-changes-${new Date().toISOString().slice(0, 10)}.zip`;
		const dest = await saveDialog({
			defaultPath: defaultName,
			filters: [{ name: 'Zip Archive', extensions: ['zip'] }]
		});

		if (!dest) return;

		zipping = true;
		showProgressModal = true;
		progressModalTitle = 'Zipping local changes';

		try {
			const result = await zipLocalChanges(
				$selectedFiles.map((file) => file.path),
				dest
			);
			showZipPreview = false;
			await emit(
				'success',
				`Wrote ${result.fileCount} file${result.fileCount === 1 ? '' : 's'} to ${
					result.destination
				}`
			);
		} catch (e) {
			await emit('error', e);
		}

		showProgressModal = false;
		zipping = false;
	};

	const handleStartImport = async () => {
		const source = await openDialog({
			multiple: false,
			directory: false,
			filters: [{ name: 'Zip Archive', extensions: ['zip'] }]
		});

		if (!source || Array.isArray(source)) return;

		try {
			importPreview = await previewImportZip(source);
			importSelective = false;
			importSelectedPaths = new Set(importPreview.entries.map((e) => e.path));
			showImportPreview = true;
		} catch (e) {
			await emit('error', e);
		}
	};

	const toggleImportEntry = (path: string) => {
		if (importSelectedPaths.has(path)) {
			importSelectedPaths.delete(path);
		} else {
			importSelectedPaths.add(path);
		}
		importSelectedPaths = new Set(importSelectedPaths);
	};

	const setAllImportEntries = (checked: boolean) => {
		if (!importPreview) return;
		importSelectedPaths = checked ? new Set(importPreview.entries.map((e) => e.path)) : new Set();
	};

	const handleConfirmImport = async () => {
		if (!importPreview) return;

		const allSelected = importSelectedPaths.size === importPreview.entries.length;
		const subset = importSelective && !allSelected ? Array.from(importSelectedPaths) : null;

		// Anything we're about to extract that has uncommitted local changes will
		// be clobbered (overwrites or deletions). Snapshot those paths first so
		// the user has a way back.
		const entriesToImport = importPreview.entries.filter(
			(entry) => subset === null || importSelectedPaths.has(entry.path)
		);
		const pathsAtRisk = entriesToImport
			.filter((entry) => entry.conflictsWithLocal)
			.map((entry) => entry.path);

		importing = true;
		showProgressModal = true;

		try {
			if (pathsAtRisk.length > 0) {
				progressModalTitle = 'Snapshotting local changes before import';
				const zipName = importPreview.source.split(/[\\/]/).pop() || importPreview.source;
				await saveSnapshot(`Auto-snapshot before importing ${zipName}`, pathsAtRisk);
				await refreshSnapshots();
			}

			progressModalTitle = 'Importing zipped changes';
			const result = await importZippedChanges(importPreview.source, subset);
			showImportPreview = false;
			importPreview = null;
			importSelective = false;
			importSelectedPaths = new Set();

			await refreshFiles(true);

			const parts: string[] = [];
			if (result.extracted > 0) {
				parts.push(`extracted ${result.extracted}`);
			}
			if (result.deleted > 0) {
				parts.push(`deleted ${result.deleted}`);
			}
			if (pathsAtRisk.length > 0) {
				parts.push(
					`snapshotted ${pathsAtRisk.length} local change${pathsAtRisk.length === 1 ? '' : 's'}`
				);
			}
			const summary = parts.length > 0 ? ` (${parts.join(', ')})` : '';
			await emit('success', `Imported zipped changes${summary}`);
		} catch (e) {
			await emit('error', e);
		}

		showProgressModal = false;
		importing = false;
	};

	const getImportEntryTextClass = (entry: ZipPreviewEntry): string => {
		if (entry.conflictsWithLocal) {
			return 'text-red-500 dark:text-red-500';
		}
		if (entry.state === 'Added') {
			return 'text-lime-500 dark:text-lime-500';
		}
		if (entry.state === 'Deleted') {
			return 'text-yellow-300 dark:text-gray-300';
		}
		if (entry.state === 'Modified') {
			return 'text-yellow-300 dark:text-yellow-300';
		}
		return 'text-white';
	};

	$: importConflictCount = importPreview?.entries.filter((e) => e.conflictsWithLocal).length ?? 0;
	$: importSelectedConflictCount =
		importPreview?.entries.filter((e) => e.conflictsWithLocal && importSelectedPaths.has(e.path))
			.length ?? 0;
	$: importTotal = importPreview?.entries.length ?? 0;
	// Surface the riskiest entries first: conflicts with local changes, then plain
	// disk overwrites, then everything else. Array#sort with a stable key keeps
	// each tier in the manifest's original order.
	$: importSortedEntries = importPreview
		? [...importPreview.entries].sort((a, b) => {
				const rank = (e: ZipPreviewEntry) => {
					if (e.conflictsWithLocal) return 0;
					if (e.existsOnDisk && e.state !== 'Deleted') return 1;
					return 2;
				};
				return rank(a) - rank(b);
		  })
		: [];
	$: importAllSelected = importTotal > 0 && importSelectedPaths.size === importTotal;
	$: importSomeSelected = importSelectedPaths.size > 0 && importSelectedPaths.size < importTotal;

	const getRestoreEntryTextClass = (entry: SnapshotPreviewEntry): string => {
		if (entry.conflictsWithLocal) {
			return 'text-red-500 dark:text-red-500';
		}
		if (entry.state === 'Added') {
			return 'text-lime-500 dark:text-lime-500';
		}
		if (entry.state === 'Deleted') {
			return 'text-yellow-300 dark:text-gray-300';
		}
		if (entry.state === 'Modified') {
			return 'text-yellow-300 dark:text-yellow-300';
		}
		return 'text-white';
	};

	$: restoreConflictCount = restorePreview?.entries.filter((e) => e.conflictsWithLocal).length ?? 0;
	$: restoreSelectedConflictCount =
		restorePreview?.entries.filter((e) => e.conflictsWithLocal && restoreSelectedPaths.has(e.path))
			.length ?? 0;
	$: restoreTotal = restorePreview?.entries.length ?? 0;
	$: restoreSortedEntries = restorePreview
		? [...restorePreview.entries].sort((a, b) => {
				const rank = (e: SnapshotPreviewEntry) => {
					if (e.conflictsWithLocal) return 0;
					if (e.existsOnDisk && e.state !== 'Deleted') return 1;
					return 2;
				};
				return rank(a) - rank(b);
		  })
		: [];
	$: restoreAllSelected = restoreTotal > 0 && restoreSelectedPaths.size === restoreTotal;
	$: restoreSomeSelected =
		restoreSelectedPaths.size > 0 && restoreSelectedPaths.size < restoreTotal;

	const handleOpenPreferences = async () => {
		promptForPAT = false;
		preferencesOpen = true;
		await emit('open-preferences');
	};

	const handleRevertUproject = async () => {
		promptRevertUProject = false;

		try {
			const engineReady = await checkEngineReady();
			if (!engineReady) {
				await emit('error', 'Unreal Editor must be closed before reverting files.');
				return;
			}
		} catch (e) {
			// eslint-disable-next-line no-console
			console.warn('Engine ready check failed, proceeding with revert:', e);
		}

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
		dismissedUProjectPrompt = true;
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

	const getFileTextClass = (file: ModifiedFile): string => {
		if (file.submitStatus !== SubmitStatus.Ok) {
			return 'text-red-700 dark:text-red-700';
		}

		if (file.state === ModifiedFileState.Added) {
			return 'text-lime-500 dark:text-lime-500';
		}
		if (file.state === ModifiedFileState.Modified) {
			return 'text-yellow-300 dark:text-yellow-300';
		}
		if (file.state === ModifiedFileState.Deleted) {
			return 'text-yellow-300 dark:text-gray-300';
		}
		if (file.state === ModifiedFileState.Unmerged) {
			return 'text-red-700 dark:text-red-700';
		}

		return '';
	};

	const getFileDisplayString = (file: ModifiedFile): string => {
		if (file.displayName === '') {
			return file.path;
		}
		return file.displayName;
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
			// Restore last quick submit type/scope from config if no type is set yet
			if ($repoConfig?.useConventionalCommits) {
				tempCommitType = $appConfig.lastQuickSubmitType ?? '';
				tempCommitScope = $appConfig.lastQuickSubmitScope ?? '';
			}
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
<div class="flex flex-row flex-1 min-h-[20rem] gap-2 overflow-auto">
	<div class="flex flex-col gap-2 w-full h-full overflow-x-auto">
		<ModifiedFilesCard
			disabled={loading}
			bind:selectedFiles={$selectedFiles}
			bind:selectAll
			changeSets={$changeSets}
			onChangesetsSaved={handleSaveChangesets}
			modifiedFiles={$allModifiedFiles}
			onOpenDirectory={handleOpenDirectory}
			onRevertFiles={handleRevertFiles}
			onSaveSnapshot={handleSaveSnapshot}
			onLockSelected={handleLockSelected}
			onRightClick={onModifiedFileRightClick}
			onShowFileHistory={showFileHistory}
		>
			<Button
				slot="actions-dropdown-trigger"
				size="xs"
				color="primary"
				id="modified-files-actions-dropdown"
				disabled={loading || zipping || importing}
			>
				<ChevronDownOutline size="xs" />
			</Button>
			<Dropdown
				slot="actions-dropdown"
				placement="bottom-end"
				triggeredBy="#modified-files-actions-dropdown"
			>
				<DropdownItem
					class="text-xs"
					disabled={$selectedFiles.length === 0 || zipping}
					on:click={() => {
						if ($selectedFiles.length === 0) return;
						showZipPreview = true;
					}}
				>
					Zip Local Changes{$selectedFiles.length > 0 ? ` (${$selectedFiles.length})` : ''}
				</DropdownItem>
				<Tooltip class="text-xs w-[22rem]" placement="left"
					>Bundle the selected files into a zip preserving their repo paths so a teammate can drop
					them onto their working tree.</Tooltip
				>
				<DropdownItem class="text-xs" disabled={importing} on:click={handleStartImport}
					>Import Zipped Changes</DropdownItem
				>
				<Tooltip class="text-xs w-[22rem]" placement="left"
					>Pick a zip created by Zip Local Changes and extract it on top of your repo. You'll see a
					preview before anything is written.</Tooltip
				>
			</Dropdown>
		</ModifiedFilesCard>
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
							on:click={() => {
								showQuickSubmitPreview = true;
							}}
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
	class="w-full p-4 mt-2 sm:p-4 max-w-full min-h-0 h-[20rem] bg-secondary-700 dark:bg-space-900 border-0 shadow-none"
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
											await handleStartRestoreSnapshot(snapshot.commit);
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
										<p class="text-white flex items-center gap-1">
											Commit Files
											{#if !loadingCommitFiles}
												<span class="text-xs text-gray-400 font-italic">({commitFiles.length})</span
												>
											{/if}
										</p>
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
		{#if hasUnsubmittableFiles}
			<span class="text-red-400"
				>Some selected files require checkout (lock) before submitting. Deselect them or lock them
				first.</span
			><br /><br />
		{/if}
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

<Modal
	open={showQuickSubmitPreview}
	dismissable={true}
	on:close={() => {
		showQuickSubmitPreview = false;
	}}
	class="bg-secondary-700 dark:bg-space-900"
	backdropClass="fixed mt-8 inset-0 z-40 bg-gray-900 bg-opacity-50 dark:bg-opacity-80"
	dialogClass="fixed mt-8 top-0 start-0 end-0 h-modal md:inset-0 md:h-full z-50 w-full p-4 pb-12 flex"
	size="md"
>
	<div class="flex flex-col gap-3">
		<h3 class="text-lg font-semibold text-white">Quick Submit Preview</h3>
		<p class="text-sm text-gray-300 break-words">{formattedCommitMessage}</p>
		<p class="text-sm text-gray-400">
			{$selectedFiles.length} file{$selectedFiles.length === 1 ? '' : 's'}
		</p>
		<div
			class="bg-secondary-800 dark:bg-space-950 p-2 max-h-64 overflow-y-auto rounded text-nowrap"
		>
			{#each $selectedFiles as file}
				<div class="flex gap-2 items-center" role="listitem">
					{#if file.state === ModifiedFileState.Added}
						<PlusOutline class="w-4 h-4 text-lime-500 shrink-0" />
					{:else if file.state === ModifiedFileState.Modified}
						<PenSolid class="w-4 h-4 text-yellow-300 shrink-0" />
					{:else if file.state === ModifiedFileState.Deleted}
						<CloseCircleSolid class="w-4 h-4 text-red-700 shrink-0" />
					{:else if file.state === ModifiedFileState.Unmerged}
						<FileCopySolid class="w-4 h-4 text-red-700 shrink-0" />
					{/if}
					<span class="truncate {getFileTextClass(file)}" title={file.path}
						>{getFileDisplayString(file)}</span
					>
				</div>
			{/each}
		</div>
		<div class="flex justify-end gap-2">
			<Button
				size="sm"
				color="alternative"
				on:click={() => {
					showQuickSubmitPreview = false;
				}}>Cancel</Button
			>
			<Button
				size="sm"
				color="primary"
				on:click={async () => {
					showQuickSubmitPreview = false;
					await handleQuickSubmit();
				}}>Confirm Submit</Button
			>
		</div>
	</div>
</Modal>

<Modal
	open={showZipPreview}
	dismissable={true}
	on:close={() => {
		showZipPreview = false;
	}}
	class="bg-secondary-700 dark:bg-space-900"
	backdropClass="fixed mt-8 inset-0 z-40 bg-gray-900 bg-opacity-50 dark:bg-opacity-80"
	dialogClass="fixed mt-8 top-0 start-0 end-0 h-modal md:inset-0 md:h-full z-50 w-full p-4 pb-12 flex"
	size="md"
>
	<div class="flex flex-col gap-3">
		<h3 class="text-lg font-semibold text-white">Zip Local Changes Preview</h3>
		<p class="text-sm text-gray-400">
			{$selectedFiles.length} file{$selectedFiles.length === 1 ? '' : 's'} will be bundled. Hover an
			OFPA file to see its full repo path.
		</p>
		<div
			class="bg-secondary-800 dark:bg-space-950 p-2 max-h-64 overflow-y-auto rounded text-nowrap"
		>
			{#each $selectedFiles as file}
				<div class="flex gap-2 items-center" role="listitem">
					{#if file.state === ModifiedFileState.Added}
						<PlusOutline class="w-4 h-4 text-lime-500 shrink-0" />
					{:else if file.state === ModifiedFileState.Modified}
						<PenSolid class="w-4 h-4 text-yellow-300 shrink-0" />
					{:else if file.state === ModifiedFileState.Deleted}
						<CloseCircleSolid class="w-4 h-4 text-red-700 shrink-0" />
					{:else if file.state === ModifiedFileState.Unmerged}
						<FileCopySolid class="w-4 h-4 text-red-700 shrink-0" />
					{/if}
					<span class="truncate {getFileTextClass(file)}" title={file.path}
						>{getFileDisplayString(file)}</span
					>
				</div>
			{/each}
		</div>
		<div class="flex justify-end gap-2">
			<Button
				size="sm"
				color="alternative"
				disabled={zipping}
				on:click={() => {
					showZipPreview = false;
				}}>Cancel</Button
			>
			<Button
				size="sm"
				color="primary"
				disabled={zipping || $selectedFiles.length === 0}
				on:click={handleZipLocalChanges}>Choose destination & zip</Button
			>
		</div>
	</div>
</Modal>

<Modal
	open={showImportPreview}
	dismissable={true}
	on:close={() => {
		showImportPreview = false;
		importPreview = null;
	}}
	class="bg-secondary-700 dark:bg-space-900"
	backdropClass="fixed mt-8 inset-0 z-40 bg-gray-900 bg-opacity-50 dark:bg-opacity-80"
	dialogClass="fixed mt-8 top-0 start-0 end-0 h-modal md:inset-0 md:h-full z-50 w-full p-4 pb-12 flex"
	size="lg"
>
	<div class="flex flex-col gap-3">
		<h3 class="text-lg font-semibold text-white">Import Zipped Changes</h3>
		{#if importPreview}
			<p class="text-xs text-gray-400 break-all">Source: {importPreview.source}</p>
			{#if importPreview.createdBy || importPreview.createdAt}
				<p class="text-xs text-gray-400">
					Created
					{#if importPreview.createdBy}by <span class="text-primary-400"
							>{importPreview.createdBy}</span
						>{/if}
					{#if importPreview.createdAt}
						on {new Date(importPreview.createdAt).toLocaleString()}
					{/if}
				</p>
			{/if}
			<p class="text-sm text-gray-300">
				{importSelective ? importSelectedPaths.size : importTotal} of {importTotal} file{importTotal ===
				1
					? ''
					: 's'} will be written to your repo.
				{#if importSelective ? importSelectedConflictCount > 0 : importConflictCount > 0}
					<span class="text-red-400 font-semibold"
						>{importSelective ? importSelectedConflictCount : importConflictCount} will overwrite uncommitted
						local change{(importSelective ? importSelectedConflictCount : importConflictCount) === 1
							? ''
							: 's'}.</span
					>
				{/if}
			</p>
			<div class="flex flex-row items-center justify-between gap-2 px-1">
				<Toggle bind:checked={importSelective} class="text-sm text-gray-300"
					>Selectively choose files</Toggle
				>
				{#if importSelective}
					<div class="flex items-center gap-2 text-sm text-gray-300">
						<Checkbox
							class="!p-1.5"
							checked={importAllSelected}
							indeterminate={importSomeSelected}
							on:change={() => {
								setAllImportEntries(!importAllSelected);
							}}>{importAllSelected ? 'Deselect all' : 'Select all'}</Checkbox
						>
					</div>
				{/if}
			</div>
			<div
				class="bg-secondary-800 dark:bg-space-950 p-2 max-h-72 overflow-y-auto rounded text-nowrap"
			>
				{#each importSortedEntries as entry}
					{@const checked = importSelectedPaths.has(entry.path)}
					{@const dimmed = importSelective && !checked}
					<div class="flex gap-2 items-center" class:opacity-40={dimmed} role="listitem">
						{#if importSelective}
							<Checkbox
								class="!p-1.5 shrink-0"
								{checked}
								on:change={() => {
									toggleImportEntry(entry.path);
								}}
							/>
						{/if}
						{#if entry.state === 'Added'}
							<PlusOutline class="w-4 h-4 text-lime-500 shrink-0" />
						{:else if entry.state === 'Modified'}
							<PenSolid class="w-4 h-4 text-yellow-300 shrink-0" />
						{:else if entry.state === 'Deleted'}
							<CloseCircleSolid class="w-4 h-4 text-red-700 shrink-0" />
						{:else if entry.state === 'Unmerged'}
							<FileCopySolid class="w-4 h-4 text-red-700 shrink-0" />
						{:else}
							<FileCodeSolid class="w-4 h-4 text-gray-400 shrink-0" />
						{/if}
						<span
							class="truncate {getImportEntryTextClass(entry)}"
							title={entry.displayName && entry.displayName !== entry.path
								? `${entry.displayName} — ${entry.path}`
								: entry.path}
						>
							{entry.displayName !== '' ? entry.displayName : entry.path}
						</span>
						{#if entry.conflictsWithLocal}
							<span class="text-xs text-red-400 shrink-0">[overwrites local change]</span>
						{:else if entry.existsOnDisk && entry.state !== 'Deleted'}
							<span class="text-xs text-yellow-400 shrink-0">[overwrites file]</span>
						{/if}
					</div>
				{/each}
			</div>
			{#if (importSelective ? importSelectedConflictCount : importConflictCount) > 0}
				<p class="text-xs text-yellow-300">
					A snapshot of the affected local changes will be saved automatically before the import so
					you can restore them.
				</p>
			{/if}
		{:else}
			<Spinner class="w-4 h-4" />
		{/if}
		<div class="flex justify-end gap-2">
			<Button
				size="sm"
				color="alternative"
				disabled={importing}
				on:click={() => {
					showImportPreview = false;
					importPreview = null;
				}}>Cancel</Button
			>
			<Button
				size="sm"
				color="primary"
				disabled={importing ||
					importPreview === null ||
					importPreview.entries.length === 0 ||
					(importSelective && importSelectedPaths.size === 0)}
				on:click={handleConfirmImport}
				>Import{importSelective ? ` (${importSelectedPaths.size})` : ''}</Button
			>
		</div>
	</div>
</Modal>

<Modal
	open={showRestorePreview}
	dismissable={true}
	on:close={() => {
		showRestorePreview = false;
		restorePreview = null;
		restorePreviewCommit = null;
	}}
	class="bg-secondary-700 dark:bg-space-900"
	backdropClass="fixed mt-8 inset-0 z-40 bg-gray-900 bg-opacity-50 dark:bg-opacity-80"
	dialogClass="fixed mt-8 top-0 start-0 end-0 h-modal md:inset-0 md:h-full z-50 w-full p-4 pb-12 flex"
	size="lg"
>
	<div class="flex flex-col gap-3">
		<h3 class="text-lg font-semibold text-white">Restore Snapshot</h3>
		{#if restorePreview}
			<p class="text-sm text-gray-300">
				{restoreSelective ? restoreSelectedPaths.size : restoreTotal} of {restoreTotal} file{restoreTotal ===
				1
					? ''
					: 's'} will be written to your repo.
				{#if restoreSelective ? restoreSelectedConflictCount > 0 : restoreConflictCount > 0}
					<span class="text-red-400 font-semibold"
						>{restoreSelective ? restoreSelectedConflictCount : restoreConflictCount} will overwrite
						uncommitted local change{(restoreSelective
							? restoreSelectedConflictCount
							: restoreConflictCount) === 1
							? ''
							: 's'}.</span
					>
				{/if}
			</p>
			<div class="flex flex-row items-center justify-between gap-2 px-1">
				<Toggle bind:checked={restoreSelective} class="text-sm text-gray-300"
					>Selectively choose files</Toggle
				>
				{#if restoreSelective}
					<div class="flex items-center gap-2 text-sm text-gray-300">
						<Checkbox
							class="!p-1.5"
							checked={restoreAllSelected}
							indeterminate={restoreSomeSelected}
							on:change={() => {
								setAllRestoreEntries(!restoreAllSelected);
							}}>{restoreAllSelected ? 'Deselect all' : 'Select all'}</Checkbox
						>
					</div>
				{/if}
			</div>
			<div
				class="bg-secondary-800 dark:bg-space-950 p-2 max-h-72 overflow-y-auto rounded text-nowrap"
			>
				{#each restoreSortedEntries as entry}
					{@const checked = restoreSelectedPaths.has(entry.path)}
					{@const dimmed = restoreSelective && !checked}
					<div class="flex gap-2 items-center" class:opacity-40={dimmed} role="listitem">
						{#if restoreSelective}
							<Checkbox
								class="!p-1.5 shrink-0"
								{checked}
								on:change={() => {
									toggleRestoreEntry(entry.path);
								}}
							/>
						{/if}
						{#if entry.state === 'Added'}
							<PlusOutline class="w-4 h-4 text-lime-500 shrink-0" />
						{:else if entry.state === 'Modified'}
							<PenSolid class="w-4 h-4 text-yellow-300 shrink-0" />
						{:else if entry.state === 'Deleted'}
							<CloseCircleSolid class="w-4 h-4 text-red-700 shrink-0" />
						{:else if entry.state === 'Unmerged'}
							<FileCopySolid class="w-4 h-4 text-red-700 shrink-0" />
						{:else}
							<FileCodeSolid class="w-4 h-4 text-gray-400 shrink-0" />
						{/if}
						<span class="truncate {getRestoreEntryTextClass(entry)}" title={entry.path}>
							{entry.path}
						</span>
						{#if entry.conflictsWithLocal}
							<span class="text-xs text-red-400 shrink-0">[overwrites local change]</span>
						{:else if entry.existsOnDisk && entry.state !== 'Deleted'}
							<span class="text-xs text-yellow-400 shrink-0">[overwrites file]</span>
						{/if}
					</div>
				{/each}
			</div>
			{#if (restoreSelective ? restoreSelectedConflictCount : restoreConflictCount) > 0}
				<div class="flex flex-col gap-1 px-1">
					<Checkbox bind:checked={restoreOverwriteLocal} class="text-sm text-gray-300">
						Overwrite local changes
					</Checkbox>
					<p class="text-xs text-red-400">
						{restoreSelective ? restoreSelectedConflictCount : restoreConflictCount} selected file{(restoreSelective
							? restoreSelectedConflictCount
							: restoreConflictCount) === 1
							? ''
							: 's'} would overwrite uncommitted local change{(restoreSelective
							? restoreSelectedConflictCount
							: restoreConflictCount) === 1
							? ''
							: 's'}. Check the box to proceed anyway, deselect the file{(restoreSelective
							? restoreSelectedConflictCount
							: restoreConflictCount) === 1
							? ''
							: 's'}, or take a snapshot of your current state first.
					</p>
				</div>
			{/if}
		{:else}
			<Spinner class="w-4 h-4" />
		{/if}
		<div class="flex justify-end gap-2">
			<Button
				size="sm"
				color="alternative"
				disabled={restoring}
				on:click={() => {
					showRestorePreview = false;
					restorePreview = null;
					restorePreviewCommit = null;
				}}>Cancel</Button
			>
			<Button
				size="sm"
				color="primary"
				disabled={restoring ||
					restorePreview === null ||
					restorePreview.entries.length === 0 ||
					(restoreSelective && restoreSelectedPaths.size === 0) ||
					((restoreSelective ? restoreSelectedConflictCount : restoreConflictCount) > 0 &&
						!restoreOverwriteLocal)}
				on:click={handleConfirmRestoreSnapshot}
				>Restore{restoreSelective ? ` (${restoreSelectedPaths.size})` : ''}</Button
			>
		</div>
	</div>
</Modal>

<ProgressModal showModal={showProgressModal} title={progressModalTitle} />

<FileHistoryModal
	bind:open={fileHistoryModalOpen}
	filePath={fileHistoryPath}
	onReverted={handleFileReverted}
/>
