<script lang="ts">
	import { onMount } from 'svelte';
	import { Button } from 'flowbite-svelte';
	import { ChevronDownOutline, ChevronRightOutline } from 'flowbite-svelte-icons';
	import JunitTestCase from './JunitTestCase.svelte';
	import type { JunitTestSuite } from '$lib/types';

	export let testSuite: JunitTestSuite;

	let subTestSuites: JunitTestSuite[] = [];
	let totalTests = 0;
	let totalFailures = 0;
	let expanded = false;

	const toggleExpanded = (): void => {
		expanded = !expanded;
	};

	const calculateTotalTests = (): void => {
		let total = 0;
		let failures = 0;

		// traverse all sub test suites and test cases
		const traverse = (inTestSuite: JunitTestSuite): void => {
			if (inTestSuite.testsuite) {
				inTestSuite.testsuite.forEach((suite) => {
					traverse(suite);
				});
			}
			if (inTestSuite.testcase) {
				total += inTestSuite.testcase.length;
				failures += inTestSuite.testcase.filter((test) => test.failure).length;
			}
		};
		traverse(testSuite);

		totalTests = total;
		totalFailures = failures;
	};

	onMount(() => {
		subTestSuites = testSuite.testsuite ?? [];
		calculateTotalTests();
	});
</script>

<div class="flex flex-col gap-2 w-full">
	<div class="flex gap-2 items-center justify-between">
		<span class="text-sm font-bold text-gray-300 dark:text-gray-300 w-36 min-w-36"
			>{testSuite.name}</span
		>
		<div class="flex gap-2 items-center">
			<span class="text-sm font-medium text-gray-300 dark:text-gray-300 w-36 min-w-36 text-right"
				>{totalFailures > 0 ? '⚠️ ' : ''}
				{totalTests - totalFailures} / {totalTests} tests passed</span
			>
			<Button size="xs" on:click={toggleExpanded}>
				{#if expanded}
					<ChevronDownOutline class="w-4 h-4" />
				{:else}
					<ChevronRightOutline class="w-4 h-4" />
				{/if}
			</Button>
		</div>
	</div>
	{#if expanded}
		{#if testSuite.testcase}
			{#each testSuite.testcase as testcase}
				<div class="pl-4">
					<JunitTestCase {testcase} />
				</div>
			{/each}
		{/if}
		{#each subTestSuites as subSuite}
			<div class="pl-4">
				<svelte:self testSuite={subSuite} />
			</div>
		{/each}
	{/if}
</div>
