<script lang="ts">
	import { CheckStatus } from '$lib/types';

	export let divClass: string = 'text-2xl';

	export let checkStatus: CheckStatus;

	/** Shown on hover. Optional - callers that have nothing to add leave it unset. */
	export let hint: string = '';

	// A bare emoji tells a screen reader nothing, so each state carries a word.
	const labels: Record<CheckStatus, string> = {
		[CheckStatus.Loading]: 'checking',
		[CheckStatus.Success]: 'passed',
		[CheckStatus.Failure]: 'failed',
		[CheckStatus.Unknown]: 'cannot tell'
	};
</script>

{#if checkStatus === CheckStatus.Loading}
	<div class={divClass} role="img" aria-label={hint || labels[checkStatus]} title={hint}>🤔</div>
{:else if checkStatus === CheckStatus.Success}
	<div class={divClass} role="img" aria-label={hint || labels[checkStatus]} title={hint}>✅</div>
{:else if checkStatus === CheckStatus.Failure}
	<div class={divClass} role="img" aria-label={hint || labels[checkStatus]} title={hint}>❌</div>
{:else if checkStatus === CheckStatus.Unknown}
	<div class={divClass} role="img" aria-label={hint || labels[checkStatus]} title={hint}>🤷</div>
{/if}
