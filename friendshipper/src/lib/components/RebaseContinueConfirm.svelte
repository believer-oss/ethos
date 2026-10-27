<script lang="ts">
	import { Button, Modal } from 'flowbite-svelte';
	import { createEventDispatcher } from 'svelte';
	import { confirmLine, sha7 } from '$lib/branches';
	import type { ConflictFile, Nullable, RebaseCommitInfo } from '$lib/types';

	export let showModal: boolean;
	export let step: number;
	export let totalSteps: number;
	export let currentCommit: Nullable<RebaseCommitInfo>;
	export let conflicts: ConflictFile[];

	const dispatch = createEventDispatcher<{ confirm: null; back: null }>();

	const handleBack = () => {
		showModal = false;
		dispatch('back', null);
	};

	const handleConfirm = () => {
		showModal = false;
		dispatch('confirm', null);
	};
</script>

<Modal
	defaultClass="bg-secondary-700 dark:bg-space-900 overflow-y-auto"
	bodyClass="!border-t-0"
	backdropClass="fixed mt-8 inset-0 z-40 bg-gray-900 bg-opacity-50 dark:bg-opacity-80"
	dialogClass="fixed mt-8 top-0 start-0 end-0 h-modal md:inset-0 md:h-full z-50 w-full p-4 pb-12 flex"
	dismissable={false}
	size="lg"
	bind:open={showModal}
>
	<p class="text-xl text-primary-400">
		Confirm your choices for commit {step} of {totalSteps}{#if currentCommit}:
			<span class="font-mono">{sha7(currentCommit.sha)}</span> "{currentCommit.subject}" by {currentCommit.author}{/if}
	</p>
	<div
		class="flex flex-col gap-2 max-h-[50vh] overflow-y-auto bg-secondary-800 dark:bg-space-950 p-2 rounded"
		role="list"
	>
		{#each conflicts as f (f.path)}
			<div class="flex flex-col text-sm" role="listitem">
				<span class="text-white break-all">{f.displayName}</span>
				<code class="text-xs text-gray-400 break-all">{f.path}</code>
				<span class="text-xs text-primary-300">{confirmLine(f)}</span>
			</div>
		{/each}
	</div>
	<p class="text-sm text-yellow-300">
		After you confirm, these choices are committed and can't be changed except by aborting the whole
		rebase.
	</p>
	<div class="flex flex-row-reverse gap-2">
		<Button size="sm" on:click={handleConfirm}>Confirm and continue</Button>
		<Button size="sm" color="alternative" on:click={handleBack}>Back</Button>
	</div>
</Modal>
