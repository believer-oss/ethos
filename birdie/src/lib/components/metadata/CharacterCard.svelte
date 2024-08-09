<script lang="ts">
	import { Button, Card, Input, Label } from 'flowbite-svelte';
	import { CheckSolid, CloseSolid, EditOutline, PlusSolid } from 'flowbite-svelte-icons';
	import { emit } from '@tauri-apps/api/event';
	import type { DirectoryMetadata, Nullable } from '$lib/types';
	import { enableGlobalSearch } from '$lib/stores';

	export let metadata: DirectoryMetadata;
	export let onMetadataSaved: (metadata: DirectoryMetadata) => Promise<void>;

	let tempMetadata: Nullable<DirectoryMetadata> = null;

	let editingMetadata: boolean = false;
	let updatingMetadata: boolean = false;
	let newRigEntry: string = '';
	let tempRigData: [string, string][];
	let tempAnimationData: [string, string][];
	let newAnimationEntry: string = '';
	let tempMeshData: [string, string][];
	let newMeshEntry: string = '';

	const handleEditMetadata = () => {
		if (!metadata) return;

		$enableGlobalSearch = false;

		tempMetadata = { ...metadata };

		if (!tempMetadata.character) {
			tempMetadata.character = {
				codeName: '',
				characterName: '',
				rigs: {},
				animations: {},
				meshes: {}
			};
		}

		if (tempMetadata.character) {
			// Temporary array from 'rigs' so {#each} in svelte can iterate over it
			tempRigData = Object.entries(tempMetadata.character.rigs);
			tempAnimationData = Object.entries(tempMetadata.character.animations);
			tempMeshData = Object.entries(tempMetadata.character.meshes);
		}

		editingMetadata = true;
	};

	const cancelEditMetadata = () => {
		editingMetadata = false;
		$enableGlobalSearch = true;
	};

	const handleAddRig = (rigName: string) => {
		if (rigName !== '') {
			tempRigData.push([rigName, '']);
			tempRigData = [...tempRigData]; // Force UI update
			newRigEntry = '';
		}
	};

	const handleRemoveRig = (rigName: string) => {
		const index = tempRigData.findIndex((x) => x[0] === rigName);
		if (index > -1) {
			tempRigData.splice(index, 1);
			tempRigData = [...tempRigData]; // Force UI update
		}
	};

	const handleAddAnimation = (directoryName: string) => {
		if (directoryName !== '') {
			tempAnimationData.push([directoryName, '']);
			tempAnimationData = [...tempAnimationData]; // Force UI update
			newAnimationEntry = '';
		}
	};

	const handleRemoveAnimation = (directoryName: string) => {
		const index = tempAnimationData.findIndex((x) => x[0] === directoryName);
		if (index > -1) {
			tempAnimationData.splice(index, 1);
			tempAnimationData = [...tempAnimationData]; // Force UI update
		}
	};

	const handleAddMesh = (meshName: string) => {
		if (meshName !== '') {
			tempMeshData.push([meshName, '']);
			tempMeshData = [...tempMeshData]; // Force UI update
			newMeshEntry = '';
		}
	};

	const handleRemoveMesh = (meshName: string) => {
		const index = tempMeshData.findIndex((x) => x[0] === meshName);
		if (index > -1) {
			tempMeshData.splice(index, 1);
			tempMeshData = [...tempMeshData]; // Force UI update
		}
	};

	const saveMetadata = async () => {
		if (!tempMetadata) return;

		updatingMetadata = true;
		// Save the metadata
		// On Save fill 'rigs' Record from the Temporary Array
		if (tempMetadata.character) {
			tempMetadata.character.rigs = {};
			for (const [name, path] of tempRigData) {
				tempMetadata.character.rigs[name] = path;
			}
			tempMetadata.character.animations = {};
			for (const [name, path] of tempAnimationData) {
				tempMetadata.character.animations[name] = path;
			}
			tempMetadata.character.meshes = {};
			for (const [name, path] of tempMeshData) {
				tempMetadata.character.meshes[name] = path;
			}
		}

		try {
			await onMetadataSaved(tempMetadata);

			metadata = tempMetadata;
		} catch (e) {
			await emit('error', e);
		}

		editingMetadata = false;
		updatingMetadata = false;
		$enableGlobalSearch = true;
	};
</script>

<Card class="w-full p-4 sm:p-4 max-w-full dark:bg-secondary-600 border-0 shadow-none overflow-auto">
	<div class="flex items-center gap-2 mb-2 justify-between">
		<p class="text-xl dark:text-primary-400">Character Metadata</p>
		{#if editingMetadata}
			<div class="flex gap-2">
				<Button disabled={updatingMetadata} size="xs" class="my-1" on:click={saveMetadata}
					><CheckSolid class="w-4 h-4" /></Button
				>
				<Button
					disabled={updatingMetadata}
					size="xs"
					class="my-1 dark:bg-red-800 hover:dark:bg-red-900"
					on:click={cancelEditMetadata}><CloseSolid class="w-4 h-4" /></Button
				>
			</div>
		{:else}
			<div class="flex gap-2">
				<Button size="xs" on:click={handleEditMetadata}><EditOutline class="w-4 h-4" /></Button>
			</div>
		{/if}
	</div>
	<div>
		{#if editingMetadata && tempMetadata && tempMetadata.character}
			<div class="flex items-center gap-2 mb-2 justify-between">
				<Label for="codeName" class="mb-1 w-1/12">Code Name</Label>
				<Input label="CodeName" class="h-9" bind:value={tempMetadata.character.codeName} />
			</div>

			<div class="flex items-center gap-2 mb-2 justify-between">
				<Label for="characterName" class="mb-1 w-1/12">Name</Label>
				<Input
					label="CharacterName"
					class="h-9"
					bind:value={tempMetadata.character.characterName}
				/>
			</div>

			<div class="flex items-center gap-2 mt-6 mb-2">
				<Label for="rig" class="mb-1 w-1/2">CONTROL RIGS</Label>
				<div class="flex items-center gap-2 w-1/2">
					<Label for="newRigName" class="font-bold w-3/4 mb-1" style="text-align: right"
						>New Rig File</Label
					>
					<Input label="rigPath" class="h-8 w-1/3" bind:value={newRigEntry} />
					<Button
						size="xs"
						on:click={() => {
							handleAddRig(newRigEntry);
						}}><PlusSolid class="w-3 h-4" /></Button
					>
				</div>
			</div>

			{#each tempRigData as [rigName, rigPath]}
				<div class="flex items-center justify-start gap-2 mb-0.5">
					<Label for="rigName" class="mb-1 ml-4 w-1/12">{rigName}</Label>
					<Input label="rigPath" class="h-8" bind:value={rigPath} />
					<Button
						size="xs"
						class="my-1 dark:bg-red-800 hover:dark:bg-red-900"
						on:click={() => {
							handleRemoveRig(rigName);
						}}
					>
						<CloseSolid class="w-3 h-3" /></Button
					>
				</div>
			{/each}

			<div class="flex items-center gap-2 mt-6 mb-2">
				<Label for="rig" class="mb-1 w-1/2">ANIMATIONS</Label>
				<div class="flex items-center gap-2 w-1/2">
					<Label for="newDirectoryName" class="font-bold w-3/4 mb-1" style="text-align: right"
						>New Animation Directory</Label
					>
					<Input label="directoryPath" class="h-8 w-1/3" bind:value={newAnimationEntry} />
					<Button
						size="xs"
						on:click={() => {
							handleAddAnimation(newAnimationEntry);
						}}><PlusSolid class="w-3 h-4" /></Button
					>
				</div>
			</div>

			{#each tempAnimationData as [directoryName, directoryPath]}
				<div class="flex items-center justify-start gap-2 mb-0.5">
					<Label for="directoryName" class="mb-1 ml-4 w-1/12">{directoryName}</Label>
					<Input label="directoryPath" class="h-8" bind:value={directoryPath} />
					<Button
						size="xs"
						class="my-1 dark:bg-red-800 hover:dark:bg-red-900"
						on:click={() => {
							handleRemoveAnimation(directoryName);
						}}
					>
						<CloseSolid class="w-3 h-3" /></Button
					>
				</div>
			{/each}

			<div class="flex items-center gap-2 mt-6 mb-2">
				<Label for="rig" class="mb-1 w-1/2">SKELETAL MESHES</Label>
				<div class="flex items-center gap-2 w-1/2">
					<Label for="newMeshName" class="font-bold w-3/4 mb-1" style="text-align: right"
						>New Mesh File</Label
					>
					<Input label="meshPath" class="h-8 w-1/3" bind:value={newMeshEntry} />
					<Button
						size="xs"
						on:click={() => {
							handleAddMesh(newMeshEntry);
						}}><PlusSolid class="w-3 h-4" /></Button
					>
				</div>
			</div>

			{#each tempMeshData as [meshName, meshPath]}
				<div class="flex items-center justify-start gap-2 mb-0.5">
					<Label for="meshName" class="mb-1 ml-4 w-1/12">{meshName}</Label>
					<Input label="meshPath" class="h-8" bind:value={meshPath} />
					<Button
						size="xs"
						class="my-1 dark:bg-red-800 hover:dark:bg-red-900"
						on:click={() => {
							handleRemoveMesh(meshName);
						}}
					>
						<CloseSolid class="w-3 h-3" /></Button
					>
				</div>
			{/each}
		{:else}
			Name: {metadata?.character?.characterName ?? ''}
			<div>
				CodeName: {metadata?.character?.codeName ?? ''}
			</div>
		{/if}
	</div>
</Card>
