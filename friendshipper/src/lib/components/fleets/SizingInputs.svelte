<script lang="ts">
	// One sizing's inputs: buffer, min, max and (optionally) parked. Empty fields are inherited from
	// the next less specific level, shown as placeholders. Problems show under the inputs.
	import { sizingIssueText, type SizingForm, type SizingIssue } from '$lib/fleetSizing';

	export let form: SizingForm;
	export let issues: SizingIssue[] = [];
	export let disabled = false;
	export let allowParked = true;
	// Shown in empty fields: what they inherit.
	export let placeholders: Partial<Record<'bufferSize' | 'minReplicas' | 'maxReplicas', string>> =
		{};

	const inputClass =
		'w-20 rounded p-1 text-xs bg-secondary-600 dark:bg-space-800 text-white border border-gray-500 disabled:opacity-50';

	const hasIssue = (list: SizingIssue[], field: string) => list.some((i) => i.field === field);
</script>

<div class="flex flex-col gap-1">
	<div class="flex items-center gap-2 text-xs text-gray-400">
		<label class="flex items-center gap-1">
			Buffer
			<input
				class={inputClass}
				class:border-red-500={hasIssue(issues, 'bufferSize')}
				bind:value={form.bufferSize}
				placeholder={placeholders.bufferSize ?? 'e.g. 2 or 20%'}
				disabled={disabled || form.parked}
			/>
		</label>
		<label class="flex items-center gap-1">
			Min
			<input
				class={inputClass}
				class:border-red-500={hasIssue(issues, 'minReplicas')}
				bind:value={form.minReplicas}
				placeholder={placeholders.minReplicas ?? ''}
				disabled={disabled || form.parked}
			/>
		</label>
		<label class="flex items-center gap-1">
			Max
			<input
				class={inputClass}
				class:border-red-500={hasIssue(issues, 'maxReplicas')}
				bind:value={form.maxReplicas}
				placeholder={placeholders.maxReplicas ?? ''}
				disabled={disabled || form.parked}
			/>
		</label>
		{#if allowParked}
			<label class="flex items-center gap-1" title="Scale this Fleet to zero, with no autoscaler">
				<input type="checkbox" bind:checked={form.parked} {disabled} />
				Parked
			</label>
		{/if}
	</div>
	{#each issues as issue}
		<span class="text-xs text-red-400">{sizingIssueText(issue)}</span>
	{/each}
</div>
