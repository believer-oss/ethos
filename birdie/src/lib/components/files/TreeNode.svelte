<script lang="ts">
	import { onMount } from 'svelte';
	import { Button, TableBodyCell, TableBodyRow } from 'flowbite-svelte';
	import {
		FileCheckOutline,
		FileCheckSolid,
		FolderSolid,
		HeartOutline,
		HeartSolid
	} from 'flowbite-svelte-icons';
	import { FileType, type LFSFile, LocalFileLFSState, type Node, type Nullable } from '$lib/types';
	import { getFiles } from '$lib/repo';
	import { fetchIncludeList } from '$lib/stores';

	export let fileNode: Node;
	export let selectedFile: Nullable<LFSFile>;
	export let level: number;

	let open = false;

	$: isSelected = selectedFile === fileNode.value;
	$: isInFetchInclude = $fetchIncludeList.includes(fileNode.value?.path.replace(/^\/+/, '') ?? '');

	const getChildren = async () => {
		if (fileNode.value?.fileType === FileType.File) {
			return;
		}

		let childFiles: LFSFile[] = [];
		fileNode.children = [];
		if (fileNode.value) {
			childFiles = await getFiles(fileNode.value.path);
		} else {
			childFiles = await getFiles('');
		}
		if (childFiles.length > 0) {
			childFiles.forEach((child) => {
				const newChild: Node = {
					parent: fileNode,
					value: child,
					children: []
				};
				fileNode.children.push(newChild);
			});
		}
	};

	onMount(() => {
		void getChildren();
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
				outline={!isSelected}
				class="flex justify-start items-center border-0 py-0.5 pl-2 rounded-md w-full"
				on:click={() => {
					open = !open;
					selectedFile = fileNode.value ? fileNode.value : null;
				}}
			>
				<div class="flex gap-2 items-center w-full">
					{#if fileNode.value && fileNode.value.fileType === FileType.File}
						<div class="flex gap-2 items-center">
							{#if fileNode.value.fileType === FileType.File}
								{#if isInFetchInclude}
									<HeartSolid class="w-4 h-4 text-green-500" />
								{:else}
									<HeartOutline class="w-4 h-4 text-gray-500" />
								{/if}
							{/if}
							{#if fileNode.value.lfsState === LocalFileLFSState.Local}
								<FileCheckSolid class="w-4 h-4 text-green-500" />
							{:else}
								<FileCheckOutline class="w-4 h-4 text-gray-500" />
							{/if}
							<div class="w-3 mr-3">{fileNode.value.locked ? '🔒' : ''}</div>
							<span>{fileNode.value.name ?? '/'}</span>
						</div>
					{:else}
						<div class="flex gap-2 items-center">
							<FolderSolid class="w-4 h-4" />
							{fileNode.value ? fileNode.value.name : '/'}
						</div>
					{/if}
				</div>
			</Button>
		</TableBodyCell>
	</TableBodyRow>
	{#if open}
		{#each fileNode.children as child}
			<svelte:self bind:fileNode={child} bind:selectedFile level={level + 1} />
		{/each}
	{/if}
</div>
