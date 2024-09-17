<script lang="ts">
	import { Button, Checkbox, TableBodyCell, TableBodyRow } from 'flowbite-svelte';
	import {
		ChevronDownOutline,
		ChevronUpOutline,
		FileSolid,
		FolderSolid
	} from 'flowbite-svelte-icons';
	import { FileType, type Node } from '$lib/types';

	export let files: Node;
	export let level: number;

	let open = false;
</script>

<div>
	<TableBodyRow class="text-left w-max border-b-0" color="custom">
		{#each Array(level) as _}
			<TableBodyCell class="w-1" />
		{/each}
		<TableBodyCell class="p-1 w-8">
			<Checkbox class="!p-1.5 mr-0" />
		</TableBodyCell>
		<TableBodyCell class="p-2 w-4">
			<Button
				outline
				class="flex justify-start items-center border-0 py-0.5 pl-2 rounded-md w-full"
				on:click={() => {
					open = !open;
				}}
			>
				{#if files.value.fileType === FileType.Directory}
					<FolderSolid class="w-4 h-4" />
					<div class="w-3 mr-3">{files.value.locked ? '🔒' : ''}</div>
					{files.value.name}
					{#if open}
						<ChevronDownOutline class="w-3 h-3" />
					{:else}
						<ChevronUpOutline class="w-3 h-3" />
					{/if}
				{:else}
					<FileSolid class="w-4 h-4" />
					<div class="w-3 mr-3">{files.value.locked ? '🔒' : ''}</div>
					{files.value.name}
				{/if}
			</Button>
		</TableBodyCell>
	</TableBodyRow>
	{#if open}
		{#each files.children as child}
			<svelte:self files={child} level={level + 1} />
		{/each}
	{/if}
</div>
