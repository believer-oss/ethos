<script lang="ts">
	import {
		Button,
		ButtonGroup,
		Card,
		Input,
		Modal,
		Select,
		Spinner,
		Table,
		TableBody,
		TableBodyCell,
		TableBodyRow,
		Toggle,
		Tooltip
	} from 'flowbite-svelte';
	import { onMount, onDestroy, tick } from 'svelte';
	import {
		CheckSolid,
		CloseSolid,
		EditOutline,
		FileCheckOutline,
		FileCheckSolid,
		FolderSolid,
		HeartOutline,
		HeartSolid,
		RotateOutline
	} from 'flowbite-svelte-icons';
	import { emit, listen } from '@tauri-apps/api/event';
	import { type Commit, CommitTable, ProgressModal } from '@ethos/core';
	import { get } from 'svelte/store';
	import { fs } from '@tauri-apps/api';
	import {
		cloneRepo,
		delFetchInclude,
		downloadLFSFiles,
		getAllFiles,
		getFetchInclude,
		getFileHistory,
		getFiles,
		lockFiles,
		showCommitFiles,
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
		type Nullable
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
		selectedTreeFiles
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
	import { CURRENT_ROOT_PATH, FILE_TREE_PATH } from '$lib/consts';

	let loading = false;
	let allFiles: string[] = [];
	let downloadInProgress: boolean = false;
	let search: string = '';
	let showSearchModal: boolean = false;
	let searchInput: HTMLInputElement;
	let modalLoading: boolean = false;
	let includeWip = true;

	// sync and tools
	let inAsyncOperation = false;
	let asyncModalText = '';

	$: filteredFiles = allFiles.filter(
		(file) =>
			search.split(' ').every((s) => file.toLowerCase().includes(s.toLowerCase())) &&
			search.length > 2
	);

	$: ancestry = $currentRoot.split('/').filter((a) => a !== '');

	// directory metadata
	let directoryMetadata: Nullable<DirectoryMetadata> = null;
	let editingDirectoryClass: boolean = false;
	const defaultDirectoryClass: string = 'none';
	let selectedDirectoryClass: string = defaultDirectoryClass;
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
			directoryMetadata = await getDirectoryMetadata($currentRoot);
			selectedDirectoryClass = directoryMetadata?.directoryClass ?? defaultDirectoryClass;
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

		loadingFileHistory = true;
		commits = await getFileHistory($selectedFile.path);
		loadingFileHistory = false;
	};

	const selectFile = async (file: LFSFile) => {
		$selectedFile = file;

		if (file.fileType === FileType.File) {
			await handleShowFileHistory();
		} else {
			commits = [];
		}
		await handleGetDirectoryMetadata();
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
		} catch (e) {
			await emit('error', e);
		}

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
		const fullPath = selected.path;

		try {
			await downloadFiles([fullPath]);

			$selectedFile.lfsState = LocalFileLFSState.Local;
			$fetchIncludeList = await getFetchInclude();
		} catch (e) {
			await emit('error', e);
		}
	};

	const handleDownloadSelectedFiles = async () => {
		loading = true;

		if ($selectedTreeFiles.length === 0) return;

		const paths = $selectedTreeFiles.map((file) => file.path);

		try {
			await downloadFiles(paths);

			$selectedTreeFiles = [];
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
		if ($selectedTreeFiles.length === 0) return;
		loading = true;

		const paths = $selectedTreeFiles.map((file) => file.path);

		try {
			await delFetchInclude(paths);

			$selectedTreeFiles = [];
			$fetchIncludeList = await getFetchInclude();
		} catch (e) {
			await emit('error', e);
		}
		loading = false;
	};

	const handleLockSelectedFiles = async () => {
		if ($selectedTreeFiles.length === 0) return;
		loading = true;

		const paths = $selectedTreeFiles.map((file) => file.path);

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
		loading = true;
		if ($selectedTreeFiles.length === 0) return;

		const paths = $selectedTreeFiles.map((file) => file.path);

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

	const showInExplorer = async (file: Nullable<LFSFile>) => {
		if (file === null) return;

		const parentPath = file.path.substring(0, file.path.lastIndexOf('/'));
		const directory = `${$appConfig.repoPath}/${parentPath}`;

		await openUrl(directory);
	};

	const getLockOwner = (selected: LFSFile): string => {
		if (!selected.locked || selected.lockInfo === null) return 'None';
		return selected.lockInfo.lock.owner?.name ?? 'None';
	};

	const onKeyDown = async (event: KeyboardEvent) => {
		if (!$enableGlobalSearch) return;

		if (search === '' && event.key.match(/^[a-z]$/) && $appConfig.repoPath !== '') {
			showSearchModal = true;

			await tick();
			searchInput.focus();
		}
	};

	const onSearchModalClosed = () => {
		search = '';
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

		modalLoading = false;
		showSearchModal = false;
	};

	const handleSaveFileTree = async () => {
		await fs.writeFile(FILE_TREE_PATH, JSON.stringify($rootNode, null, 2), {
			dir: fs.BaseDirectory.AppLocalData
		});
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

	const handleLoadFileTree = async () => {
		if (await fs.exists(FILE_TREE_PATH, { dir: fs.BaseDirectory.AppLocalData })) {
			const fileTreeResponse = await fs.readTextFile(FILE_TREE_PATH, {
				dir: fs.BaseDirectory.AppLocalData
			});
			const parsedFileTree: Node = JSON.parse(fileTreeResponse);
			rootNode.set(parsedFileTree);
		}
		if ($selectedFile) {
			$rootNode = await addSelectedFilePathToFileTree(get(rootNode), $selectedFile.path.split('/'));
		}
	};

	const handleSaveCurrentRoot = async () => {
		await fs.writeFile(CURRENT_ROOT_PATH, $currentRoot, { dir: fs.BaseDirectory.AppLocalData });
	};

	const handleLoadCurrentRoot = async () => {
		if (
			!$currentRoot &&
			(await fs.exists(CURRENT_ROOT_PATH, { dir: fs.BaseDirectory.AppLocalData }))
		) {
			const currentRootResponse = await fs.readTextFile(CURRENT_ROOT_PATH, {
				dir: fs.BaseDirectory.AppLocalData
			});
			currentRoot.set(currentRootResponse);
		}
		if ($selectedFile?.fileType === FileType.File) {
			commits = await getFileHistory($selectedFile.path);
		} else {
			commits = [];
		}
		await refreshFiles();
	};

	void listen('refresh-files', () => {
		void refreshFiles();
	});

	onMount(() => {
		void refreshFiles();

		const setupFetchIncludeList = async (): Promise<void> => {
			$fetchIncludeList = await getFetchInclude();
		};
		void setupFetchIncludeList();

		const setupAssetExplorerViews = async (): Promise<void> => {
			await handleLoadFileTree();
			await handleLoadCurrentRoot();
		};
		void setupAssetExplorerViews();

		// refresh every 30 seconds
		const interval = setInterval(() => {
			void refreshFiles();
		}, 30000);

		return () => {
			clearInterval(interval);
		};
	});

	onDestroy(() => {
		void handleSaveFileTree();
		void handleSaveCurrentRoot();
	});
</script>

<svelte:window on:keydown={onKeyDown} />
<div class="flex flex-col h-full gap-2">
	<div class="flex items-baseline gap-2">
		<p class="text-2xl mt-2 dark:text-primary-400">Asset Explorer</p>
		<span class="text-sm text-gray-300 italic tracking-wide"
			>(Start typing to search all files!)</span
		>
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
	{#if selectedDirectoryClass === 'character' && directoryMetadata}
		<CharacterCard metadata={directoryMetadata} onMetadataSaved={handleUpdateDirectoryMetadata} />
	{/if}
	<div class="flex gap-2 overflow-hidden w-full max-w-full max-h-[70vh]">
		<FileTree bind:fileNode={$rootNode} bind:loading />
		{#if $selectedTreeFiles.length !== 0}
			<div class="flex flex-col h-full min-h-full w-full">
				<Card
					class="w-full p-4 sm:p-4 h-full max-w-full dark:bg-secondary-600 border-0 shadow-none overflow-auto"
				>
					<div
						class="w-full flex flex-row justify-between gap-2 p-4 sm:p-4 max-w-full max-h-full dark:bg-secondary-600 border-0 shadow-none"
					>
						<Button disabled={loading} on:click={handleLockSelectedFiles}>Lock All</Button>
						<Button disabled={loading} on:click={handleUnlockSelectedFiles}>Unlock All</Button>
						<Button color="primary" disabled={loading} on:click={handleDownloadSelectedFiles}
							>Download All
						</Button>
						<Button disabled={loading} on:click={handleUnFavoriteSelectedFiles}
							>Unfavorite All
						</Button>
						<Toggle class="whitespace-nowrap" bind:checked={includeWip}>Include WIP</Toggle>
						<Tooltip>
							{includeWip ? 'Include' : 'Exclude'} WIP folders
						</Tooltip>
					</div>
					<div class="flex flex-col gap-2 w-full h-full">
						<Table>
							<TableBody>
								{#each $selectedTreeFiles as file, index}
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
											<div class="flex gap-2 items-center justify-start w-full">
												<Button
													outline={$selectedFile?.path !== file.path}
													class="justify-start border-0 py-0.5 pl-2 rounded-md w-full"
													on:click={() => selectFile(file)}
												>
													{#if file.fileType === FileType.File}
														{#if file.lfsState === LocalFileLFSState.Local}
															<FileCheckSolid class="w-4 h-4 text-green-500" />
														{:else}
															<FileCheckOutline class="w-4 h-4 text-gray-500" />
														{/if}
													{:else}
														<FolderSolid class="h-6 w-6 pr-2" />
													{/if}
													<div class="w-3 mr-3">{file.locked ? '🔒' : ''}</div>
													{file.name}
												</Button>
											</div>
										</TableBodyCell>
									</TableBodyRow>
								{/each}
							</TableBody>
						</Table>
					</div>
				</Card>
			</div>
		{/if}
		<div class="flex flex-col h-full min-w-[26rem] gap-2">
			<Card class="h-10 p-4 sm:p-4 max-h-full dark:bg-secondary-600 border-0 shadow-none">
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
								>{selectedDirectoryClass}</code
							>
							<Button size="xs" on:click={handleEditDirectoryClass}>
								<EditOutline class="w-4 h-4" />
							</Button>
						</div>
					{/if}
				</div>
			</Card>
			<Card
				class="flex flex-col h-full p-4 sm:p-4 max-h-full dark:bg-secondary-600 border-0 shadow-none overflow-hidden"
			>
				<div class="flex items-center gap-2">
					<p class="text-xl my-2 dark:text-primary-400">File Details</p>
				</div>
				{#if $selectedFile === null}
					<p class="text-gray-500 dark:text-gray-400 pb-4">No file selected.</p>
				{:else}
					<div class="flex flex-col gap-2 w-full h-full">
						<div class="w-full h-full">
							<div class="flex gap-2 w-full dark:text-white">
								<span class="w-20">Name:</span>
								<p class="dark:text-primary-400 w-64 break-all">{$selectedFile.name}</p>
							</div>
							<div class="flex gap-2 w-full dark:text-white">
								<span class="w-20">Size:</span>
								<span class="dark:text-primary-400 w-64">{formatBytes($selectedFile.size)}</span>
							</div>
							<div class="flex gap-2 w-full dark:text-white">
								<span class="w-20">On disk:</span>
								<span class="dark:text-primary-400 w-64"
									>{$selectedFile.lfsState === LocalFileLFSState.Stub ? 'No' : 'Yes'}</span
								>
							</div>
							<div class="flex gap-2 w-full dark:text-white">
								<span class="w-20">Favorited:</span>
								<span class="dark:text-primary-400 w-64"
									>{!$fetchIncludeList.includes($selectedFile.path) ? 'No' : 'Yes'}</span
								>
							</div>
							<div class="flex gap-2 w-full dark:text-white">
								<span class="w-20">Locked by:</span>
								<span class="dark:text-primary-400 w-64">{getLockOwner($selectedFile)}</span>
							</div>
						</div>
						<Button on:click={() => showInExplorer($selectedFile)}>Show in Explorer</Button>
						{#if $selectedFile.lfsState === LocalFileLFSState.Stub || !$fetchIncludeList.includes($selectedFile.path)}
							<Button
								class="w-full"
								color="primary"
								on:click={() => handleDownloadFile($selectedFile)}>Download</Button
							>
							<Tooltip
								>Downloads selected files on disk and adds them to the automatic downloads list.
							</Tooltip>
						{/if}
						{#if $fetchIncludeList.includes($selectedFile.path) || $selectedFile.fileType === FileType.Directory}
							<Button
								class="w-full"
								color="primary"
								on:click={() => handleUnFavoriteFile($selectedFile)}>Unfavorite</Button
							>
							<Tooltip>Removes any favorited files from the automatic downloads list.</Tooltip>
						{/if}
						{#if $selectedFile.lfsState === LocalFileLFSState.Local && $selectedFile.lockInfo?.ours}
							<Button disabled={loading} on:click={unlockSelectedFile}>Unlock File</Button>
						{:else if $selectedFile.lfsState === LocalFileLFSState.Local && !$selectedFile.locked}
							<Button disabled={loading} on:click={lockSelectedFile}>Lock File</Button>
						{/if}
					</div>
				{/if}
			</Card>
		</div>
	</div>
	<Card
		class="w-full p-4 sm:p-4 max-w-full max-h-[30vh] dark:bg-secondary-600 border-0 shadow-none overflow-auto"
	>
		{#if loadingFileHistory}
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
