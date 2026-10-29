<script lang="ts">
	// Gates a write control on the Release controls preference: the control stays visible but is
	// disabled, with a tooltip saying where to turn it on. Use as
	// <ReleaseGate let:disabled><Button {disabled} …/></ReleaseGate>.
	import { Tooltip } from 'flowbite-svelte';
	import { releaseControlsEnabled } from '$lib/stores';
	import { writeControl } from '$lib/fleetFormat';

	$: gate = writeControl($releaseControlsEnabled);
</script>

<!-- A disabled button gets no pointer events, so the wrapper carries the hover for the tooltip. -->
<span class="inline-flex" class:cursor-not-allowed={gate.disabled}>
	<span class="contents" class:pointer-events-none={gate.disabled}>
		<slot disabled={gate.disabled} />
	</span>
</span>
{#if gate.disabled}
	<Tooltip class="text-xs">{gate.tooltip}</Tooltip>
{/if}
