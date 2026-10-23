<script lang="ts">
	import { Button, Input, Label, Modal } from 'flowbite-svelte';
	import { createEventDispatcher } from 'svelte';
	import type { Commit } from '@ethos/core';
	import { defaultPrTitle, prTitleError } from '$lib/branches';
	import type { Nullable } from '$lib/types';

	export let open: boolean;
	export let mode: 'create' | 'enqueue';
	export let branch: string;
	export let trunk: string;
	export let commitCount: number;
	export let commits: Commit[];
	export let prNumber: Nullable<number>;
	export let busy: boolean;

	const MAX_LISTED_COMMITS = 10;

	const dispatch = createEventDispatcher<{ create: { title: string }; enqueue: null }>();

	let title = '';
	let touched = false;
	let wasOpen = false;

	// Reset the title to the default every time the modal opens.
	$: if (open !== wasOpen) {
		wasOpen = open;
		if (open) {
			title = defaultPrTitle(branch, trunk);
			touched = false;
		}
	}

	$: titleError = mode === 'create' ? prTitleError(title) : null;
	$: canConfirm = !busy && titleError === null;
	$: listed = commits.slice(0, MAX_LISTED_COMMITS);

	const subject = (c: Commit): string => (c.message ?? '').trim().split('\n')[0];

	const onTitleInput = () => {
		touched = true;
	};

	const handleCancel = () => {
		open = false;
	};

	const handleConfirm = () => {
		if (!canConfirm) return;
		if (mode === 'create') {
			dispatch('create', { title: title.trim() });
		} else {
			dispatch('enqueue', null);
		}
	};
</script>

<Modal
	defaultClass="bg-secondary-700 dark:bg-space-900 overflow-y-auto"
	bodyClass="!border-t-0"
	backdropClass="fixed mt-8 inset-0 z-40 bg-gray-900 bg-opacity-50 dark:bg-opacity-80"
	dialogClass="fixed mt-8 top-0 start-0 end-0 h-modal md:inset-0 md:h-full z-50 w-full p-4 pb-12 flex"
	size="md"
	outsideclose
	{open}
	on:close={handleCancel}
>
	{#if mode === 'create'}
		<p class="text-xl text-primary-400">Create pull request</p>
		<p class="text-sm text-gray-300">
			Create a pull request from <code>{branch}</code> into <code>{trunk}</code> with {commitCount}
			commit(s). It opens in your browser; add it to the merge queue from this page when it is ready.
		</p>
		{#if listed.length > 0}
			<ul class="text-sm text-gray-300 flex flex-col gap-1">
				{#each listed as commit}
					<li class="truncate" title={commit.message}>
						<span class="font-mono text-primary-400">{commit.sha.substring(0, 8)}</span>
						{subject(commit)}
					</li>
				{/each}
				{#if commitCount > listed.length}
					<li class="text-gray-400">…and {commitCount - listed.length} more</li>
				{/if}
			</ul>
		{/if}
		<div class="flex flex-col gap-1">
			<Label for="branch-pr-title" class="text-white dark:text-white">Title</Label>
			<Input id="branch-pr-title" type="text" bind:value={title} on:input={onTitleInput} />
			{#if touched && titleError}
				<p class="text-sm text-red-500">{titleError}</p>
			{/if}
		</div>
	{:else}
		<p class="text-xl text-primary-400">Add to merge queue</p>
		<p class="text-sm text-gray-300">
			Add PR #{prNumber} to the merge queue. It is queued in the background once GitHub reports it mergeable;
			you will be notified of the result.
		</p>
	{/if}
	<div class="flex flex-row-reverse gap-2">
		<Button class="bg-lime-700 dark:bg-lime-700" disabled={!canConfirm} on:click={handleConfirm}>
			{mode === 'create' ? 'Create PR' : 'Add to merge queue'}
		</Button>
		<Button color="alternative" on:click={handleCancel}>Cancel</Button>
	</div>
</Modal>
