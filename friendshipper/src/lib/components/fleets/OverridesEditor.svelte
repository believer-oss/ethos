<script lang="ts">
	// A list of per (map, cluster) sizing overrides: the shard's live overrides, or one release's.
	// A list rather than a matrix, so it stays small as clusters grow.
	import { Button } from 'flowbite-svelte';
	import { overrideRowIssues, sizingToForm, type OverrideRow } from '$lib/fleetSizing';
	import type { Nullable } from '$lib/types';
	import SizingInputs from './SizingInputs.svelte';

	export let rows: OverrideRow[];
	export let clusters: string[];
	export let maps: string[];
	export let limit: Nullable<number> | undefined = null;
	export let disabled = false;
	// Out: problems across every row, so the parent can disable Save.
	export let problemCount = 0;

	$: issues = rows.map((row) => overrideRowIssues(row, rows, limit));
	$: problemCount = issues.reduce((n, list) => n + list.length, 0);

	const add = () => {
		rows = [...rows, { cluster: clusters[0] ?? '', mapId: maps[0] ?? '', form: sizingToForm({}) }];
	};

	const remove = (index: number) => {
		rows = rows.filter((_, i) => i !== index);
	};

	const selectClass =
		'rounded p-1 text-xs bg-secondary-600 dark:bg-space-800 text-white border border-gray-500';
</script>

<div class="flex flex-col gap-2">
	{#each rows as row, i}
		<div class="flex items-start gap-2 flex-wrap">
			<select class={selectClass} bind:value={row.mapId} {disabled}>
				{#each maps.includes(row.mapId) ? maps : [row.mapId, ...maps] as m}
					<option value={m}>{m}</option>
				{/each}
			</select>
			<span class="text-xs text-gray-400 pt-1">in</span>
			<select class={selectClass} bind:value={row.cluster} {disabled}>
				{#each clusters.includes(row.cluster) ? clusters : [row.cluster, ...clusters] as c}
					<option value={c}>{c}</option>
				{/each}
			</select>
			<SizingInputs bind:form={row.form} issues={issues[i]} {disabled} />
			<button
				class="text-xs text-red-400 hover:underline pt-1 disabled:opacity-50"
				{disabled}
				on:click={() => {
					remove(i);
				}}>Remove</button
			>
		</div>
	{:else}
		<span class="text-xs text-gray-500">No overrides.</span>
	{/each}
	<div>
		<Button
			size="xs"
			color="alternative"
			disabled={disabled || !clusters.length || !maps.length}
			on:click={add}>Add override</Button
		>
	</div>
</div>
