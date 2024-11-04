<script lang="ts">
	import { Button, Spinner } from 'flowbite-svelte';
	import { CirclePlusOutline } from 'flowbite-svelte-icons';

	// We get our playtests from the parent store
	import { onMount } from 'svelte';
	import { builds, playtests } from '$lib/stores';
	import type { Playtest } from '$lib/types';
	import { getPlaytests, ModalState } from '$lib/playtests';
	import PlaytestCard from '$lib/components/playtests/PlaytestCard.svelte';
	import PlaytestModal from '$lib/components/playtests/PlaytestModal.svelte';
	import { handleError } from '$lib/utils';
	import { getBuilds } from '$lib/builds';

	let loading = false;
	let loadingMessage = 'Loading...';
	let showModal = false;
	let selectedPlaytest: Playtest | null = null;
	let modalMode = ModalState.Creating;

	const handleCreatePlaytest = async () => {
		loadingMessage = 'Fetching latest builds...';
		loading = true;
		modalMode = ModalState.Creating;
		selectedPlaytest = null;
		builds.set(await getBuilds(250));
		loading = false;

		showModal = true;
	};

	const handleEditPlaytest = async (playtest: Playtest | null) => {
		loadingMessage = 'Fetching latest builds...';
		loading = true;
		modalMode = ModalState.Editing;
		selectedPlaytest = playtest;
		builds.set(await getBuilds(250));
		loading = false;

		showModal = true;
	};

	const updatePlaytests = async () => {
		loading = true;
		try {
			playtests.set(await getPlaytests());
		} catch (e) {
			await handleError(e);
		} finally {
			loading = false;
		}
	};

	const handleModalSubmit = async () => {
		loading = true;
		try {
			await updatePlaytests();
		} catch (e) {
			await handleError(e);
		} finally {
			loading = false;
		}
	};

	onMount(() => {
		void updatePlaytests();
	});
</script>

<div class="flex items-center gap-2">
	<p class="text-2xl my-2 text-primary-400 dark:text-primary-400">Playtests</p>
	<Button
		class="!p-1.5"
		size="xs"
		disabled={loading}
		on:click={async () => {
			await handleCreatePlaytest();
		}}
	>
		<CirclePlusOutline class="w-4 h-4" />
	</Button>
	{#if loading}
		<div class="flex items-center gap-2">
			<Spinner size="4" />
			<code class="text-xs text-gray-400 dark:text-gray-400">{loadingMessage}</code>
		</div>
	{/if}
</div>
<div class="flex flex-col gap-2 mb-2 overflow-auto">
	{#each $playtests as playtest}
		<PlaytestCard
			{playtest}
			handleEditPlaytest={async () => {
				await handleEditPlaytest(playtest);
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
