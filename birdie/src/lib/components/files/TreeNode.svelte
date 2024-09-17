<script lang="ts">
	import { onMount } from 'svelte';
	import { Button, Checkbox, TableBodyCell, TableBodyRow } from 'flowbite-svelte';
	import { FileSolid, FolderSolid } from 'flowbite-svelte-icons';
	import { FileType, type LFSFile, type Node } from '$lib/types';
	import { getFiles } from '$lib/repo';

	export let files: Node;
	export let selectedFiles: LFSFile[];
	export let level: number;
	export let onDeselect: (path: string) => void = () => {};

	$: isSelected = selectedFiles.some((selected) => selected.path === files.value.path);

	const select = () => {
		const addFileRecursively = (node: Node) => {
			if (node.value.fileType === FileType.File) {
				if (!selectedFiles.some((file) => file.path === node.value.path)) {
					selectedFiles = [...selectedFiles, node.value as LFSFile];
				}
			} else {
				selectedFiles = [...selectedFiles, node.value as LFSFile];
				node.children.forEach(addFileRecursively);
			}
		};

		addFileRecursively(files);
	};

	const deselect = (path: string) => {
		selectedFiles = selectedFiles.filter((item) => item.path !== path);
		onDeselect(files.parent?.value.path);
	};

	// Deselect all children recursively
	const deselectRecursively = (node: Node) => {
		if (node.value.fileType === FileType.File) {
			selectedFiles = selectedFiles.filter((file) => file.path !== node.value.path);
		} else {
			selectedFiles = selectedFiles.filter((file) => file.path !== node.value.path);
			node.children.forEach(deselectRecursively);
		}
	};

	const handleFileToggled = () => {
		if (!isSelected) {
			select();
		} else {
			deselectRecursively(files);
			deselect(files.value.path);
		}
	};

	let open = false;

	onMount(async () => {
		const childFiles: LFSFile[] = await getFiles(files.value.path);
		if (childFiles.length > 0) {
			childFiles.forEach((child) => {
				const newChild: Node = {
					parent: files,
					value: child,
					children: []
				};
				files.children.push(newChild);
			});
		}
	});
</script>

<div class="w-full {level % 2 === 0 ? 'dark:bg-secondary-600' : 'dark:bg-secondary-700'}">
	<TableBodyRow class="text-left w-max border-b-0 w-full" color="custom">
		{#each Array(level) as _}
			<TableBodyCell class="w-1 px-2" />
		{/each}
		<TableBodyCell class="p-1 w-8">
			<Checkbox class="!p-1.5 mr-0" checked={isSelected} on:change={handleFileToggled} />
		</TableBodyCell>
		<TableBodyCell class="p-2 w-full">
			<Button
				outline
				class="flex justify-start items-center border-0 py-0.5 pl-2 rounded-md w-full"
				on:click={() => {
					open = !open;
				}}
			>
				<div class="flex gap-2 items-center w-full">
					{#if files.value.fileType === FileType.File}
						<div class="flex gap-2 items-center">
							<FileSolid class="w-4 h-4" />
							<div class="w-3 mr-3">{files.value.locked ? '🔒' : ''}</div>
							<span>{files.value.name ?? '/'}</span>
						</div>
					{:else}
						<div class="flex gap-2 items-center">
							<FolderSolid class="w-4 h-4" />
							{files.value.name ?? '/'}
						</div>
					{/if}
				</div>
			</Button>
		</TableBodyCell>
	</TableBodyRow>
	{#if open}
		{#each files.children as child}
			<svelte:self files={child} level={level + 1} bind:selectedFiles onDeselect={deselect} />
		{/each}
	{/if}
</div>
