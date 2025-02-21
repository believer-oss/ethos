<script lang="ts">
	import {
		Alert,
		Button,
		Card,
		Checkbox,
		Input,
		Modal,
		Table,
		TableBody,
		TableBodyCell,
		TableBodyRow,
		TableHead,
		TableHeadCell,
		Tooltip
	} from 'flowbite-svelte';
	import {
		CloseCircleSolid,
		PenSolid,
		FolderOpenOutline,
		InfoCircleSolid,
		PlusOutline,
		FileCopySolid,
		TrashBinSolid,
		ChevronSortOutline,
		EditOutline
	} from 'flowbite-svelte-icons';
	import { onMount } from 'svelte';
	import {
		ModifiedFileState,
		SubmitStatus,
		SortKey,
		type ModifiedFile,
		type ChangeSet
	} from '$lib/types/index.js';

	export let disabled: boolean;
	export let modifiedFiles: ModifiedFile[];
	export let selectedFiles: ModifiedFile[];
	export let onSaveSnapshot: () => Promise<void> = async () => {};
	export let snapshotsEnabled = true;
	export let selectAll: boolean = false;
	export let onRevertFiles: (files: string[]) => Promise<void>;
	export let onOpenDirectory: (path: string) => Promise<void>;
	export let onLockSelected: () => Promise<void>;
	export let lockSelectedEnabled = true;
	export let changeSets: ChangeSet[];
	export let onChangesetsSaved: (changeSets: ChangeSet[]) => Promise<void>;
	export let enableGlobalSearch: boolean = false;
	export let onRightClick: (e: MouseEvent, file: ModifiedFile) => void = () => {};

	let hoveringSetIndex: number = -1;
	let hoveringCreateNewChangeset: boolean = false;
	let editingChangeSetIndex: number = -1;
	let editingChangeSetValue: string = '';
	let searchInput: string = '';

	// multi select
	let shiftHeld = false;

	const onKeyDown = (e: KeyboardEvent) => {
		if (e.key === 'Shift') {
			shiftHeld = true;
		}
	};

	const onKeyup = (e: KeyboardEvent) => {
		if (e.key === 'Shift') {
			shiftHeld = false;
		}
	};

	const isChecked = (files: ModifiedFile[]) =>
		selectedFiles.length > 0 &&
		files.every((file) => selectedFiles.some((selectedFile) => selectedFile.path === file.path));

	const isIndeterminate = (files: ModifiedFile[]) =>
		selectedFiles.length > 0 &&
		!isChecked(files) &&
		files.some((file) => selectedFiles.some((selectedFile) => selectedFile.path === file.path));

	// changeSetOrderedFiles is the list of modified files, but ordered the way they appear in the changeSets
	$: changeSetOrderedFiles = changeSets
		.map((changeSet) => changeSet.files.map((file) => file))
		.flat();

	let showRevertConfirmation = false;

	// Ensure there's a "default" changeset containing all unassigned modified files
	const ensureDefaultChangeset = () => {
		let defaultChangesetIndex = changeSets.findIndex((cs) => cs.name === 'default');

		if (defaultChangesetIndex === -1) {
			// If "default" changeset doesn't exist, create it
			changeSets = [
				...changeSets,
				{ name: 'default', files: [], open: true, checked: false, indeterminate: false }
			];
			defaultChangesetIndex = changeSets.length - 1;
		}

		// Get all files that are not in any changeset
		const unassignedFiles = modifiedFiles.filter(
			(file) => !changeSets.some((cs) => cs.files.some((f) => f.path === file.path))
		);

		// Add unassigned files to the "default" changeset
		changeSets[defaultChangesetIndex].files = [
			...changeSets[defaultChangesetIndex].files,
			...unassignedFiles
		];

		// Update changeSets
		changeSets = [...changeSets];
	};

	const isDeletable = (changeSetName: string): boolean =>
		changeSetName !== 'default' &&
		changeSets.find((cs) => cs.name === changeSetName)?.files.length === 0;

	// Make sure every file in every changeset is in modifiedFiles
	const cleanUpChangeSets = async () => {
		ensureDefaultChangeset();

		for (let i = 0; i < changeSets.length; i += 1) {
			// Keep only files that exist in modifiedFiles
			changeSets[i].files = changeSets[i].files.filter((file) =>
				modifiedFiles.some((mf) => mf.path === file.path)
			);
		}

		await onChangesetsSaved(changeSets);
	};

	// Set up a listener for changes to modifiedFiles
	$: if (modifiedFiles) {
		void cleanUpChangeSets();
	}

	const promptForRevertConfirmation = () => {
		showRevertConfirmation = true;
	};

	const closeRevertConfirmation = () => {
		showRevertConfirmation = false;
	};

	const handleFileToggled = (selectedFile: ModifiedFile) => {
		// if ctrl is held, select or unselect everything in between
		if (shiftHeld) {
			const currentIndex = changeSetOrderedFiles.findIndex(
				(file) => file.path === selectedFile.path
			);
			const lastSelectedIndex = changeSetOrderedFiles.findIndex(
				(file) => selectedFiles[selectedFiles.length - 1].path === file.path
			);

			if (currentIndex > lastSelectedIndex) {
				for (let i = lastSelectedIndex + 1; i <= currentIndex; i += 1) {
					if (!selectedFiles.includes(changeSetOrderedFiles[i])) {
						selectedFiles = [...selectedFiles, changeSetOrderedFiles[i]];
					} else {
						selectedFiles = selectedFiles.filter(
							(item) => item.path !== changeSetOrderedFiles[i].path
						);
					}
				}
			} else {
				for (let i = currentIndex; i < lastSelectedIndex; i += 1) {
					if (!selectedFiles.includes(changeSetOrderedFiles[i])) {
						selectedFiles = [...selectedFiles, changeSetOrderedFiles[i]];
					} else {
						selectedFiles = selectedFiles.filter(
							(item) => item.path !== changeSetOrderedFiles[i].path
						);
					}
				}
			}

			// if we're unchecking, include the last selected file as well
			if (!selectedFiles.includes(selectedFile)) {
				selectedFiles = selectedFiles.filter(
					(item) => item.path !== changeSetOrderedFiles[lastSelectedIndex].path
				);
			}

			return;
		}

		if (!selectedFiles.includes(selectedFile)) {
			selectedFiles = [...selectedFiles, selectedFile];
		} else {
			selectedFiles = selectedFiles.filter((item) => item.path !== selectedFile.path);
		}
	};

	const handleToggleAllFilesInChangeset = (changeSetIndex: number) => {
		// if not all files are selected, select all files in the changeset
		// if all files are selected, unselect all files in the changeset
		const changeSet = changeSets[changeSetIndex];
		const allFilesSelected = changeSet.files.every((file) =>
			selectedFiles.some((selectedFile) => selectedFile.path === file.path)
		);

		if (allFilesSelected) {
			selectedFiles = selectedFiles.filter(
				(file) => !changeSet.files.some((csFile) => csFile.path === file.path)
			);
		} else {
			// avoid duplicates
			selectedFiles = [
				...selectedFiles,
				...changeSet.files.filter(
					(file) => !selectedFiles.some((selectedFile) => selectedFile.path === file.path)
				)
			];
		}
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
		if (file.state === ModifiedFileState.Unknown) {
			return 'text-lime-500 dark:text-yellow-300';
		}

		return '';
	};

	const getFileDisplayString = (file: ModifiedFile): string => {
		if (file.displayName === '') {
			return file.path;
		}
		return file.displayName;
	};

	const getFileTooltip = (file: ModifiedFile): string => {
		let tooltip = '';
		if (file.submitStatus === SubmitStatus.CheckedOutByOtherUser) {
			tooltip = ': Unable to submit - checked out by other user';
		} else if (file.submitStatus === SubmitStatus.CheckoutRequired) {
			tooltip = ': Unable to submit - checkout required';
		} else if (file.submitStatus === SubmitStatus.Unmerged) {
			tooltip = ': Unable to submit - unmerged file requires a revert';
		} else if (file.submitStatus === SubmitStatus.Conflicted) {
			tooltip = ': Unable to submit - conflicted file requires a revert';
		}

		return file.state + tooltip;
	};

	const handleDragEnter = (_e: DragEvent, changeSetIndex: number) => {
		hoveringSetIndex = changeSetIndex;
		hoveringCreateNewChangeset = false;
	};

	const handleCreateNewChangesetDragEnter = (_e: DragEvent) => {
		hoveringSetIndex = -1;
		hoveringCreateNewChangeset = true;
	};

	const handleFileDragStart = (e: DragEvent, file: ModifiedFile) => {
		if (selectedFiles.length === 0) {
			e.dataTransfer?.setData('text/plain', JSON.stringify(file));
		}
	};

	const refreshCheckboxes = () => {
		changeSets = changeSets.map((changeSet) => ({
			...changeSet,
			checked: isChecked(changeSet.files),
			indeterminate: isIndeterminate(changeSet.files)
		}));
	};

	const handleCreateNewChangeset = async (e: DragEvent) => {
		e.preventDefault();

		let filesToMove: ModifiedFile[] = [];
		if (selectedFiles.length === 0) {
			const file = JSON.parse(e.dataTransfer?.getData('text/plain') ?? '{}');
			filesToMove = [file];
		} else {
			filesToMove = selectedFiles;
		}

		// Remove the file from all other changesets
		changeSets = changeSets.map((changeSet) => ({
			...changeSet,
			files: changeSet.files.filter((f) => !filesToMove.find((ftm) => ftm.path === f.path))
		}));

		// if there already exists a changeset named "New Changeset" rename the new one to "New Changeset(i)"
		let newName = 'New Changeset';
		if (changeSets.some((cs) => cs.name.startsWith('New Changeset'))) {
			let i = 1;
			newName = `New Changeset(${i})`;

			const isDuplicateName = (name: string) => changeSets.some((cs) => cs.name === name);

			while (isDuplicateName(newName)) {
				i += 1;
				newName = `New Changeset(${i})`;
			}
		}

		changeSets = [
			...changeSets,
			{
				name: newName,
				files: filesToMove,
				open: true,
				checked: false,
				indeterminate: false
			}
		];

		await onChangesetsSaved(changeSets);
		refreshCheckboxes();

		hoveringSetIndex = -1;
		hoveringCreateNewChangeset = false;
	};

	const handleDrop = async (e: DragEvent, changeSetIndex: number) => {
		e.preventDefault();

		let filesToMove: ModifiedFile[] = [];
		if (selectedFiles.length === 0) {
			const file = JSON.parse(e.dataTransfer?.getData('text/plain') ?? '{}');
			filesToMove = [file];
		} else {
			filesToMove = selectedFiles;
		}

		changeSets = changeSets.map((changeSet, index) => {
			if (index === changeSetIndex) {
				return {
					...changeSet,
					files: [
						...changeSets[changeSetIndex].files,
						...filesToMove.filter(
							(file) => !changeSets[changeSetIndex].files.some((f) => f.path === file.path)
						)
					]
				};
			}
			return {
				...changeSet,
				files: changeSet.files.filter((f) => !filesToMove.find((ftm) => ftm.path === f.path)),
				checked: false,
				indeterminate: false
			};
		});

		await onChangesetsSaved(changeSets);
		refreshCheckboxes();

		hoveringSetIndex = -1;
		hoveringCreateNewChangeset = false;
	};

	// sorting and filtering
	let sortDirection: number = 1;
	let sortKey: SortKey = SortKey.FileName;
	let sortFunction = (a: ModifiedFile, b: ModifiedFile) =>
		sortDirection * getFileDisplayString(a).localeCompare(getFileDisplayString(b));
	$: filteredSortedChangeSets = changeSets.map((changeSet) => {
		if (searchInput.length < 3) {
			return {
				...changeSet,
				files: [...changeSet.files].sort(sortFunction)
			};
		}

		return {
			...changeSet,
			files: changeSet.files
				.filter(
					(file) =>
						file.path.toLowerCase().includes(searchInput.toLowerCase()) ||
						file.displayName.toLowerCase().includes(searchInput.toLowerCase())
				)
				.sort(sortFunction)
		};
	});

	const changeSortDirection = (newSortKey: SortKey) => {
		if (newSortKey === sortKey) {
			sortDirection = -sortDirection;
		} else {
			sortDirection = 1;
			sortKey = newSortKey;
		}
	};

	onMount(async () => {
		await cleanUpChangeSets();
	});
</script>

<svelte:window
	on:drop|preventDefault
	on:dragover|preventDefault
	on:keydown={onKeyDown}
	on:keyup={onKeyup}
/>
<Card
	class="w-full relative p-4 sm:p-4 max-w-full bg-secondary-700 dark:bg-space-900 h-full overflow-y-hidden border-0 shadow-none"
>
	<div class="flex justify-between items-center gap-2 pb-2">
		<h3 class="text-primary-400 text-xl">Modified Files</h3>
		<div class="flex gap-2">
			{#if lockSelectedEnabled}
				<Button
					size="xs"
					disabled={disabled || selectedFiles.length === 0}
					on:click={onLockSelected}
					>Lock Selected
				</Button>
			{/if}
			<Button
				size="xs"
				disabled={disabled || selectedFiles.length === 0}
				on:click={promptForRevertConfirmation}
				>Revert Selected
			</Button>
			{#if snapshotsEnabled}
				<Button size="xs" disabled={modifiedFiles.length === 0} on:click={onSaveSnapshot}
					>Save Snapshot {selectedFiles.length > 0 ? `(${selectedFiles.length})` : '(all)'}
				</Button>
			{/if}
		</div>
	</div>
	<div class="flex gap-2 pb-1">
		<Input
			class="w-full h-8 text-white bg-secondary-800 dark:bg-space-950"
			bind:value={searchInput}
			placeholder="Filter files"
		/>
	</div>
	<div class="overflow-y-auto pr-1 mb-16">
		{#each filteredSortedChangeSets as changeSet, index}
			<div
				on:dragover|preventDefault
				on:dragenter|preventDefault={(e) => {
					handleDragEnter(e, index);
				}}
				on:drop|preventDefault={(e) => handleDrop(e, index)}
				class="rounded-md p-1 {hoveringSetIndex === index
					? 'border border-primary-500'
					: 'border-0'}"
				role="button"
				tabindex="0"
			>
				<div class="flex gap-2 mb-1 items-center justify-between w-full text-white cursor-default">
					{#if editingChangeSetIndex === index}
						<Input
							bind:value={editingChangeSetValue}
							on:keydown={async (e) => {
								if (e.key === 'Enter') {
									changeSets[editingChangeSetIndex] = {
										...changeSet,
										name: editingChangeSetValue
									};
									editingChangeSetIndex = -1;
									enableGlobalSearch = true;
									await onChangesetsSaved(changeSets);
								}
							}}
							class="w-full h-8 text-white bg-secondary-800 dark:bg-space-950"
						/>
					{:else}
						<div class="flex gap-1">
							{#if changeSet.files.length > 0}
								<Checkbox
									class="align-middle"
									disabled={isDeletable(changeSet.name)}
									checked={changeSet.checked}
									indeterminate={changeSet.indeterminate}
									on:click={() => {
										handleToggleAllFilesInChangeset(index);
										changeSets[index] = {
											...changeSet,
											checked: isChecked(changeSet.files),
											indeterminate: isIndeterminate(changeSet.files)
										};
									}}
								/>

								<style>
									/* Override indeterminate checkbox icons with a minus, since current version of flowbite ("^0.44.18") doesn't set the icon properly. Maybe could be fixed with an update. */
									[type='checkbox']:indeterminate {
										background-image: url("data:image/svg+xml,%3csvg aria-hidden='true' xmlns='http://www.w3.org/2000/svg' fill='none' viewBox='0 0 16 12'%3e %3cpath stroke='white' stroke-linecap='round' stroke-linejoin='round' stroke-width='3' d='M0.5 6h14'/%3e %3c/svg%3e");
									}
								</style>

								<Tooltip>Select All</Tooltip>
							{/if}
							<span class="flex items-center gap-1"
								>{changeSet.name}
								<span class="text-xs text-gray-400 font-italic">({changeSet.files.length})</span>
							</span>
							{#if changeSet.name !== 'default'}
								<Button
									class="p-1 px-2 w-auto h-auto"
									color="primary"
									on:click={() => {
										editingChangeSetIndex = index;
										editingChangeSetValue = changeSet.name;
										enableGlobalSearch = false;
									}}
								>
									<EditOutline class="w-4 h-4" />
								</Button>
								<Tooltip>Rename</Tooltip>
							{/if}
							{#if isDeletable(changeSet.name)}
								<Button
									color="red"
									on:click={async () => {
										changeSets = changeSets.filter((_, i) => i !== index);
										await onChangesetsSaved(changeSets);
									}}
									class="p-1 px-2 w-auto h-auto"
								>
									<TrashBinSolid class="w-4 h-4" />
								</Button>
								<Tooltip>Delete</Tooltip>
							{/if}
						</div>
						<div class="flex gap-1">
							<Button
								outline
								color="primary"
								class="py-0.5 px-1"
								on:click={() => {
									changeSet.open = !changeSet.open;
								}}
							>
								{#if changeSet.open}
									<ChevronSortOutline class="w-3 h-3" />
								{:else}
									<PlusOutline class="w-3 h-3" />
								{/if}
							</Button>
						</div>
					{/if}
				</div>
				{#if changeSet.files.length > 0 && changeSet.open}
					<Table color="custom" striped={true}>
						<TableHead class="w-full border-b-0 p-2 bg-secondary-800 dark:bg-space-950">
							<TableHeadCell />
							<TableHeadCell
								class="p-1 cursor-pointer gap-2"
								on:click={() => {
									changeSortDirection(SortKey.FileState);
									sortFunction = (a, b) => sortDirection * a.state.localeCompare(b.state);
								}}
							>
								<div class="flex items-center">
									<ChevronSortOutline class="text-gray-400" size="xs" />
								</div>
							</TableHeadCell>
							<TableHeadCell
								class="p-1 cursor-pointer"
								on:click={() => {
									changeSortDirection(SortKey.LockStatus);
									sortFunction = (a, b) => sortDirection * a.lockedBy.localeCompare(b.lockedBy);
								}}
							>
								<div class="flex flex-row w-min items-center gap-2">
									Locks
									<ChevronSortOutline class="text-gray-400" size="xs" />
								</div>
							</TableHeadCell>
							<TableHeadCell
								class="p-1 cursor-pointer"
								on:click={() => {
									changeSortDirection(SortKey.FileName);
									sortFunction = (a, b) =>
										sortDirection * getFileDisplayString(a).localeCompare(getFileDisplayString(b));
								}}
							>
								<div class="flex items-center gap-2">
									File Name
									<ChevronSortOutline class="text-gray-400" size="xs" />
								</div>
							</TableHeadCell>
						</TableHead>
						<TableBody>
							{#each changeSet.files as file, fileIndex}
								<TableBodyRow
									on:contextmenu={(e) => {
										onRightClick(e, file);
									}}
									class="text-left border-b-0 {fileIndex % 2 === 0
										? 'bg-secondary-800 dark:bg-space-950'
										: 'bg-secondary-700 dark:bg-space-900'}"
								>
									<TableBodyCell tdClass="p-1 w-8 whitespace-nowrap font-medium">
										<Checkbox
											class="!p-1.5"
											checked={selectedFiles.some(
												(selectedFile) => selectedFile.path === file.path
											)}
											on:change={() => {
												handleFileToggled(file);
												changeSets[index] = {
													...changeSet,
													checked: isChecked(changeSet.files),
													indeterminate: isIndeterminate(changeSet.files)
												};
											}}
										/>
									</TableBodyCell>
									<TableBodyCell tdClass="p-1 w-8">
										{#if file.state === ModifiedFileState.Added}
											<PlusOutline class="w-4 h-4 text-lime-500" />
											<Tooltip
												class="w-auto bg-secondary-600 dark:bg-space-800 font-semibold shadow-2xl"
												placement="right"
												>{getFileTooltip(file)}
											</Tooltip>
										{:else if file.state === ModifiedFileState.Modified}
											<PenSolid class="w-4 h-4 text-yellow-300" />
											<Tooltip
												class="w-auto bg-secondary-600 dark:bg-space-800 font-semibold shadow-2xl"
												placement="right"
												>{getFileTooltip(file)}
											</Tooltip>
										{:else if file.state === ModifiedFileState.Deleted}
											<CloseCircleSolid class="w-4 h-4 text-red-700" />
											<Tooltip
												class="w-auto bg-secondary-600 dark:bg-space-800 font-semibold shadow-2xl"
												placement="right"
												>{getFileTooltip(file)}
											</Tooltip>
										{:else if file.state === ModifiedFileState.Unmerged}
											<FileCopySolid class="w-4 h-4 text-red-700" />
											<Tooltip
												class="w-auto bg-secondary-600 dark:bg-space-800 font-semibold shadow-2xl"
												placement="right"
												>{getFileTooltip(file)}
											</Tooltip>
										{/if}
									</TableBodyCell>
									<TableBodyCell tdClass="p-1 w-8 whitespace-nowrap font-medium">
										{#if file.lockedBy !== ''}
											{file.lockedBy}
										{:else}
											<span class="text-gray-300">Unlocked</span>
										{/if}
									</TableBodyCell>
									<TableBodyCell
										class="p-1 flex gap-1 items-center h-full whitespace-nowrap font-medium"
									>
										<Button
											outline
											size="xs"
											class="p-1 border-0 focus-within:ring-0 dark:focus-within:ring-0"
											on:click={async () => onOpenDirectory(file.path)}
										>
											<FolderOpenOutline class="w-4 h-4" />
										</Button>
										{#if file.submitStatus !== SubmitStatus.Ok}
											<span> ⚠️ </span>
											<Tooltip
												class="w-auto bg-secondary-600 dark:bg-space-800 font-semibold shadow-2xl"
											>
												{getFileTooltip(file)}
											</Tooltip>
										{/if}
										<div
											draggable={true}
											role="button"
											tabindex="0"
											class="p-0 w-full justify-start items-center text-left {getFileTextClass(
												file
											)}"
											on:dragstart={(e) => {
												handleFileDragStart(e, file);
											}}
										>
											{getFileDisplayString(file)}
										</div>
										{#if file.displayName !== ''}
											<Tooltip
												class="w-auto bg-secondary-600 dark:bg-space-800 font-semibold shadow-2xl"
											>
												{file.path}
											</Tooltip>
										{/if}
									</TableBodyCell>
								</TableBodyRow>
							{:else}
								<TableBodyRow class="text-center border-b-0 bg-secondary-700 dark:bg-space-900">
									<TableBodyCell class="p-1" colspan="4">
										<p class="text-gray-300">No modified files</p>
									</TableBodyCell>
								</TableBodyRow>
							{/each}
						</TableBody>
					</Table>
				{/if}
			</div>
		{/each}
	</div>
	<div
		class="absolute bottom-0 left-0 w-full p-4 rounded-b-lg border-t border-primary-500 bg-secondary-700 dark:bg-space-900"
	>
		<div
			on:dragover|preventDefault
			on:dragenter|preventDefault={(e) => {
				handleCreateNewChangesetDragEnter(e);
			}}
			on:drop|preventDefault={(e) => handleCreateNewChangeset(e)}
			class="rounded-md p-1 {hoveringCreateNewChangeset
				? 'border border-primary-500 border-solid'
				: 'border border-gray-300 border-dotted'}"
			role="button"
			tabindex="0"
		>
			<div class="text-white whitespace-nowrap">drag file(s) here to create a new group</div>
		</div>
	</div>
</Card>

<Modal
	class="bg-secondary-700 dark:bg-space-900"
	bodyClass="!border-t-0 flex-1 overflow-y-auto overscroll-contain"
	backdropClass="fixed mt-8 inset-0 z-40 bg-gray-900 bg-opacity-50 dark:bg-opacity-80"
	dialogClass="fixed mt-8 top-0 start-0 end-0 h-modal md:inset-0 md:h-full z-50 w-full p-4 pb-12 flex"
	open={showRevertConfirmation}
	dismissable={false}
>
	<div class="flex flex-col h-full gap-2">
		<p class="text-lg font-semibold text-gray-300">
			Are you sure you want to revert the selected files?
		</p>
		<Alert class="bg-secondary-800 dark:bg-space-950 my-2 py-2 text-white dark:text-white">
			<InfoCircleSolid slot="icon" class="w-4 h-4" />
			Warning: Reverting <span class="font-bold">newly added</span> files will delete them from your
			computer.
		</Alert>
		<div
			class="bg-secondary-800 dark:bg-space-950 p-2 h-full w-full text-white overflow-auto text-nowrap"
		>
			{#each selectedFiles as file}
				<div class="flex gap-2 items-center" role="listitem">
					{#if file.state === ModifiedFileState.Added}
						<PlusOutline class="w-4 h-4 text-lime-500" />
						<Tooltip
							class="w-auto bg-secondary-600 dark:bg-space-800 font-semibold shadow-2xl"
							placement="right"
							>{getFileTooltip(file)}
						</Tooltip>
					{:else if file.state === ModifiedFileState.Modified}
						<PenSolid class="w-4 h-4 text-yellow-300" />
						<Tooltip
							class="w-auto bg-secondary-600 dark:bg-space-800 font-semibold shadow-2xl"
							placement="right"
							>{getFileTooltip(file)}
						</Tooltip>
					{:else if file.state === ModifiedFileState.Deleted}
						<CloseCircleSolid class="w-4 h-4 text-red-700" />
						<Tooltip
							class="w-auto bg-secondary-600 dark:bg-space-800 font-semibold shadow-2xl"
							placement="right"
							>{getFileTooltip(file)}
						</Tooltip>
					{:else if file.state === ModifiedFileState.Unmerged}
						<p class="w-4 h-4 mb-2 text-red-700">⚠️</p>
						<Tooltip
							class="w-auto bg-secondary-600 dark:bg-space-800 font-semibold shadow-2xl"
							placement="right"
							>{getFileTooltip(file)}
						</Tooltip>
					{/if}
					<p class="font-bold {getFileTextClass(file)}">
						{getFileDisplayString(file)}
					</p>
					{#if file.displayName !== ''}
						<Tooltip class="w-auto bg-secondary-600 dark:bg-space-800 font-semibold shadow-2xl">
							{file.path}
						</Tooltip>
					{/if}
				</div>
			{/each}
		</div>
		<div class="flex justify-end gap-2">
			<Button
				size="sm"
				on:click={async () => {
					closeRevertConfirmation();
					await onRevertFiles(selectedFiles.map((file) => file.path));
					selectAll = false;
				}}
				>Yes
			</Button>
			<Button size="sm" on:click={closeRevertConfirmation}>No</Button>
		</div>
	</div>
</Modal>
