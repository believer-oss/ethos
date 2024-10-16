<script lang="ts">
	import type { JunitOutput } from '$lib/types';
	import JunitTestSuite from './JunitTestSuite.svelte';

	export let junitOutput: JunitOutput;
</script>

<div
	class="flex flex-col gap-2 h-full p-4 mx-16 bg-secondary-800 dark:bg-space-950 overflow-x-auto overflow-y-auto rounded-md border border-secondary-600"
>
	{#each junitOutput.testsuite as testSuite}
		<JunitTestSuite {testSuite} />
	{/each}
</div>
