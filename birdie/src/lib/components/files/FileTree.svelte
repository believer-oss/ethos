<script lang="ts">
	import { Card, Table, TableBody } from 'flowbite-svelte';
	import { onMount } from 'svelte';
	import { fs } from '@tauri-apps/api';
	import { FileType, type LFSFile, type Node } from '$lib/types';
	import TreeNode from '$lib/components/files/TreeNode.svelte';
	import { getFiles } from '$lib/repo';
	import { shiftSelectedFile, selectedFile, selectedTreeFiles, currentRoot } from '$lib/stores';
	import { CURRENT_ROOT_PATH, FILE_TREE_PATH } from '$lib/consts';

	export let fileNode: Node;
	export let loading: boolean;
	export let onFileClick: (file: LFSFile) => Promise<void>;

	let ctrlHeld = false;
	let shiftHeld = false;
	const level = 0;

	const handleSaveFileTree = async () => {
		await fs.writeFile(FILE_TREE_PATH, JSON.stringify(fileNode, null, 2), {
			dir: fs.BaseDirectory.AppLocalData
		});
	};

	const handleSaveCurrentRoot = async () => {
		await fs.writeFile(CURRENT_ROOT_PATH, $currentRoot, { dir: fs.BaseDirectory.AppLocalData });
	};

	// recursively update the tree starting from the root node
	const updateTree = async (node: Node): Promise<Node> => {
		// if node isn't open, don't try updating children
		if (!node.open || node.value.fileType === FileType.File) {
			return node;
		}
		const updatedChildFiles = await getFiles(node.value.path);
		let updatedChildNodes: Node[] = [];
		// deleted children will not be considered since they will not exist inside updatedChildFiles
		updatedChildFiles
			.filter((child) => child.fileType !== FileType.File)
			.forEach((child) => {
				const existingChild = node.children.find((c) => c.value.path === child.path);
				if (existingChild) {
					// if this file already exists as a child, simply update it's LFSFile value
					updatedChildNodes.push({
						...existingChild,
						value: child
					});
				} else {
					// otherwise, create a new node for a new child
					updatedChildNodes.push({
						value: child,
						open: false,
						children: []
					});
				}
			});
		updatedChildNodes = await Promise.all(updatedChildNodes.map((child) => updateTree(child)));

		// overwrite the children of the current node with the updated children
		return { ...node, children: updatedChildNodes };
	};

	const onKeyDown = (event: KeyboardEvent) => {
		if (event.key === 'Shift') {
			shiftHeld = true;
		} else if (event.key === 'Control') {
			ctrlHeld = true;
		}
	};

	const onKeyUp = (event: KeyboardEvent) => {
		if (event.key === 'Shift') {
			shiftHeld = false;
		} else if (event.key === 'Control') {
			ctrlHeld = false;
		}
	};

	const refresh = async () => {
		loading = true;
		const updatedTree = await updateTree(fileNode);
		if (updatedTree) {
			fileNode = updatedTree;
		}
		await handleSaveFileTree();
		await handleSaveCurrentRoot();
		loading = false;
	};

	const clicked = async () => {
		let foundStart = false;
		// recursively traverse the tree to select all files between selectedFile and multiSelectedFile
		const dfsMultiSelect = (node: Node): boolean => {
			if (
				!foundStart &&
				(node.value.path === $selectedFile?.path || node.value.path === $shiftSelectedFile?.path)
			) {
				// if we haven't found the start, and we reach either selected files, start pushing to selectedTreeFiles
				foundStart = true;
			} else if (
				node.value.path === $selectedFile?.path ||
				node.value.path === $shiftSelectedFile?.path
			) {
				// there should never be 2 duplicate paths in the file tree, so we will always hit the other selected end to stop traversing
				$selectedTreeFiles.push(node.value);
				return true;
			}
			if (foundStart) {
				$selectedTreeFiles.push(node.value);
			}
			for (const child of node.children) {
				if (dfsMultiSelect(child)) {
					return true;
				}
			}
			return false;
		};
		if ($selectedFile && $shiftSelectedFile && $selectedFile.path !== $shiftSelectedFile.path) {
			dfsMultiSelect(fileNode);
			await refresh();
		}
		await handleSaveFileTree();

		if ($selectedFile) {
			await onFileClick($selectedFile);
		}
	};

	onMount(() => {
		// refresh every 30 seconds
		const interval = setInterval(async () => {
			await refresh();
		}, 15000);

		return () => {
			clearInterval(interval);
		};
	});
</script>

<svelte:window on:keydown={onKeyDown} on:keyup={onKeyUp} />
<Card
	class="p-4 sm:p-4 h-full max-w-full dark:bg-secondary-600 border-0 shadow-none overflow-auto"
	on:click={async () => {
		await clicked();
	}}
>
	<div class="flex flex-col gap-2 w-full h-full">
		<Table>
			<TableBody>
				<TreeNode bind:fileNode bind:loading {shiftHeld} {ctrlHeld} {level} />
			</TableBody>
		</Table>
	</div>
</Card>
