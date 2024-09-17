<script lang="ts">
	import { Card, Table, TableBody } from 'flowbite-svelte';
	import type { LFSFile, Node } from '$lib/types';
	import TreeNode from '$lib/components/files/TreeNode.svelte';

	export let files: Node;
	export let selectedFiles: LFSFile[];
	const level = 0;
</script>

<Card
	class="w-full p-4 sm:p-4 h-full max-w-full dark:bg-secondary-600 border-0 shadow-none overflow-auto"
>
	<div class="flex flex-col gap-2 w-full h-full">
		<Table>
			<TableBody>
				<TreeNode {files} {level} bind:selectedFiles />
			</TableBody>
		</Table>
	</div>
</Card>
