<script lang="ts">
	import { Alert, Button, Checkbox, Label, Modal } from 'flowbite-svelte';
	import { ExclamationCircleSolid } from 'flowbite-svelte-icons';
	import { createEventDispatcher } from 'svelte';
	import type { RebasePreflight } from '$lib/types';

	export let showModal: boolean;
	export let branch: string;
	export let trunk: string;
	export let predictedConflicts: string[];
	export let preflight: RebasePreflight;

	const MAX_LISTED_CONFLICTS = 20;
	const MAX_LISTED_CPP = 5;

	const dispatch = createEventDispatcher<{ start: null }>();

	const conflictAlertClass =
		'bg-red-900 dark:bg-red-900 border border-red-700 text-red-100 dark:text-red-100';
	const cppAlertClass =
		'bg-yellow-900 dark:bg-yellow-900 border border-yellow-700 text-yellow-100 dark:text-yellow-100';

	let understood = false;
	let wasOpen = false;

	// Reset on open, so a dismissed modal never carries the acknowledgement into the next rebase.
	$: if (showModal !== wasOpen) {
		wasOpen = showModal;
		if (showModal) {
			understood = false;
		}
	}

	$: hasConflicts = predictedConflicts.length > 0;
	$: canStart = !hasConflicts || understood;
	$: cppFiles = preflight.cppFiles.slice(0, MAX_LISTED_CPP);

	const handleCancel = () => {
		showModal = false;
	};

	const handleStart = () => {
		if (!canStart) return;
		showModal = false;
		dispatch('start', null);
	};
</script>

<Modal
	defaultClass="bg-secondary-700 dark:bg-space-900 overflow-y-auto"
	bodyClass="!border-t-0"
	backdropClass="fixed mt-8 inset-0 z-40 bg-gray-900 bg-opacity-50 dark:bg-opacity-80"
	dialogClass="fixed mt-8 top-0 start-0 end-0 h-modal md:inset-0 md:h-full z-50 w-full p-4 pb-12 flex"
	dismissable={false}
	size="md"
	outsideclose
	bind:open={showModal}
>
	<p class="text-xl text-primary-400">Rebase onto latest {trunk}</p>
	<p class="text-sm text-gray-300">
		Rebasing replays {preflight.commitCount}
		commit{preflight.commitCount === 1 ? '' : 's'} of <code>{branch}</code> on top of the latest
		<code>{trunk}</code>. You review the result and push the rebased branch in a separate step.
		History is rewritten: collaborators must Sync after the push. A backup is kept for 14 days.
	</p>

	{#if hasConflicts}
		<Alert class={conflictAlertClass}>
			<ExclamationCircleSolid slot="icon" class="w-5 h-5 shrink-0 text-red-300" />
			<p class="text-sm">
				{predictedConflicts.length} file(s) changed on both sides and will conflict:
			</p>
			<ul class="font-mono text-xs pl-4 max-h-40 overflow-y-auto">
				{#each predictedConflicts.slice(0, MAX_LISTED_CONFLICTS) as path}
					<li class="break-all">{path}</li>
				{/each}
			</ul>
			{#if predictedConflicts.length > MAX_LISTED_CONFLICTS}
				<p class="pl-4 text-xs">…and {predictedConflicts.length - MAX_LISTED_CONFLICTS} more</p>
			{/if}
			<Label class="flex flex-row items-center gap-2 mt-2 text-sm text-white dark:text-white">
				<Checkbox bind:checked={understood} />
				I understand I will have to resolve these conflicts
			</Label>
		</Alert>
	{/if}

	{#if preflight.touchesCpp}
		<Alert class={cppAlertClass}>
			<ExclamationCircleSolid slot="icon" class="w-5 h-5 shrink-0 text-yellow-300" />
			<p class="text-sm">
				This branch changes C++ (<span class="font-mono text-xs break-all"
					>{cppFiles.join(', ')}{preflight.cppFiles.length > MAX_LISTED_CPP ? '…' : ''}</span
				>). After the rebase, the editor binaries built for this branch no longer match; you will be
				offered an editor build.
			</p>
		</Alert>
	{/if}

	<div class="flex flex-row-reverse gap-2">
		<Button size="sm" disabled={!canStart} on:click={handleStart}>Start rebase</Button>
		<Button size="sm" color="alternative" on:click={handleCancel}>Cancel</Button>
	</div>
</Modal>
