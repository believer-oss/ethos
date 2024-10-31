<script lang="ts">
	import { Card, Table, TableBody } from 'flowbite-svelte';
	import { onMount } from 'svelte';
	import { FileType, type Node } from '$lib/types';
	import TreeNode from '$lib/components/files/TreeNode.svelte';
	import { getFiles } from '$lib/repo';

	export let fileNode: Node;
	export let loading: boolean;

	let shiftHeld = false;
	const level = 0;

	// recursively update the tree starting from the root node
	const updateTree = async (node: Node): Promise<Node> => {
		// if node isn't open, don't try updating children
		if (!node.open || node.value.fileType === FileType.File) {
			return node;
		}
		const updatedChildFiles = await getFiles(node.value.path);
		let updatedChildNodes: Node[] = [];
		// deleted children will not be considered since they will not exist inside updatedChildFiles
		updatedChildFiles.forEach((child) => {
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
		}
	};

	const onKeyUp = (e: KeyboardEvent) => {
		if (e.key === 'Shift') {
			shiftHeld = false;
		}
	};

	const refresh = async () => {
		loading = true;
		const updatedTree = await updateTree(fileNode);
		if (updatedTree) {
			fileNode = updatedTree;
		}
		loading = false;
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
	class="w-full p-4 sm:p-4 h-full max-w-full dark:bg-secondary-600 border-0 shadow-none overflow-auto"
>
	<div class="flex flex-col gap-2 w-full h-full">
		<Table>
			<TableBody>
				<TreeNode bind:fileNode bind:loading {shiftHeld} {level} />
			</TableBody>
		</Table>
	</div>
</Card>
