<script lang="ts">
	import {
		Breadcrumb,
		BreadcrumbItem,
		Button,
		Card,
		Checkbox,
		Input,
		Modal,
		Select,
		Spinner,
		Table,
		TableBody,
		TableBodyCell,
		TableBodyRow
	} from 'flowbite-svelte';
	import { onMount, tick } from 'svelte';
	import { CheckSolid, CloseSolid, EditOutline, FolderSolid } from 'flowbite-svelte-icons';
	import { emit, listen } from '@tauri-apps/api/event';
	import { CommitTable, ProgressModal } from '@ethos/core';
	import { get } from 'svelte/store';
	import {
		downloadLFSFiles,
		getAllFiles,
		getFileHistory,
		getFiles,
		lockFiles,
		showCommitFiles,
		unlockFiles,
		verifyLocks
	} from '$lib/repo';
	import {
		type Commit,
		type DirectoryMetadata,
		FileType,
		type LFSFile,
		LocalFileLFSState,
		type Nullable
	} from '$lib/types';
	import { appConfig, currentRoot, currentRootFiles, enableGlobalSearch, locks } from '$lib/stores';
	import { openUrl } from '$lib/utils';
	import CharacterCard from '$lib/components/metadata/CharacterCard.svelte';
	import {
		getDirectoryMetadata,
		updateDirectoryMetadata,
		updateMetadataClass
	} from '$lib/metadata';

	let loading = false;
	let allFiles: string[] = [];
	let selectedFile: Nullable<LFSFile> = null;
	let downloadInProgress: boolean = false;
	let search: string = '';
	let showSearchModal: boolean = false;
	let searchInput: HTMLInputElement;
	let modalLoading: boolean = false;
	let selectedFiles: LFSFile[] = [];
	let shiftHeld = false;

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
		loadingFileHistory = true;
		commits = await getFileHistory(`${$currentRoot}/${selectedFile?.name}`);
		loadingFileHistory = false;
	};

	const handleFileToggled = (selected: LFSFile) => {
		// if ctrl is held, select or unselect everything in between
		if (shiftHeld) {
			const currentIndex = $currentRootFiles.findIndex((file) => file.name === selected.name);
			const lastSelectedIndex = $currentRootFiles.findIndex(
				(file) => selectedFiles[selectedFiles.length - 1].name === file.name
			);

			if (currentIndex > lastSelectedIndex) {
				for (let i = lastSelectedIndex + 1; i <= currentIndex; i += 1) {
					if (!selectedFiles.includes($currentRootFiles[i])) {
						selectedFiles = [...selectedFiles, $currentRootFiles[i]];
					} else {
						selectedFiles = selectedFiles.filter((item) => item.name !== $currentRootFiles[i].name);
					}
				}
			} else {
				for (let i = currentIndex; i < lastSelectedIndex; i += 1) {
					if (!selectedFiles.includes($currentRootFiles[i])) {
						selectedFiles = [...selectedFiles, $currentRootFiles[i]];
					} else {
						selectedFiles = selectedFiles.filter((item) => item.name !== $currentRootFiles[i].name);
					}
				}
			}

			// if we're unchecking, include the last selected file as well
			if (!selectedFiles.includes(selected)) {
				selectedFiles = selectedFiles.filter(
					(item) => item.name !== $currentRootFiles[lastSelectedIndex].name
				);
			}

			return;
		}

		if (!selectedFiles.includes(selected)) {
			selectedFiles = [...selectedFiles, selected];
		} else {
			selectedFiles = selectedFiles.filter((item) => item.name !== selected.name);
		}
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
		} catch (e) {
			await emit('error', e);
		}

		loading = false;
	};

	const downloadFiles = async (paths: string[]) => {
		downloadInProgress = true;

		try {
			await downloadLFSFiles(paths);
		} catch (e) {
			await emit('error', e);
		}

		downloadInProgress = false;

		await refreshFiles();
	};

	const handleDownloadFile = async (selected: Nullable<LFSFile>) => {
		if (selected === null || selectedFile === null) return;
		const fullPath = `${$currentRoot}/${selected.name}`;

		try {
			await downloadFiles([fullPath]);

			selectedFile.lfsState = LocalFileLFSState.Local;
		} catch (e) {
			await emit('error', e);
		}
	};

	const handleDownloadSelectedFiles = async () => {
		if (selectedFiles.length === 0) return;

		const paths = selectedFiles.map((file) => `${$currentRoot}/${file.name}`);

		try {
			await downloadFiles(paths);

			selectedFiles = [];
		} catch (e) {
			await emit('error', e);
		}
	};

	const goHome = async () => {
		$currentRoot = '';
		selectedFile = null;
		selectedFiles = [];
		await refreshFiles();
	};

	const goBack = async (index: number) => {
		ancestry = ancestry.slice(0, index + 1);
		$currentRoot = ancestry.join('/');
		selectedFile = null;
		selectedFiles = [];
		await refreshFiles();
	};

	const setCurrentRoot = async (root: string) => {
		const currRoot = get(currentRoot);
		$currentRoot = currRoot === '' ? root : `${$currentRoot}/${root}`;
		selectedFile = null;
		selectedFiles = [];
		await refreshFiles();
	};

	const selectFile = async (file: LFSFile) => {
		selectedFile = file;

		await handleShowFileHistory();
	};

	const lockSelectedFile = async () => {
		if (selectedFile === null) return;

		loading = true;

		const fullPath = `${$currentRoot}/${selectedFile.name}`;

		try {
			await lockFiles([fullPath]);

			$locks = await verifyLocks();
		} catch (e) {
			await emit('error', e);
		}

		await refreshFiles();
		selectedFile = $currentRootFiles.find((f) => f.name === selectedFile?.name) ?? null;

		loading = false;
	};

	const unlockSelectedFile = async () => {
		if (selectedFile === null) return;

		loading = true;

		const fullPath = `${$currentRoot}/${selectedFile.name}`;

		try {
			await unlockFiles([fullPath], false);

			$locks = await verifyLocks();
		} catch (e) {
			await emit('error', e);
		}

		await refreshFiles();
		selectedFile = $currentRootFiles.find((f) => f.name === selectedFile?.name) ?? null;

		loading = false;
	};

	const showInExplorer = async (file: Nullable<LFSFile>) => {
		if (file === null) return;

		const directory = `${$appConfig.repoPath}/${$currentRoot}`;

		await openUrl(`${directory}`);
	};

	const getLockOwner = (selected: LFSFile): string => {
		if (!selected.locked || selected.lockInfo === null) return 'None';
		return selected.lockInfo.lock.owner?.name ?? 'None';
	};

	const onKeyDown = async (event: KeyboardEvent) => {
		if (event.key === 'Shift') {
			shiftHeld = true;
			return;
		}

		if (!$enableGlobalSearch) return;

		if (search === '' && event.key.match(/^[a-z]$/) && $appConfig.repoPath !== '') {
			showSearchModal = true;

			await tick();
			searchInput.focus();
		}
	};

	const onKeyUp = (e: KeyboardEvent) => {
		if (e.key === 'Shift') {
			shiftHeld = false;
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

	void listen('refresh-files', () => {
		void refreshFiles();
	});

	onMount(() => {
		void refreshFiles();

		// refresh every 30 seconds
		const interval = setInterval(() => {
			void refreshFiles();
		}, 30000);

		return () => clearInterval(interval);
	});
</script>

<svelte:window on:keydown={onKeyDown} on:keyup={onKeyUp} />
<div class="flex flex-col h-full gap-2">
	<div class="flex items-baseline gap-2">
		<p class="text-2xl mt-2 dark:text-primary-400">Asset Explorer</p>
		<span class="text-sm text-gray-300 italic tracking-wide"
			>(Start typing to search all files!)</span
		>
		{#if loading}
			<Spinner class="w-4 h-4 dark:text-gray-500 fill-white" />
		{/if}
	</div>
	<div class="overflow-x-auto overflow-y-hidden py-1 h-8 min-h-[2rem]">
		<Breadcrumb
			aria-label="File ancestry"
			olClass="inline-flex items-center space-x-1 rtl:space-x-reverse rtl:space-x-reverse"
		>
			<BreadcrumbItem
				homeClass="inline-flex items-center text-sm font-medium text-gray-700 hover:text-gray-900 dark:text-gray-400 dark:hover:text-gray-400"
				home
				><Button
					outline
					size="xs"
					class="mx-0 py-1 dark:focus-within:ring-0"
					on:click={async () => goHome()}>/</Button
				></BreadcrumbItem
			>
			{#each ancestry as path, i}
				<BreadcrumbItem spanClass="text-sm font-medium text-gray-500 dark:text-gray-400"
					><Button
						outline
						size="xs"
						class="py-1 mx-0 dark:focus-within:ring-0"
						on:click={async () => goBack(i)}>{path}</Button
					></BreadcrumbItem
				>
			{/each}
		</Breadcrumb>
	</div>
	{#if selectedDirectoryClass === 'character' && directoryMetadata}
		<CharacterCard metadata={directoryMetadata} onMetadataSaved={handleUpdateDirectoryMetadata} />
	{/if}
	<div class="flex gap-2 overflow-hidden w-full max-w-full max-h-[70vh]">
		<div class="flex flex-col h-full min-h-full w-full">
			<Card
				class="w-full p-4 sm:p-4 h-full max-w-full dark:bg-secondary-600 border-0 shadow-none overflow-auto"
			>
				{#if loading && $currentRootFiles.length === 0}
					<Spinner class="w-12 h-12 dark:text-gray-500 fill-white" />
				{:else if $currentRootFiles.length === 0}
					<p class="text-center text-gray-500 dark:text-gray-400">No files found</p>
				{:else}
					<div class="flex flex-col gap-2 w-full">
						<Table>
							<TableBody>
								{#each $currentRootFiles as file, index}
									<TableBodyRow
										class="text-left border-b-0 {index % 2 === 0
											? 'bg-secondary-700 dark:bg-space-900'
											: 'bg-secondary-800 dark:bg-space-950'}"
									>
										<TableBodyCell class="p-1 w-8">
											<Checkbox
												class="!p-1.5 mr-0"
												checked={selectedFiles.some((selected) => selected.name === file.name)}
												on:change={() => {
													handleFileToggled(file);
												}}
											/>
										</TableBodyCell>
										<TableBodyCell class="p-2">
											{#if file.fileType === FileType.Directory}
												<Button
													outline
													disabled={loading}
													class="flex justify-start items-center py-0.5 pl-2 border-0 w-full"
													on:click={async () => {
														await setCurrentRoot(file.name);
													}}><FolderSolid class="h-6 w-6 pr-2" />{file.name}</Button
												>
											{:else}
												<div class="flex gap-2 items-center justify-start w-full">
													<Button
														outline
														class="justify-start border-0 py-0.5 pl-2 rounded-md w-full"
														on:click={() => selectFile(file)}
													>
														<div class="w-3 mr-3">{file.locked ? '🔒' : ''}</div>
														{file.name}
													</Button>
												</div>
											{/if}
										</TableBodyCell>
									</TableBodyRow>
								{/each}
							</TableBody>
						</Table>
					</div>
				{/if}
			</Card>
		</div>
		<div class="flex flex-col h-full min-w-[26rem] gap-2">
			{#if selectedFiles.length > 0}
				<Card
					class="w-full p-4 sm:p-4 max-w-full max-h-full dark:bg-secondary-600 border-0 shadow-none"
				>
					<Button class="w-full" on:click={handleDownloadSelectedFiles}
						>Download Selected Files</Button
					>
				</Card>
			{/if}
			<Card
				class="w-full h-10 p-4 sm:p-4 max-w-full max-h-full dark:bg-secondary-600 border-0 shadow-none"
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
								on:click={saveDirectoryClass}><CheckSolid class="w-4 h-4" /></Button
							>
							<Button
								disabled={updatingDirectoryClass}
								size="xs"
								class="my-1 dark:bg-red-800 hover:dark:bg-red-900"
								on:click={cancelEditDirectoryClass}><CloseSolid class="w-4 h-4" /></Button
							>
						</div>
					{:else}
						<div class="flex gap-2">
							<code class="dark:bg-secondary-700 px-2 py-1 w-32 text-center text-white"
								>{selectedDirectoryClass}</code
							>
							<Button size="xs" on:click={handleEditDirectoryClass}
								><EditOutline class="w-4 h-4" /></Button
							>
						</div>
					{/if}
				</div>
			</Card>
			<Card
				class="flex flex-col w-full h-full p-4 sm:p-4 max-w-full max-h-full dark:bg-secondary-600 border-0 shadow-none overflow-hidden"
			>
				<div class="flex items-center gap-2">
					<p class="text-xl my-2 dark:text-primary-400">File Details</p>
				</div>
				{#if selectedFile === null}
					<p class="text-gray-500 dark:text-gray-400 pb-4">No file selected.</p>
				{:else}
					<div class="flex flex-col gap-2 w-full h-full">
						<div class="w-full h-full">
							<div class="flex gap-2 w-full dark:text-white">
								<span class="w-20">Name:</span>
								<p class="dark:text-primary-400 w-64 break-all">{selectedFile.name}</p>
							</div>
							<div class="flex gap-2 w-full dark:text-white">
								<span class="w-20">Size:</span>
								<span class="dark:text-primary-400 w-64">{formatBytes(selectedFile.size)}</span>
							</div>
							<div class="flex gap-2 w-full dark:text-white">
								<span class="w-20">On disk:</span>
								<span class="dark:text-primary-400 w-64"
									>{selectedFile.lfsState === LocalFileLFSState.Stub ? 'No' : 'Yes'}</span
								>
							</div>
							<div class="flex gap-2 w-full dark:text-white">
								<span class="w-20">Locked by:</span>
								<span class="dark:text-primary-400 w-64">{getLockOwner(selectedFile)}</span>
							</div>
						</div>
						<Button on:click={() => showInExplorer(selectedFile)}>Show in Explorer</Button>
						{#if selectedFile.lfsState === LocalFileLFSState.Stub}
							<Button on:click={() => handleDownloadFile(selectedFile)}>Download</Button>
						{:else if selectedFile.lfsState === LocalFileLFSState.Local && selectedFile.lockInfo?.ours}
							<Button disabled={loading} on:click={unlockSelectedFile}>Unlock File</Button>
						{:else if selectedFile.lfsState === LocalFileLFSState.Local && !selectedFile.locked}
							<Button disabled={loading} on:click={lockSelectedFile}>Lock File</Button>
						{/if}
					</div>
				{/if}
			</Card>
		</div>
	</div>
	{#if selectedFile}
		<Card
			class="w-full p-4 sm:p-4 max-w-full max-h-[50vh] dark:bg-secondary-600 border-0 shadow-none overflow-auto"
		>
			{#if loadingFileHistory}
				<Spinner class="w-4 h-4 dark:text-gray-500 fill-white" />
			{:else}
				<CommitTable {commits} showFilesHandler={showCommitFiles} />
			{/if}
		</Card>
	{/if}
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
