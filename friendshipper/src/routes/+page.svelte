<script lang="ts">
	import { appConfig } from '$lib/stores.js';
	import PlaytestLayout from '$lib/components/home/PlaytestLayout.svelte';
	import ContributorLayout from '$lib/components/home/ContributorLayout.svelte';
</script>

{#if $appConfig.repoUrl === ''}
	<PlaytestLayout />
{:else}
	<ContributorLayout />
{/if}
