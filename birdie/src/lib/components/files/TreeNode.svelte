<script lang="ts">
	import { onDestroy, onMount } from 'svelte';
	import { Button, TableBodyCell, TableBodyRow } from 'flowbite-svelte';
	import {
		FileCheckOutline,
		FileCheckSolid,
		FolderSolid,
		HeartOutline,
		HeartSolid
	} from 'flowbite-svelte-icons';
	import { FileType, LocalFileLFSState, type Node } from '$lib/types';
	import { getFiles } from '$lib/repo';
	import { fetchIncludeList, selectedFile } from '$lib/stores';

	export let fileNode: Node;
	export let level: number;

	let open = false;

	const getChildren = async () => {
		if (fileNode.value.fileType === FileType.File) {
			return;
		}
		fileNode.children = [];
		const childFiles = await getFiles(fileNode.value.path);
		childFiles.forEach((child) => {
			const newChild: Node = {
				value: child,
				children: []
			};
			fileNode.children.push(newChild);
		});
	};

	const handleOnClick = () => {
		open = !open;
		if (fileNode.value.fileType === FileType.File) {
			$selectedFile = fileNode.value;
		} else {
			$selectedFile = null;
		}
	};

	onMount(() => {
		void getChildren();
	});

	onDestroy(() => {
		fileNode.children = [];
	});
</script>

<div class="w-full {level % 2 === 0 ? 'dark:bg-secondary-600' : 'dark:bg-secondary-700'}">
	<TableBodyRow class="text-left w-max border-b-0 w-full" color="custom">
		{#each Array(level) as _}
			<TableBodyCell class="w-1 px-2" />
			<TableBodyCell class="w-1 px-2" />
		{/each}
		<TableBodyCell class="p-2 w-full">
			<Button
				outline={$selectedFile?.path !== fileNode.value.path}
				class="flex justify-start items-center border-0 gap-3 py-0.5 pl-2 rounded-md w-full"
				on:click={handleOnClick}
			>
				{#if fileNode.value.fileType === FileType.File}
					{#if $fetchIncludeList.includes(fileNode.value.path)}
						<HeartSolid />
					{:else}
						<HeartOutline />
					{/if}
					{#if fileNode.value.lfsState === LocalFileLFSState.Stub}
						<FileCheckOutline />
					{:else}
						<FileCheckSolid />
					{/if}
					<div class="w-3 mr-3">{fileNode.value.locked ? '🔒' : ''}</div>
					{fileNode.value.name}
				{:else}
					<FolderSolid />
					{fileNode.value.name}
				{/if}
			</Button>
		</TableBodyCell>
	</TableBodyRow>
	{#if open}
		{#each fileNode.children as child}
			<svelte:self bind:fileNode={child} level={level + 1} />
		{/each}
	{/if}
</div>
