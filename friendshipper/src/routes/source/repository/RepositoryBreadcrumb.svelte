<script lang="ts">
	import { ChevronRightOutline, HomeOutline } from 'flowbite-svelte-icons';

	export let path: string = '';
	export let onNavigate: (path: string) => void;

	$: segments = path === '' ? [] : path.split('/');
</script>

<div class="flex items-center gap-1 flex-wrap text-sm">
	<button
		type="button"
		class="flex items-center gap-1 px-2 py-1 rounded hover:bg-secondary-800 dark:hover:bg-space-950 {path ===
		''
			? 'text-primary-400'
			: 'text-white'}"
		on:click={() => {
			onNavigate('');
		}}
	>
		<HomeOutline class="w-4 h-4" />
		<span class="font-medium">repo</span>
	</button>
	{#each segments as segment, i}
		<ChevronRightOutline class="w-3 h-3 text-gray-400" />
		{@const segmentPath = segments.slice(0, i + 1).join('/')}
		<button
			type="button"
			class="px-2 py-1 rounded hover:bg-secondary-800 dark:hover:bg-space-950 {i ===
			segments.length - 1
				? 'text-primary-400 font-medium'
				: 'text-white'}"
			on:click={() => {
				onNavigate(segmentPath);
			}}
		>
			{segment}
		</button>
	{/each}
</div>
