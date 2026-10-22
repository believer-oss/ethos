<script lang="ts">
	import { Label, Select } from 'flowbite-svelte';
	import type { CommitTagCategory } from '$lib/types';

	export let categories: CommitTagCategory[];
	export let selected: string[];
	export let disabled = false;

	let lastCount = -1;
	$: if (categories.length !== lastCount) {
		lastCount = categories.length;
		selected = categories.map(() => '');
	}

	$: visible = categories.some((c) => c.options.length > 0);
</script>

{#if visible}
	<div class="flex flex-row flex-wrap gap-3 w-full">
		{#each categories as category, i}
			{#if category.options.length > 0}
				<div class="flex flex-col gap-1 min-w-[10rem] flex-1">
					<Label class="text-gray-300">{category.name}</Label>
					<Select
						size="sm"
						bind:value={selected[i]}
						{disabled}
						class="text-white bg-secondary-800 dark:bg-space-950"
					>
						<option value="">None</option>
						{#each category.options as o}
							<option value={o.tag}>{o.tag}{o.label ? ` — ${o.label}` : ''}</option>
						{/each}
					</Select>
					<span class="text-xs text-gray-400 min-h-[1rem]">
						{category.options.find((o) => o.tag === selected[i])?.description ?? ''}
					</span>
				</div>
			{/if}
		{/each}
	</div>
{/if}
