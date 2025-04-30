<script lang="ts">
	import {
		Breadcrumb,
		BreadcrumbItem,
		Button,
		ButtonGroup,
		Card,
		Checkbox,
		Input,
		Label,
		Modal,
		Select,
		Spinner,
		Table,
		TableBody,
		TableBodyCell,
		TableBodyRow,
		Textarea,
		Toggle,
		Tooltip
	} from 'flowbite-svelte';
	import { onMount, onDestroy, tick } from 'svelte';
	import {
		CheckSolid,
		CloseSolid,
		EditOutline,
		FileSearchOutline,
		FolderSolid,
		HeartOutline,
		HeartSolid,
		LockOpenOutline,
		LockSolid,
		RotateOutline,
		SearchOutline
	} from 'flowbite-svelte-icons';
	import { emit, listen } from '@tauri-apps/api/event';
	import {
		type ChangeSet,
		type Commit,
		CommitTable,
		ModifiedFilesCard,
		ProgressModal
	} from '@ethos/core';
	import { get } from 'svelte/store';
	import {} from '@tauri-apps/api';
	import * as fs from '@tauri-apps/plugin-fs';
	import {
		cloneRepo,
		delFetchInclude,
		downloadLFSFiles,
		getAllFiles,
		getFetchInclude,
		getFileHistory,
		getFiles,
		getRepoStatus,
		lockFiles,
		revertFiles,
		showCommitFiles,
		submit,
		syncLatest,
		unlockFiles,
		verifyLocks
	} from '$lib/repo';
	import {
		type DirectoryMetadata,
		FileType,
		type LFSFile,
		LocalFileLFSState,
		type Node,
		type Nullable,
		type PushRequest,
		type RevertFilesRequest
	} from '$lib/types';
	import {
		appConfig,
		currentRoot,
		currentRootFiles,
		enableGlobalSearch,
		fetchIncludeList,
		locks,
		rootNode,
		selectedFile,
		selectedTreeFiles,
		selectedExplorerFiles,
		selectedFiles,
		repoStatus,
		changeSets,
		allModifiedFiles,
		commitMessage,
		selectedDirectoryClass
	} from '$lib/stores';
	import { openUrl } from '$lib/utils';
	import CharacterCard from '$lib/components/metadata/CharacterCard.svelte';
	import {
		getDirectoryMetadata,
		updateDirectoryMetadata,
		updateMetadataClass
	} from '$lib/metadata';
	import { getAppConfig } from '$lib/config';
	import { runSetEnv, syncTools } from '$lib/tools';
	import FileTree from '$lib/components/files/FileTree.svelte';
	import { CHANGE_SETS_PATH, CURRENT_ROOT_PATH, FILE_TREE_PATH } from '$lib/consts';

	let loading = false;
	let allFiles: string[] = [];
	let downloadInProgress: boolean = false;
	let search: string = '';
	let showSearchModal: boolean = false;
	let searchInput: HTMLInputElement;
	let modalLoading: boolean = false;
	let shiftHeld = false;
	let ctrlHeld = false;
	let includeWip = true;
	let selectAll = false;
	let showSourceControl = true;

	// sync and tools
	let inAsyncOperation = false;
	let asyncModalText = '';

	$: filteredFiles = allFiles.filter(
		(file) =>
			search.split(' ').every((s) => file.toLowerCase().includes(s.toLowerCase())) &&
			search.length > 2
	);

	$: ancestry = $currentRoot.split('/').filter((a) => a !== '');

	$: canSubmit = $selectedFiles.length > 0 && get(commitMessage) !== '' && !loading;

	// directory metadata
	let directoryMetadata: Nullable<DirectoryMetadata> = null;
	let editingDirectoryClass: boolean = false;
	const defaultDirectoryClass: string = 'none';
	let tempDirectoryClass: string = defaultDirectoryClass;
	let updatingDirectoryClass: boolean = false;
	const directoryClassOptions = [
		{ name: 'none', value: 'none' },
		{
			name: 'character',
			value: 'character'
		}
	];

	// sync and tools
	const handleSyncClicked = async () => {
		try {
			inAsyncOperation = true;
			asyncModalText = 'Pulling latest with git';

			await syncLatest();
			// refresh will run when commits view opens
		} catch (e) {
			await emit('error', e);
		}

		inAsyncOperation = false;
	};

	const handleSearchClicked = async () => {
		showSearchModal = true;
		await tick();
		searchInput.focus();
	};

	const handleSyncToolsClicked = async () => {
		try {
			inAsyncOperation = true;
			asyncModalText = 'Pulling tools with git';

			try {
				$appConfig = await getAppConfig();
			} catch (e) {
				await emit('error', e);
			}

			const didSync: boolean = await syncTools();
			if (!didSync && $appConfig.toolsPath && $appConfig.toolsUrl) {
				await cloneRepo({ url: $appConfig.toolsUrl, path: $appConfig.toolsPath });
				await runSetEnv();
			}
		} catch (e) {
			await emit('error', e);
		}

		inAsyncOperation = false;
	};

	const handleGetDirectoryMetadata = async () => {
		try {
			if ($selectedFile) {
				directoryMetadata = await getDirectoryMetadata($selectedFile.path);
				$selectedDirectoryClass = directoryMetadata?.directoryClass ?? defaultDirectoryClass;
			}
		} catch (e) {
			await emit('error', e);
		}
	};

	const handleEditDirectoryClass = () => {
		editingDirectoryClass = true;
	};

	const saveDirectoryClass = async () => {
		updatingDirectoryClass = true;

		try {
			await updateMetadataClass($currentRoot, tempDirectoryClass);

			await handleGetDirectoryMetadata();
		} catch (e) {
			await emit('error', e);
		}

		editingDirectoryClass = false;
		updatingDirectoryClass = false;
	};

	const cancelEditDirectoryClass = () => {
		editingDirectoryClass = false;
	};

	const handleUpdateDirectoryMetadata = async (metadata: DirectoryMetadata) => {
		try {
			await updateDirectoryMetadata($currentRoot, metadata);
		} catch (e) {
			await emit('error', e);
		}
	};

	// file history
	let loadingFileHistory: boolean = false;
	let commits: Commit[] = [];

	const handleShowFileHistory = async () => {
		if ($selectedFile === null) return;

		loading = true;
		loadingFileHistory = true;
		commits = await getFileHistory($selectedFile.path);
		loadingFileHistory = false;
		loading = false;
	};

	const selectFile = async (file: LFSFile) => {
		$selectedFile = file;

		await handleShowFileHistory();
		await handleGetDirectoryMetadata();
	};

	const handleFileSelected = async (selected: LFSFile) => {
		// if shift is held, select or unselect everything in between
		$selectedTreeFiles = [];
		if (shiftHeld) {
			$selectedExplorerFiles = [];
			const currentIndex = $currentRootFiles.findIndex((file) => file.name === selected.name);
			const lastSelectedIndex = $currentRootFiles.findIndex(
				(file) => $selectedFile?.name === file.name
			);

			if (currentIndex > lastSelectedIndex) {
				for (let i = lastSelectedIndex; i <= currentIndex; i += 1) {
					if (!$selectedExplorerFiles.includes($currentRootFiles[i])) {
						$selectedExplorerFiles = [...$selectedExplorerFiles, $currentRootFiles[i]];
					} else {
						$selectedExplorerFiles = $selectedExplorerFiles.filter(
							(item) => item.name !== $currentRootFiles[i].name
						);
					}
				}
			} else {
				for (let i = currentIndex; i <= lastSelectedIndex; i += 1) {
					if (!$selectedExplorerFiles.includes($currentRootFiles[i])) {
						$selectedExplorerFiles = [...$selectedExplorerFiles, $currentRootFiles[i]];
					} else {
						$selectedExplorerFiles = $selectedExplorerFiles.filter(
							(item) => item.name !== $currentRootFiles[i].name
						);
					}
				}
			}
			$selectedFile = null;
			return;
		}
		if (ctrlHeld) {
			// if there was any file selected before ctrl was held, also add it to the list
			if ($selectedFile) {
				const lastSelectedIndex = $currentRootFiles.findIndex(
					(file) => $selectedFile?.name === file.name
				);
				if (!$selectedExplorerFiles.includes($currentRootFiles[lastSelectedIndex])) {
					$selectedExplorerFiles = [
						...$selectedExplorerFiles,
						$currentRootFiles[lastSelectedIndex]
					];
				}
				$selectedFile = null;
			}

			const currentIndex = $currentRootFiles.findIndex((file) => file.name === selected.name);
			$selectedExplorerFiles = [...$selectedExplorerFiles, $currentRootFiles[currentIndex]];
			return;
		}
		await selectFile(selected);
		$selectedExplorerFiles = [];
	};

	const formatBytes = (bytes: number, decimals = 2): string => {
		if (bytes === 0) return '0 Bytes';

		const k = 1024;
		const dm = decimals < 0 ? 0 : decimals;
		const sizes = ['Bytes', 'KB', 'MB', 'GB', 'TB', 'PB', 'EB', 'ZB', 'YB'];

		const i = Math.floor(Math.log(bytes) / Math.log(k));

		return `${parseFloat((bytes / k ** i).toFixed(dm))} ${sizes[i]}`;
	};

	const refreshFiles = async () => {
		// do nothing if we have no repo path
		if ($appConfig.repoPath === '') return;

		loading = true;

		const root = get(currentRoot);
		try {
			if (root !== '') {
				$currentRootFiles = await getFiles($currentRoot);
			} else {
				$currentRootFiles = await getFiles();
			}

			allFiles = await getAllFiles();

			await handleGetDirectoryMetadata();
			$fetchIncludeList = await getFetchInclude();
			$repoStatus = await getRepoStatus();
		} catch (e) {
			await emit('error', e);
		}
		$selectedFiles = $selectedFiles.filter((file) => {
			// clear autosave files if hide autosave is set
			if ($appConfig.hideAutosave && file.path.includes('/.autosave/')) {
				return false;
			}
			// clear selected files if they no longer exist
			return (
				$repoStatus?.modifiedFiles.some((f) => f.path === file.path) ||
				$repoStatus?.untrackedFiles.some((f) => f.path === file.path)
			);
		});

		loading = false;
	};

	const downloadFiles = async (paths: string[]) => {
		downloadInProgress = true;

		try {
			await downloadLFSFiles(paths, includeWip);
		} catch (e) {
			await emit('error', e);
		}

		downloadInProgress = false;

		await refreshFiles();
	};

	const handleDownloadFile = async (selected: Nullable<LFSFile>) => {
		if (selected === null || $selectedFile === null) return;
		loading = true;
		const fullPath = selected.path;

		try {
			await downloadFiles([fullPath]);

			$selectedFile.lfsState = LocalFileLFSState.Local;
			$fetchIncludeList = await getFetchInclude();
		} catch (e) {
			await emit('error', e);
		}
		loading = false;
	};

	const handleDownloadSelectedFiles = async () => {
		if ($selectedExplorerFiles.length > 0 && $selectedTreeFiles.length > 0) {
			await emit('error', 'Selected file state inconsistent');
		}

		if ($selectedExplorerFiles.length === 0 && $selectedTreeFiles.length === 0) return;
		loading = true;

		let paths;
		if ($selectedExplorerFiles.length > 0) {
			paths = $selectedExplorerFiles.map((file) => file.path);
		} else {
			paths = $selectedTreeFiles.map((file) => file.path);
		}

		try {
			await downloadFiles(paths);

			$fetchIncludeList = await getFetchInclude();
		} catch (e) {
			await emit('error', e);
		}

		loading = false;
	};

	const handleUnFavoriteFile = async (selected: Nullable<LFSFile>) => {
		if (selected === null) return;
		loading = true;

		const fullPath = selected.path;

		try {
			await delFetchInclude([fullPath]);

			$fetchIncludeList = await getFetchInclude();
		} catch (e) {
			await emit('error', e);
		}
		loading = false;
	};

	const handleUnFavoriteSelectedFiles = async () => {
		if ($selectedExplorerFiles.length > 0 && $selectedTreeFiles.length > 0) {
			await emit('error', 'Selected file state inconsistent');
		}

		if ($selectedExplorerFiles.length === 0 && $selectedTreeFiles.length === 0) return;
		loading = true;

		let paths;
		if ($selectedExplorerFiles.length > 0) {
			paths = $selectedExplorerFiles.map((file) => file.path);
		} else {
			paths = $selectedTreeFiles.map((file) => file.path);
		}

		try {
			await delFetchInclude(paths);

			$fetchIncludeList = await getFetchInclude();
		} catch (e) {
			await emit('error', e);
		}
		loading = false;
	};

	const handleLockSelectedFiles = async () => {
		if ($selectedExplorerFiles.length > 0 && $selectedTreeFiles.length > 0) {
			await emit('error', 'Selected file state inconsistent');
		}

		if ($selectedExplorerFiles.length === 0 && $selectedTreeFiles.length === 0) return;
		loading = true;

		let paths;
		if ($selectedExplorerFiles.length > 0) {
			paths = $selectedExplorerFiles.map((file) => file.path);
		} else {
			paths = $selectedTreeFiles.map((file) => file.path);
		}

		try {
			await lockFiles(paths);

			$locks = await verifyLocks();
		} catch (e) {
			await emit('error', e);
		}

		await refreshFiles();
		loading = false;
	};

	const handleUnlockSelectedFiles = async () => {
		if ($selectedExplorerFiles.length > 0 && $selectedTreeFiles.length > 0) {
			await emit('error', 'Selected file state inconsistent');
		}

		if ($selectedExplorerFiles.length === 0 && $selectedTreeFiles.length === 0) return;
		loading = true;

		let paths;
		if ($selectedExplorerFiles.length > 0) {
			paths = $selectedExplorerFiles.map((file) => file.path);
		} else {
			paths = $selectedTreeFiles.map((file) => file.path);
		}

		try {
			await unlockFiles(paths, false);

			$locks = await verifyLocks();
		} catch (e) {
			await emit('error', e);
		}

		await refreshFiles();
		loading = false;
	};

	const lockSelectedFile = async () => {
		if ($selectedFile === null) return;

		loading = true;

		const fullPath = $selectedFile.path;

		try {
			await lockFiles([fullPath]);

			$locks = await verifyLocks();
		} catch (e) {
			await emit('error', e);
		}

		await refreshFiles();
		loading = false;
	};

	const unlockSelectedFile = async () => {
		if ($selectedFile === null) return;

		loading = true;

		const fullPath = $selectedFile.path;

		try {
			await unlockFiles([fullPath], false);

			$locks = await verifyLocks();
		} catch (e) {
			await emit('error', e);
		}

		await refreshFiles();
		loading = false;
	};

	const handleSubmit = async () => {
		loading = true;
		inAsyncOperation = true;
		asyncModalText = 'Submitting';

		await refreshFiles();

		const req: PushRequest = {
			commitMessage: $commitMessage,
			files: $selectedFiles.map((file) => file.path)
		};

		try {
			await submit(req);

			$repoStatus = await getRepoStatus();

			$commitMessage = '';
			$selectedFiles = [];
			selectAll = false;
		} catch (e) {
			await emit('error', e);
		}

		inAsyncOperation = false;
		asyncModalText = '';
		loading = false;
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
		inAsyncOperation = true;
		asyncModalText = 'Locking Files';

		try {
			const selectedPaths = $selectedFiles.map((file) => file.path);
			await lockFiles(selectedPaths);
			await emit('success', 'Files locked!');
			await verifyLocks();
			await refreshLocks();
		} catch (e) {
			await emit('error', e);
		}

		inAsyncOperation = false;
		asyncModalText = '';
		loading = false;
	};

	const showInExplorer = async (file: Nullable<LFSFile>) => {
		if (file === null) return;

		let directory;
		if (file.fileType === FileType.File) {
			const parentPath = file.path.substring(0, file.path.lastIndexOf('/'));
			directory = `${$appConfig.repoPath}/${parentPath}`;
		} else {
			directory = `${$appConfig.repoPath}/${$currentRoot}`;
		}

		await openUrl(directory);
	};

	const handleOpenDirectory = async (path: string) => {
		const parent = path.split('/').slice(0, -1).join('/');

		// Birdie opens up the Y drive
		const fullPath = `Y:/${parent}`;

		await openUrl(fullPath);
	};

	const getLockOwner = (selected: LFSFile): string => {
		if (!selected.locked || selected.lockInfo === null) return 'None';
		return selected.lockInfo.lock.owner?.name ?? 'None';
	};

	const onKeyDown = (event: KeyboardEvent) => {
		if (event.key === 'Shift') {
			shiftHeld = true;
			return;
		}

		if (event.key === 'Control') {
			ctrlHeld = true;
		}
	};

	const onKeyUp = (e: KeyboardEvent) => {
		if (e.key === 'Shift') {
			shiftHeld = false;
			return;
		}

		if (e.key === 'Control') {
			ctrlHeld = false;
		}
	};

	const onSearchModalClosed = () => {
		search = '';
	};

	const addSelectedFilePathToFileTree = async (node: Node, subFolders: string[]): Promise<Node> => {
		if (node.value.fileType === FileType.File) return node;
		const updatedChildFiles = await getFiles(node.value.path);
		let updatedChildNodes: Node[] = [];
		if (subFolders.length === 0) {
			// we're at the deepest subfolder level
			// update our children and "forget" anything deeper than this
			updatedChildFiles.forEach((child) => {
				updatedChildNodes.push({
					value: child,
					open: false,
					children: []
				});
			});
		} else {
			// some extra steps here to ensure we don't overwrite any sibling/deeper nodes
			updatedChildFiles.forEach((child) => {
				const existingChild = node.children.find((c) => c.value.path === child.path);
				if (existingChild) {
					updatedChildNodes.push({
						...existingChild,
						value: child
					});
				} else {
					updatedChildNodes.push({
						value: child,
						open: false,
						children: []
					});
				}
			});
			// recursively call on the child node that matches the next subfolder
			updatedChildNodes = await Promise.all(
				updatedChildNodes.map((child) => {
					if (child.value.name === subFolders[0]) {
						return addSelectedFilePathToFileTree(child, subFolders.slice(1));
					}
					return child;
				})
			);
		}
		return { ...node, open: true, children: updatedChildNodes };
	};

	const selectSearchResult = async (path: string) => {
		modalLoading = true;
		// strip last part of the path
		const parts = path.split('/');
		ancestry = parts.slice(0, -1);

		// set current root
		$currentRoot = ancestry.join('/');

		// select the file
		await refreshFiles();

		const newSelectedFile =
			$currentRootFiles.find((f) => f.name === parts[parts.length - 1]) ?? null;

		if (newSelectedFile) {
			await selectFile(newSelectedFile);
		}

		$rootNode = await addSelectedFilePathToFileTree(get(rootNode), $selectedFile.path.split('/'));

		modalLoading = false;
		showSearchModal = false;
	};

	const handleSaveFileTree = async () => {
		await fs.writeTextFile(FILE_TREE_PATH, JSON.stringify($rootNode, null, 2), {
			baseDir: fs.BaseDirectory.AppLocalData
		});
	};

	const handleLoadFileTree = async () => {
		if (await fs.exists(FILE_TREE_PATH, { baseDir: fs.BaseDirectory.AppLocalData })) {
			const fileTreeResponse = await fs.readTextFile(FILE_TREE_PATH, {
				baseDir: fs.BaseDirectory.AppLocalData
			});
			const parsedFileTree: Node = JSON.parse(fileTreeResponse);
			rootNode.set(parsedFileTree);
		}
	};

	const handleSaveCurrentRoot = async () => {
		await fs.writeTextFile(CURRENT_ROOT_PATH, $currentRoot, {
			baseDir: fs.BaseDirectory.AppLocalData
		});
	};

	const handleLoadCurrentRoot = async () => {
		if (
			!$currentRoot &&
			(await fs.exists(CURRENT_ROOT_PATH, { dir: fs.BaseDirectory.AppLocalData }))
		) {
			const currentRootResponse = await fs.readTextFile(CURRENT_ROOT_PATH, {
				baseDir: fs.BaseDirectory.AppLocalData
			});
			currentRoot.set(currentRootResponse);
		}
		await refreshFiles();
	};

	const handleSaveChangesets = async (newChangesets: ChangeSet[]) => {
		$changeSets = newChangesets;
		await fs.writeTextFile(CHANGE_SETS_PATH, JSON.stringify($changeSets, null, 2), {
			baseDir: fs.BaseDirectory.AppLocalData
		});
	};

	const handleRevertFiles = async () => {
		loading = true;

		await refreshFiles();

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

		loading = false;
	};

	const onFileTreeClick = async (file: LFSFile) => {
		await selectFile(file);
		await handleShowFileHistory();
	};

	void listen('refresh-files', () => {
		void refreshFiles();
	});

	onMount(() => {
		void refreshFiles();

		const setup = async (): Promise<void> => {
			$fetchIncludeList = await getFetchInclude();
			await handleLoadFileTree();
			await handleLoadCurrentRoot();
			$selectedDirectoryClass = defaultDirectoryClass;
		};
		void setup();
	});

	onDestroy(() => {
		void handleSaveFileTree();
		void handleSaveCurrentRoot();
	});
</script>

<svelte:window on:keydown={onKeyDown} on:keyup={onKeyUp} />
<div class="flex flex-col h-full gap-2">
	<div class="flex items-baseline gap-2">
		<p class="text-2xl mt-2 dark:text-primary-400">Asset Explorer</p>
		<Button disabled={loading} class="!p-1.5" primary on:click={() => refreshFiles()}>
			{#if loading}
				<Spinner size="4" />
			{:else}
				<RotateOutline class="w-4 h-4" />
			{/if}
		</Button>
		<Button disabled={loading} class="!p-1.5" primary on:click={handleSearchClicked}>
			<SearchOutline class="w-4 h-4" />
		</Button>
		{#if loading}
			<Spinner class="w-4 h-4 dark:text-gray-500 fill-white" />
		{/if}
		<ButtonGroup size="xs" class="space-x-px ml-auto">
			<Button
				size="xs"
				color="primary"
				disabled={inAsyncOperation || loading}
				on:click={async () => handleSyncClicked()}
			>
				<RotateOutline class="w-3 h-3 mr-2" />
				Sync
			</Button>
			<Button
				size="xs"
				color="primary"
				disabled={inAsyncOperation || loading}
				on:click={async () => handleSyncToolsClicked()}
			>
				<RotateOutline class="w-3 h-3 mr-2" />
				Tools
			</Button>
		</ButtonGroup>
	</div>
	<div class="flex gap-2 justify-between overflow-x-auto overflow-y-hidden py-1 h-8 min-h-[2rem]">
		<Breadcrumb
			aria-label="File ancestry"
			olClass="inline-flex items-center space-x-1 rtl:space-x-reverse rtl:space-x-reverse"
		>
			<BreadcrumbItem
				homeClass="inline-flex items-center text-sm font-medium text-gray-700 hover:text-gray-900 dark:text-gray-400 dark:hover:text-gray-400"
				home
			>
				<span class="mx-0 py-1">/</span>
			</BreadcrumbItem>
			{#each ancestry as path}
				<BreadcrumbItem spanClass="text-sm font-medium text-gray-500 dark:text-gray-400">
					<span class="mx-0 py-1">{path}</span>
				</BreadcrumbItem>
			{/each}
		</Breadcrumb>
		<div class="flex gap-2">
			<Checkbox bind:checked={showSourceControl}>Source Control</Checkbox>
		</div>
	</div>
	{#if $selectedDirectoryClass === 'character' && directoryMetadata}
		<CharacterCard metadata={directoryMetadata} onMetadataSaved={handleUpdateDirectoryMetadata} />
	{/if}
	<div class="flex gap-2 overflow-hidden w-full max-w-full max-h-[65vh]">
		<div class="flex flex-col min-w-[25vw] gap-2 h-full gap-2">
			<FileTree bind:fileNode={$rootNode} bind:loading onFileClick={onFileTreeClick} />
			<Card
				class="h-10 p-4 sm:p-4 max-w-full max-h-full dark:bg-secondary-600 border-0 shadow-none"
			>
				<div class="flex items-center gap-4 h-full">
					<p class="text-lg my-2 dark:text-primary-400">Directory Class</p>
					{#if editingDirectoryClass}
						<div class="flex gap-2">
							<Select
								size="sm"
								class="w-32 text-center"
								items={directoryClassOptions}
								bind:value={tempDirectoryClass}
							/>

							<Button
								disabled={updatingDirectoryClass}
								size="xs"
								class="my-1"
								on:click={saveDirectoryClass}
							>
								<CheckSolid class="w-4 h-4" />
							</Button>
							<Button
								disabled={updatingDirectoryClass}
								size="xs"
								class="my-1 dark:bg-red-800 hover:dark:bg-red-900"
								on:click={cancelEditDirectoryClass}
							>
								<CloseSolid class="w-4 h-4" />
							</Button>
						</div>
					{:else}
						<div class="flex gap-2">
							<code class="dark:bg-secondary-700 px-2 py-1 w-32 text-center text-white"
								>{$selectedDirectoryClass}</code
							>
							<Button
								size="xs"
								disabled={$selectedExplorerFiles.length > 0}
								on:click={handleEditDirectoryClass}
							>
								<EditOutline class="w-4 h-4" />
							</Button>
						</div>
					{/if}
				</div>
			</Card>
			<Card
				class="flex flex-col max-w-full p-4 sm:p-4 pt-1 sm:pt-1 min-h-[12rem] max-h-[36rem] h-[32rem] dark:bg-secondary-600 border-0 shadow-none overflow-hidden"
			>
				<div class="flex items-center gap-2">
					<p class="text-xl my-2 dark:text-primary-400">File Details</p>
					{#if $selectedFile !== null}
						<Button
							class="w-4 h-8"
							disabled={$selectedExplorerFiles.length > 0 || $selectedTreeFiles.length > 0}
							on:click={() => showInExplorer($selectedFile)}
						>
							<FileSearchOutline class="w-4 h-4" />
						</Button>
						<Tooltip>Show in explorer</Tooltip>
					{/if}
				</div>
				{#if $selectedFile === null && $selectedExplorerFiles.length === 0 && $selectedTreeFiles.length === 0}
					<p class="text-gray-500 dark:text-gray-400 pb-4">No file selected.</p>
				{:else}
					<div class="flex flex-col gap-2 w-full h-full">
						{#if $selectedExplorerFiles.length > 0 || $selectedTreeFiles.length > 0}
							<p class="pb-4">Multiple files selected.</p>
							<div class="flex flex-col mt-auto gap-2">
								<Toggle bind:checked={includeWip}>Include WIP</Toggle>
								<Tooltip placement="top">
									Enable if you want to include WIP folders in the operation.
								</Tooltip>
								<div class="flex flex-row gap-2">
									<Button class="w-full" disabled={loading} on:click={handleDownloadSelectedFiles}
										>Download</Button
									>
									<Tooltip
										>Downloads selected files on disk and adds them to the automatic downloads list.
									</Tooltip>
									<Button class="w-full" disabled={loading} on:click={handleUnFavoriteSelectedFiles}
										>Unfavorite</Button
									>
									<Tooltip
										>Downloads selected files on disk and adds them to the automatic downloads list.
									</Tooltip>
								</div>
								<div class="flex flex-row gap-2">
									<Button class="w-full" disabled={loading} on:click={handleLockSelectedFiles}
										>Lock</Button
									>
									<Button class="w-full" disabled={loading} on:click={handleUnlockSelectedFiles}
										>Unlock</Button
									>
								</div>
							</div>
						{:else}
							<div class="w-full">
								<div class="grid grid-cols-2 gap-0 w-full dark:text-white">
									<div class="flex gap-2 col-span-2">
										<span class="w-20">Name:</span>
										<p class="dark:text-primary-400 break-all">{$selectedFile.name}</p>
									</div>
									<div class="flex gap-2">
										<span class="w-20">Size:</span>
										<span class="dark:text-primary-400">{formatBytes($selectedFile.size)}</span>
									</div>
									<div class="flex gap-2">
										<span class="w-20">On disk:</span>
										<span class="dark:text-primary-400"
											>{$selectedFile.lfsState === LocalFileLFSState.Stub ? 'No' : 'Yes'}</span
										>
									</div>
									<div class="flex gap-2">
										<span class="w-20">Favorited:</span>
										<span class="dark:text-primary-400"
											>{!$fetchIncludeList.includes($selectedFile.path) ? 'No' : 'Yes'}</span
										>
									</div>
									<div class="flex gap-2">
										<span class="w-20">Locked by:</span>
										<span class="dark:text-primary-400">{getLockOwner($selectedFile)}</span>
									</div>
								</div>
							</div>
							<div class="flex flex-col mt-auto gap-2">
								<Toggle bind:checked={includeWip}>Include WIP</Toggle>
								<Tooltip placement="top">
									Enable if you want to include WIP folders in the operation.
								</Tooltip>
								<div class="flex flex-row gap-2">
									<Button
										class="w-full"
										disabled={loading || $selectedFile.lfsState === LocalFileLFSState.Local}
										on:click={async () => handleDownloadFile($selectedFile)}>Download</Button
									>
									<Tooltip
										>Downloads selected files on disk and adds them to the automatic downloads list.
									</Tooltip>
									<Button
										class="w-full"
										disabled={loading ||
											($selectedFile?.fileType === FileType.File &&
												!$fetchIncludeList.includes($selectedFile.path))}
										on:click={async () => handleUnFavoriteFile($selectedFile)}>Unfavorite</Button
									>
									<Tooltip
										>Downloads selected files on disk and adds them to the automatic downloads list.
									</Tooltip>
								</div>
								<div class="flex flex-row gap-2">
									<Button
										class="w-full"
										disabled={loading || $selectedFile.locked}
										on:click={lockSelectedFile}>Lock</Button
									>
									<Button
										class="w-full"
										disabled={loading ||
											($selectedFile.fileType === FileType.File && !$selectedFile.lockInfo?.ours)}
										on:click={unlockSelectedFile}>Unlock</Button
									>
								</div>
							</div>
						{/if}
					</div>
				{/if}
			</Card>
		</div>
		<div class="flex flex-row gap-2 w-full h-full">
			<Card class="sm:p-4 h-full max-w-full w-full dark:bg-secondary-600 border-0 shadow-none">
				<Table>
					<TableBody>
						{#each $currentRootFiles as file, index}
							<TableBodyRow
								class="text-left border-b-0 {index % 2 === 0
									? 'bg-secondary-700 dark:bg-space-900'
									: 'bg-secondary-800 dark:bg-space-950'}"
							>
								<TableBodyCell class="p-2 w-4">
									{#if file.fileType === FileType.File}
										{#if $fetchIncludeList.includes(file.path)}
											<HeartSolid class="w-4 h-4 text-green-500" />
										{:else}
											<HeartOutline class="w-4 h-4 text-gray-500" />
										{/if}
									{/if}
								</TableBodyCell>
								<TableBodyCell class="p-2">
									{#if file.fileType === FileType.Directory}
										<Button
											outline={!(
												$selectedFile?.path === file.path ||
												$selectedExplorerFiles.some((f) => f.path === file.path)
											)}
											disabled={loading}
											class="flex justify-start items-center py-0.5 pl-2 border-0 w-full"
											on:click={() => handleFileSelected(file)}
										>
											<FolderSolid class="h-6 w-6 pr-2" />{file.name}</Button
										>
									{:else}
										<Button
											outline={!(
												$selectedFile?.path === file.path ||
												$selectedExplorerFiles.some((f) => f.path === file.path)
											)}
											class="justify-start border-0 py-0.5 pl-2 rounded-md w-full group"
											on:click={() => handleFileSelected(file)}
										>
											{#if file.locked}
												{#if file.lockInfo?.ours}
													<LockSolid class="h-5 w-5 pr-2" />
												{:else}
													<LockSolid class="h-5 w-5 pr-2 text-gray-500" />
													<Tooltip>
														Locked by {file.lockInfo?.lock.owner?.name}
													</Tooltip>
												{/if}
											{:else}
												<LockOpenOutline class="h-6 w-6 pr-2 text-gray-500" />
											{/if}
											<span
												class="group-hover:text-white"
												class:text-gray-400={file.lfsState !== LocalFileLFSState.Local &&
													!(
														$selectedFile?.path === file.path ||
														$selectedExplorerFiles.some((f) => f.path === file.path)
													)}>{file.name}</span
											>
										</Button>
									{/if}
								</TableBodyCell>
							</TableBodyRow>
						{/each}
					</TableBody>
				</Table>
			</Card>
			{#if showSourceControl}
				<div class="flex flex-col gap-2 w-full h-full">
					<Card
						class="sm:p-4 max-w-full h-full dark:bg-secondary-600 border-0 shadow-none overflow-auto"
					>
						<div class="flex flex-col overflow-hidden w-full h-full">
							<ModifiedFilesCard
								disabled={loading}
								bind:selectedFiles={$selectedFiles}
								bind:selectAll
								onOpenDirectory={handleOpenDirectory}
								modifiedFiles={$allModifiedFiles}
								changeSets={$changeSets}
								onChangesetsSaved={handleSaveChangesets}
								onRevertFiles={handleRevertFiles}
								snapshotsEnabled={false}
								onLockSelected={handleLockSelected}
								bind:enableGlobalSearch={$enableGlobalSearch}
							/>
						</div>
					</Card>
					<Card
						class="w-full p-4 gap-2 sm:p-4 max-w-full h-full max-h-[12rem] dark:bg-secondary-600 border-0 shadow-none"
					>
						<div class="flex flex-col w-full h-full gap-2">
							<div class="flex flex-row justify-between gap-2">
								<Label for="commit-message" class="mb-2">Commit Message</Label>
								<p class="font-semibold text-sm">
									On branch: <span class="font-normal text-primary-400">{$repoStatus?.branch}</span>
								</p>
							</div>
							<Textarea
								id="commit-message"
								bind:value={$commitMessage}
								on:focus={() => {
									$enableGlobalSearch = false;
								}}
								on:blur={() => {
									$enableGlobalSearch = true;
								}}
								class="dark:bg-secondary-800 min-h-[4rem] h-full"
							/>
							<div class="flex flex-row w-full align-middle justify-end">
								<ButtonGroup class="space-x-px">
									<Button color="primary" disabled={!canSubmit} on:click={handleSubmit}
										>Submit</Button
									>
								</ButtonGroup>
							</div>
						</div>
					</Card>
				</div>
			{/if}
		</div>
	</div>
	<Card
		class="w-full p-4 sm:p-4 max-w-full min-h-[11rem] max-h-[30vh] dark:bg-secondary-600 border-0 shadow-none overflow-auto"
	>
		{#if loadingFileHistory && commits.length === 0}
			<Spinner class="w-4 h-4 dark:text-gray-500 fill-white" />
		{:else}
			<CommitTable {commits} showFilesHandler={showCommitFiles} />
		{/if}
	</Card>
</div>

<ProgressModal title="Downloading files" bind:showModal={downloadInProgress} />

<Modal
	bind:open={showSearchModal}
	on:close={onSearchModalClosed}
	size="xl"
	placement="top-center"
	defaultClass="dark:bg-secondary-800 overflow-y-hidden"
	dialogClass="fixed mt-8 top-0 start-0 end-0 h-modal md:inset-0 md:h-full z-50 w-full p-4 pb-12 flex overflow-y-hidden"
	bodyClass="!border-t-0"
	backdropClass="fixed mt-8 inset-0 z-40 bg-gray-900 bg-opacity-50 dark:bg-opacity-80"
>
	<div class="flex flex-col gap-4 h-full">
		<div class="flex flex-col gap-1">
			<div class="flex justify-center">
				<Input class="w-full mr-8" let:props>
					<input {...props} bind:value={search} bind:this={searchInput} />
				</Input>
			</div>
			<div class="flex justify-between mr-8">
				<span>Fuzzy, space-delimited search!</span>
				<span class="font-mono">{filteredFiles.length} files found</span>
			</div>
		</div>
		<div
			class="text-white font-mono tracking-wide rounded-xl border-primary-500 border border-r-0 dark:bg-secondary-800 max-h-[85vh] flex flex-col overflow-y-auto"
		>
			<div class="m-2 h-full">
				{#if modalLoading}
					<div class="text-center p-4">Loading...</div>
				{:else if filteredFiles.length > 0}
					{#each filteredFiles as path}
						<div>
							<Button
								size="sm"
								outline
								class="rounded-none p-0.5 my-1 w-full text-md text-left justify-start border-0"
								on:click={() => selectSearchResult(path)}>{path.split('/').reverse()[0]}</Button
							>
						</div>
					{/each}
				{:else}
					<div class="text-center p-4">Search for files! Query must be at least 3 characters.</div>
				{/if}
			</div>
		</div>
	</div>
</Modal>

<ProgressModal bind:showModal={inAsyncOperation} bind:title={asyncModalText} />
