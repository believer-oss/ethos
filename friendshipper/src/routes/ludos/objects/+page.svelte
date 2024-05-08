<script lang="ts">
	import {
		Button,
		Card,
		Table,
		TableBody,
		TableBodyCell,
		TableBodyRow,
		TableHead,
		TableHeadCell,
		Input,
		Textarea,
		Spinner
	} from 'flowbite-svelte';
	import { onMount } from 'svelte';
	import { emit } from '@tauri-apps/api/event';
	import {
		FolderOpenOutline,
		FileOutline,
		ArrowUpOutline,
		RotateOutline,
		TrashBinOutline
	} from 'flowbite-svelte-icons';

	import { ludosGet, ludosPut, ludosList, ludosDelete } from '$lib/ludos';
	import type { LudosGetResponse, LudosListResponse } from '$lib/types';
	import { appConfig } from '$lib/stores';
	import ConfirmModal from '$lib/components/ConfirmModal.svelte';

	interface LudosObject {
		name: string;
		path: string;
		isObject: boolean;
	}

	let inAsyncOperation: boolean = false;
	let currentPath = '';
	let objectsAtCurrent: LudosListResponse[] = [];
	let filteredObjects: LudosObject[] = [];

	let selectedObjectPath: string = '';
	let selectedObject: LudosGetResponse = null;
	let selectedObjectJson: string = '';
	let selectedObjectJsonEdit: string = '';
	let selectedObjectSaveInProgress: boolean = false;

	let showDeleteConfirm: boolean = false;
	let confirmTitleString: string = 'Delete this object?';

	const basename = (path: string) => {
		const index = path.lastIndexOf('/');
		if (index !== -1) {
			return path.substr(index + 1);
		}
		return path;
	};

	const dirname = (path: string) => {
		const index = path.lastIndexOf('/');
		if (index !== -1) {
			return path.substr(0, index);
		}
		return '';
	};

	const rootname = (path: string) => {
		const index = path.indexOf('/');
		if (index !== -1) {
			return path.substr(0, index);
		}
		return path;
	};

	const relativename = (root: string, path: string) => {
		const index = path.indexOf(root);
		if (index !== -1) {
			const length = path.charAt(root.length) === '/' ? root.length + 1 : root.length;
			return path.substr(length);
		}
		return path;
	};

	const refreshLocal = () => {
		filteredObjects = [];
		for (const object of objectsAtCurrent) {
			if (object.key.startsWith(currentPath)) {
				const relativeName = relativename(currentPath, object.key);
				const baseName = basename(object.key);
				const isObject = baseName === relativeName;

				if (isObject) {
					filteredObjects.push({
						name: basename(object.key),
						path: object.key,
						isObject
					});
				} else {
					const relativeRootName = rootname(relativeName);
					const existingEntry = filteredObjects.find((item) => relativeRootName === item.name);
					// don't allow duplicate entries
					if (existingEntry === undefined) {
						filteredObjects.push({
							path: object.key,
							name: relativeRootName,
							isObject
						});
					}
				}
			}
		}

		filteredObjects.sort((a, b) => {
			if (a.isObject === b.isObject) {
				return a.path.toLowerCase() < b.path.toLowerCase() ? -1 : 1;
			}
			return a.isObject ? 1 : -1;
		});
	};

	const refresh = async () => {
		inAsyncOperation = true;
		try {
			const response = await ludosList('');
			objectsAtCurrent = response.objects;
		} catch (e) {
			objectsAtCurrent = [];
			if (!e.message.includes('Objects not found for listing')) {
				await emit('error', e);
			}
		}
		refreshLocal();
		inAsyncOperation = false;
	};

	const refreshAndResetPaths = async () => {
		currentPath = '';
		selectedObjectPath = '';
		selectedObject = null;
		selectedObjectJson = '';
		selectedObjectJsonEdit = '';
		selectedObjectSaveInProgress = false;

		await refresh();
	};

	const handleUpDir = () => {
		currentPath = dirname(currentPath);

		selectedObjectPath = '';
		selectedObject = null;
		selectedObjectJson = '';
		selectedObjectJsonEdit = '';

		refreshLocal();
	};

	const handleOpenFolder = (name: string) => {
		if (currentPath === '') {
			currentPath = name;
		} else {
			currentPath = `${currentPath}/${name}`;
		}
		refreshLocal();
	};

	const handleViewObject = async (obj: LudosObject) => {
		inAsyncOperation = true;
		currentPath = obj.path;
		selectedObjectPath = currentPath;

		try {
			selectedObject = await ludosGet(obj.path);
			selectedObjectJson = JSON.stringify(JSON.parse(selectedObject.data), null, 2); // prettify
			selectedObjectJsonEdit = selectedObjectJson;
		} catch (e) {
			selectedObjectPath = '';
			selectedObject = null;
			selectedObjectJson = '';
			selectedObjectJsonEdit = '';
			await emit('error', e);
		}
		inAsyncOperation = false;
	};

	const handleDeleteObject = () => {
		showDeleteConfirm = true;
	};

	let handleDeleteConfirm = async (result: boolean) => {
		showDeleteConfirm = false;
		if (result) {
			try {
				await ludosDelete([selectedObjectPath]);
				await refresh();

				selectedObjectPath = '';
				currentPath = dirname(currentPath);

				refreshLocal();
			} catch (e) {
				await emit('error', e);
			}
		}
	};

	const handleObjectSaveClicked = async () => {
		let unprettyJson = '';
		try {
			unprettyJson = JSON.stringify(JSON.parse(selectedObjectJsonEdit));
		} catch (e) {
			await emit('error', { message: e.toString() });
			return;
		}

		selectedObjectSaveInProgress = true;
		try {
			await ludosPut(selectedObjectPath, unprettyJson);
			selectedObject = await ludosGet(selectedObjectPath);
			selectedObjectJson = selectedObjectJsonEdit;
		} catch (e) {
			await emit('error', e);
		}
		selectedObjectSaveInProgress = false;
	};

	const handleObjectRevertClicked = () => {
		selectedObjectJsonEdit = selectedObjectJson;
	};

	onMount(() => {
		void refresh();
	});

	$: selectedObjectJsonNoChanges =
		selectedObject === null || selectedObjectJson === selectedObjectJsonEdit;
	$: $appConfig,
		async () => {
			await refreshAndResetPaths();
		};
	$: isEndpointAws = $appConfig.ludosEndpointType === 'AWS';
</script>

<div class="flex items-center gap-2">
	<div class="flex items-center gap-2">
		<p class="text-2xl my-2 text-primary-400 dark:text-primary-400">Ludos Storage</p>
	</div>
	<Button class="!p-1.5" primary on:click={refresh} disabled={inAsyncOperation}>
		<RotateOutline class="w-4 h-4" />
	</Button>
</div>

<Card
	class="w-full p-4 sm:p-4 my-2 max-w-full bg-secondary-700 dark:bg-space-900 border-0 overflow-y-hidden shadow-none"
>
	<div class="flex flex-col mb-2">
		<div class="flex gap-2">
			<Button class="!p-1" on:click={handleUpDir}>
				<ArrowUpOutline class="text-primary-100 dark:text-primary-100 mx-2" />
			</Button>
			<Input bind:value={currentPath}>
				<FolderOpenOutline slot="left" />
			</Input>
			<Button
				on:click={handleDeleteObject}
				disabled={selectedObjectPath === '' || selectedObjectSaveInProgress}
			>
				<TrashBinOutline />
			</Button>
		</div>
	</div>
	{#if isEndpointAws}
		<div class="flex justify-center mb-2">
			<Card horizontal={true} size="xl" class="text-nowrap">
				<p class="text-xl font-bold text-red-500">⚠️ PRODUCTION DATA: MODIFY WITH CARE ⚠️</p>
			</Card>
		</div>
	{/if}
	{#if selectedObjectPath !== ''}
		{#if inAsyncOperation}
			<div class="flex justify-center items-center">
				<Spinner class="h-8 w-8" />
			</div>
		{:else}
			<div class="flex flex-col space-y-2 overflow-y-hidden h-full">
				<Textarea class="font-mono" rows={30} bind:value={selectedObjectJsonEdit} />
				<div class="flex flex-row-reverse gap-2">
					<Button disabled={selectedObjectJsonNoChanges} on:click={handleObjectSaveClicked}>
						{#if selectedObjectSaveInProgress}
							<Spinner size={5} />
						{:else}
							<p>Save</p>
						{/if}
					</Button>
					<Button disabled={selectedObjectJsonNoChanges} on:click={handleObjectRevertClicked}
						>Revert</Button
					>
				</div>
			</div>
		{/if}
	{:else}
		<Table color="custom" striped={true}>
			<TableHead class="text-left border-b-0 p-2 bg-secondary-800 dark:bg-space-950 mb-2">
				<TableHeadCell class="p-2 w-1" />
				<TableHeadCell class="p-2">Path</TableHeadCell>
				<TableHeadCell class="p-2 w-1" />
			</TableHead>
			<TableBody>
				{#each filteredObjects as ludosObj}
					<TableBodyRow>
						{#if ludosObj.isObject}
							<TableBodyCell class="p-2"><FileOutline /></TableBodyCell>
						{:else}
							<TableBodyCell class="p-2"><FolderOpenOutline /></TableBodyCell>
						{/if}
						<TableBodyCell class="p-2">{ludosObj.name}</TableBodyCell>
						<TableBodyCell class="p-2">
							{#if ludosObj.isObject}
								<Button
									class="w-full"
									on:click={async () => {
										await handleViewObject(ludosObj);
									}}>View</Button
								>
							{:else}
								<Button
									class="w-full"
									on:click={() => {
										handleOpenFolder(ludosObj.name);
									}}>Open</Button
								>
							{/if}
						</TableBodyCell>
					</TableBodyRow>
				{:else}
					<TableBodyRow>
						<TableBodyCell class="p-2" />
						<TableBodyCell class="p-2">No data available</TableBodyCell>
						<TableBodyCell class="p-2" />
					</TableBodyRow>
				{/each}
			</TableBody>
		</Table>
	{/if}
</Card>

<ConfirmModal
	bind:showModal={showDeleteConfirm}
	bind:title={confirmTitleString}
	bind:resultCallback={handleDeleteConfirm}
/>
