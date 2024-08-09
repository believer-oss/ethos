<script lang="ts">
	import { Button, Spinner } from 'flowbite-svelte';
	import { CirclePlusOutline } from 'flowbite-svelte-icons';

	// We get our playtests from the parent store
	import { builds, playtests } from '$lib/stores';
	import type { Playtest } from '$lib/types';
	import { getPlaytests, ModalState } from '$lib/playtests';
	import PlaytestCard from '$lib/components/playtests/PlaytestCard.svelte';
	import PlaytestModal from '$lib/components/playtests/PlaytestModal.svelte';

	let loading = false;
	let showModal = false;
	let selectedPlaytest: Playtest | null = null;
	let modalMode = ModalState.Creating;

	const handleCreatePlaytest = () => {
		modalMode = ModalState.Creating;
		selectedPlaytest = null;
		showModal = true;
	};

	const handleEditPlaytest = (playtest: Playtest | null) => {
		modalMode = ModalState.Editing;
		selectedPlaytest = playtest;
		showModal = true;
	};

	const handleModalSubmit = async () => {
		loading = true;
		playtests.set(await getPlaytests());
		loading = false;
	};
</script>

<div class="flex items-center gap-2">
	<p class="text-2xl my-2 text-primary-400 dark:text-primary-400">Playtests</p>
	<Button
		class="!p-1.5"
		size="xs"
		on:click={() => {
			handleCreatePlaytest();
		}}
	>
		<CirclePlusOutline class="w-4 h-4" />
	</Button>
	{#if loading}
		<Spinner size="4" />
	{/if}
</div>
<div class="flex flex-col gap-2 mb-2 overflow-auto">
	{#each $playtests as playtest}
		<PlaytestCard
			{playtest}
			handleEditPlaytest={() => {
				handleEditPlaytest(playtest);
			}}
			bind:loading
		/>
	{/each}
</div>

<PlaytestModal
	playtest={selectedPlaytest}
	versions={$builds.entries}
	mode={modalMode}
	bind:showModal
	onSubmit={handleModalSubmit}
/>
