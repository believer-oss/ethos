<script lang="ts">
	import { CheckStatus } from '$lib/types';

	export let divClass: string = 'text-2xl';

	export let checkStatus: CheckStatus;
</script>

{#if checkStatus === CheckStatus.Loading}
	<div class={divClass}>🤔</div>
{:else if checkStatus === CheckStatus.Success}
	<div class={divClass}>✅</div>
{:else if checkStatus === CheckStatus.Failure}
	<div class={divClass}>❌</div>
{/if}
