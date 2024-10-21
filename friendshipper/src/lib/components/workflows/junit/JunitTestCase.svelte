<script lang="ts">
	import { Badge } from 'flowbite-svelte';
	import type { JunitTestCase } from '$lib/types';

	export let testcase: JunitTestCase;

	const getBadgeColor = (): string => {
		if (testcase.failure) {
			return 'bg-red-700 dark:bg-red-700';
		}

		return 'bg-lime-600 dark:bg-lime-600';
	};

	const getBadgeText = (): string => {
		if (testcase.failure) {
			return 'Failed';
		}

		return 'Passed';
	};
</script>

<div class="flex flex-col gap-2">
	<div class="flex gap-2 w-full justify-between">
		<span class="text-sm text-gray-300 dark:text-gray-300 w-full">
			{#if testcase.failure}
				<span>⚠️</span>
			{/if}
			{testcase.name}
		</span>
		<div class="flex gap-2">
			<Badge class="text-white dark:text-white {getBadgeColor()} w-24 min-w-24">
				{getBadgeText()}
			</Badge>
		</div>
	</div>
	{#if testcase.failure}
		{#each testcase.failure as failure}
			<div class="rounded-md p-2 border border-white dark:border-white">
				<code class="text-sm font-medium text-gray-300 dark:text-gray-300 w-full"
					>{failure.$value}</code
				>
			</div>
		{/each}
	{/if}
</div>
