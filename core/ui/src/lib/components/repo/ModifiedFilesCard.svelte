<script lang="ts">
	import {
		Alert,
		Button,
		Card,
		Checkbox,
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
		FileCopySolid
	} from 'flowbite-svelte-icons';
	import { ModifiedFileState, SubmitStatus, type ModifiedFile } from '$lib/types/index.js';

	export let disabled: boolean;
	export let modifiedFiles: ModifiedFile[];
	export let selectedFiles: ModifiedFile[];
	export let onSaveSnapshot: () => Promise<void> = async () => {};
	export let snapshotsEnabled = true;
	export let selectAll: boolean = false;
	export let onRevertFiles: (files: string[]) => Promise<void>;
	export let onOpenDirectory: (path: string) => Promise<void>;

	let showRevertConfirmation = false;
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

	const promptForRevertConfirmation = () => {
		showRevertConfirmation = true;
	};

	const closeRevertConfirmation = () => {
		showRevertConfirmation = false;
	};

	const handleFileToggled = (selectedFile: ModifiedFile) => {
		// if ctrl is held, select or unselect everything in between
		if (shiftHeld) {
			const currentIndex = modifiedFiles.findIndex((file) => file.path === selectedFile.path);
			const lastSelectedIndex = modifiedFiles.findIndex(
				(file) => selectedFiles[selectedFiles.length - 1].path === file.path
			);

			if (currentIndex > lastSelectedIndex) {
				for (let i = lastSelectedIndex + 1; i <= currentIndex; i += 1) {
					if (!selectedFiles.includes(modifiedFiles[i])) {
						selectedFiles = [...selectedFiles, modifiedFiles[i]];
					} else {
						selectedFiles = selectedFiles.filter((item) => item.path !== modifiedFiles[i].path);
					}
				}
			} else {
				for (let i = currentIndex; i < lastSelectedIndex; i += 1) {
					if (!selectedFiles.includes(modifiedFiles[i])) {
						selectedFiles = [...selectedFiles, modifiedFiles[i]];
					} else {
						selectedFiles = selectedFiles.filter((item) => item.path !== modifiedFiles[i].path);
					}
				}
			}

			// if we're unchecking, include the last selected file as well
			if (!selectedFiles.includes(selectedFile)) {
				selectedFiles = selectedFiles.filter(
					(item) => item.path !== modifiedFiles[lastSelectedIndex].path
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

	const handleSelectAllFiles = (e: Event) => {
		if ((e.target as HTMLInputElement).checked) {
			selectAll = true;
			selectedFiles = modifiedFiles.map((file) => file) ?? [];
		} else {
			selectAll = false;
			selectedFiles = [];
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
</script>

<svelte:window on:keydown={onKeyDown} on:keyup={onKeyup} />
<Card
	class="w-full p-4 sm:p-4 max-w-full bg-secondary-700 dark:bg-space-900 h-full overflow-y-hidden border-0 shadow-none"
>
	<div class="flex justify-between items-center gap-2 pb-2">
		<h3 class="text-primary-400 text-xl">Modified Files</h3>
		<div class="flex gap-2">
			<Button
				size="xs"
				disabled={disabled || selectedFiles.length === 0}
				on:click={promptForRevertConfirmation}
				>Revert Selected
			</Button>
			{#if snapshotsEnabled}
				<Button
					size="xs"
					disabled={disabled || selectedFiles.length === 0}
					on:click={onSaveSnapshot}
					>Save Snapshot
				</Button>
			{/if}
		</div>
	</div>
	<Table color="custom" striped={true}>
		<TableHead class="text-left border-b-0 p-2 bg-secondary-800 dark:bg-space-950">
			<TableHeadCell class="p-1 w-8">
				<Checkbox
					disabled={modifiedFiles.length === 0}
					class="!p-1.5"
					checked={selectAll}
					on:change={(e) => {
						handleSelectAllFiles(e);
					}}
				/>
				<Tooltip
					class="w-auto bg-secondary-700 dark:bg-space-900 font-semibold shadow-2xl"
					placement="right"
					>Select/deselect all
				</Tooltip>
			</TableHeadCell>
			<TableHeadCell class="p-1" />
			<TableHeadCell class="p-1">Checked Out</TableHeadCell>
			<TableHeadCell class="p-1">File</TableHeadCell>
		</TableHead>
		<TableBody>
			{#each modifiedFiles as file, index}
				<TableBodyRow
					class="text-left border-b-0 {index % 2 === 0
						? 'bg-secondary-700 dark:bg-space-900'
						: 'bg-secondary-800 dark:bg-space-950'}"
				>
					<TableBodyCell tdClass="p-1 w-8 whitespace-nowrap font-medium">
						<Checkbox
							class="!p-1.5"
							checked={selectedFiles.some((selectedFile) => selectedFile.path === file.path)}
							on:change={() => {
								handleFileToggled(file);
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
						{file.lockedBy}
					</TableBodyCell>
					<TableBodyCell class="p-1 flex gap-1 items-center h-full whitespace-nowrap font-medium">
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
							<Tooltip class="w-auto bg-secondary-600 dark:bg-space-800 font-semibold shadow-2xl">
								{getFileTooltip(file)}
							</Tooltip>
						{/if}
						<Button
							outline
							size="xs"
							class="p-0 w-full active:border-none focus:ring-0 dark:active:border-none dark:focus:ring-0 border-none justify-start items-center text-left {getFileTextClass(
								file
							)}
                                {index % 2 === 0
								? 'hover:bg-secondary-700 dark:hover:bg-space-900'
								: 'hover:bg-secondary-800 dark:hover:bg-space-950'}"
							on:click={() => handleFileToggled(file)}
						>
							{getFileDisplayString(file)}
						</Button>
						{#if file.displayName !== ''}
							<Tooltip class="w-auto bg-secondary-600 dark:bg-space-800 font-semibold shadow-2xl">
								{file.path}
							</Tooltip>
						{/if}
					</TableBodyCell>
				</TableBodyRow>
			{:else}
				<TableBodyRow class="text-center border-b-0 bg-secondary-700 dark:bg-space-900">
					<TableBodyCell class="p-1" colspan="3">
						<p class="text-gray-300">No modified files</p>
					</TableBodyCell>
				</TableBodyRow>
			{/each}
		</TableBody>
	</Table>
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
				<div class="flex gap-2 items-center">
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
