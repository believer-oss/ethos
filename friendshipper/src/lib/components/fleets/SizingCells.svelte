<script lang="ts">
	// One sizing as three grid cells (buffer, min, max). An empty cell shows what it inherits in grey
	// italics; typing overrides it and clearing it inherits again. A cell changed since the form
	// loaded is highlighted until saved; one with a problem has a red border.
	import type { Inherited, SizingField, SizingForm, SizingIssue } from '$lib/fleetSizing';

	export let form: SizingForm;
	export let inherited: Inherited;
	export let issues: SizingIssue[] = [];
	export let disabled = false;
	// The form as loaded, to highlight changed cells; null for a row added since.
	export let loaded: SizingForm | null = null;
	// A hover on every cell, e.g. the live preview.
	export let title = '';

	const FIELDS: SizingField[] = ['bufferSize', 'minReplicas', 'maxReplicas'];

	const inputClass =
		'w-24 rounded p-1 text-xs bg-secondary-600 dark:bg-space-800 text-white border placeholder:italic placeholder:text-gray-500 disabled:opacity-50';
</script>

{#each FIELDS as field (field)}
	<td class="px-1 py-1" {title}>
		<input
			class={inputClass}
			class:border-red-500={issues.some((i) => i.field === field)}
			class:border-gray-500={!issues.some((i) => i.field === field)}
			class:ring-1={loaded !== null && loaded[field] !== form[field]}
			class:ring-primary-400={loaded !== null && loaded[field] !== form[field]}
			bind:value={form[field]}
			placeholder={inherited[field]}
			disabled={disabled || form.parked}
		/>
	</td>
{/each}
