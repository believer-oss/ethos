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

	const handleEditMetadata = () => {
		if (!metadata) return;

		$enableGlobalSearch = false;

		tempMetadata = { ...metadata };

		if (!tempMetadata.character) {
			tempMetadata.character = {
				codeName: '',
				characterName: ''
			};
		}

		editingMetadata = true;
	};

	const handleAddRig = () => {};

	const handleRemoveRig = (rigName: string) => {
		tempMetadata.character.rigs.delete(rigName);
	};

	const cancelEditMetadata = () => {
		editingMetadata = false;
		$enableGlobalSearch = true;
	};

	const saveMetadata = async () => {
		if (!tempMetadata) return;

		updatingMetadata = true;
		// Save the metadata

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
			<div class="flex h=20 items-center gap-2 mb-2 justify-between">
				<Label for="codeName" class="mb-1">Code_Name</Label>
				<Input label="CodeName" bind:value={tempMetadata.character.codeName} />
			</div>

			<div class="flex items-center gap-2 mb-2 justify-between">
				<Label for="characterName" class="mb-1">Name</Label>
				<Input label="CharacterName" bind:value={tempMetadata.character.characterName} />
			</div>

			<div class="flex items-center gap-2 mb-2 justify-between">
				<Label for="rig" class="mb-1">Rigs</Label>
				<Button size="xs" on:click={handleAddRig}><PlusSolid class="w-4 h-4" /></Button>
			</div>

			{#each Object.entries(tempMetadata.character.rigs) as [rigName, _]}
				<div class="flex gap-2">
					<Label for="rigName" class="mb-1">{rigName}</Label>
					<Input label="rigPath" bind:value={tempMetadata.character.rigs[rigName]} />
					<Button size="xs" on:click={() => handleRemoveRig(rigName)}
						><CloseSolid class="w-4 h-4" /></Button
					>
				</div>
			{/each}
		{:else}
			CodeName: {metadata?.character?.codeName ?? ''}
			Name: {metadata?.character?.characterName ?? ''}
		{/if}
	</div>
</Card>
