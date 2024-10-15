<script lang="ts">
	import { onDestroy } from 'svelte';
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
	import { currentRoot, fetchIncludeList, selectedFile } from '$lib/stores';

	export let fileNode: Node;
	export let loading: boolean;
	export let level: number;

	const getChildren = async () => {
		if (fileNode.value.fileType === FileType.File) {
			return;
		}
		fileNode.children = [];
		const childFiles = await getFiles(fileNode.value.path);
		const newChildren = childFiles.map(
			(child) =>
				({
					value: child,
					open: false,
					children: []
				} as Node)
		);

		fileNode = {
			...fileNode,
			children: newChildren
		};
	};

	const handleOnClick = async () => {
		loading = true;
		fileNode = {
			...fileNode,
			open: !fileNode.open
		};
		if (fileNode.open) {
			await getChildren();
		} else {
			fileNode.children = [];
		}
		$selectedFile = fileNode.value;
		if (fileNode.value.fileType === FileType.File) {
			$currentRoot = fileNode.value.path.substring(0, fileNode.value.path.lastIndexOf('/'));
		} else {
			$currentRoot = fileNode.value.path;
		}
		loading = false;
	};

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
				disabled={loading}
				class="flex justify-start items-center border-0 gap-3 py-0.5 pl-2 rounded-md w-full"
				on:click={handleOnClick}
			>
				{#if fileNode.value.fileType === FileType.File}
					{#if $fetchIncludeList.includes(fileNode.value.path)}
						<HeartSolid class="w-4 h-4" />
					{:else}
						<HeartOutline class="w-4 h-4" />
					{/if}
					{#if fileNode.value.lfsState === LocalFileLFSState.Stub}
						<FileCheckOutline class="w-4 h-4" />
					{:else}
						<FileCheckSolid class="w-4 h-4" />
					{/if}
					<div class="w-3 mr-3">{fileNode.value.locked ? '🔒' : ''}</div>
					{fileNode.value.name}
				{:else}
					<FolderSolid class="w-4 h-4" />
					{fileNode.value.name}
				{/if}
			</Button>
		</TableBodyCell>
	</TableBodyRow>
	{#if fileNode.open}
		{#each fileNode.children as child}
			<svelte:self bind:fileNode={child} bind:loading level={level + 1} />
		{/each}
	{/if}
</div>
