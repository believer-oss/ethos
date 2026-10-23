<script lang="ts">
	import { Button, Input, Modal } from 'flowbite-svelte';
	import { ProgressModal } from '@ethos/core';
	import { emit } from '@tauri-apps/api/event';
	import { createEventDispatcher, tick } from 'svelte';
	import { createBranch } from '$lib/repo';
	import {
		FEATURE_BRANCH_PREFIX,
		normalizeSlug,
		refreshAfterBranchChange,
		validateSlug
	} from '$lib/branches';
	import type { CreateBranchResponse, TauriError } from '$lib/types';

	export let open: boolean;

	const dispatch = createEventDispatcher<{ created: CreateBranchResponse }>();

	let input = '';
	let touched = false;
	let creating = false;
	let progressTitle = '';
	let serverError = '';
	let inputWrapper: HTMLElement;
	let wasOpen = false;

	$: slug = normalizeSlug(input);
	$: validationError = validateSlug(slug);
	$: canCreate = validationError === null && !creating;

	const focusInput = async () => {
		await tick();
		inputWrapper?.querySelector('input')?.focus();
	};

	$: if (open !== wasOpen) {
		wasOpen = open;
		if (open) {
			void focusInput();
		}
	}

	const onInput = () => {
		touched = true;
		serverError = '';
	};

	// The modal also emits close while hidden for the progress modal; ignore that.
	const handleCancel = () => {
		if (creating) return;
		open = false;
	};

	const handleCreate = async () => {
		if (!canCreate) return;
		creating = true;
		progressTitle = `Creating ${FEATURE_BRANCH_PREFIX}${slug}`;
		serverError = '';
		try {
			const res = await createBranch(slug);
			// Stay blocked until the status reflects the new branch.
			progressTitle = res.switched ? `Switching to ${res.branch}` : 'Refreshing status';
			await refreshAfterBranchChange();
			creating = false;
			open = false;
			input = '';
			touched = false;
			if (res.switched && res.message) {
				await emit('error', res.message);
			} else {
				await emit('success', res.switched ? `Switched to ${res.branch}` : res.message ?? '');
			}
			dispatch('created', res);
		} catch (e) {
			const err = e as TauriError;
			progressTitle = 'Refreshing status';
			await refreshAfterBranchChange();
			creating = false;
			if (err?.status_code === 400) {
				serverError = err.message;
			} else {
				await emit('error', e);
			}
		}
	};

	const onKeydown = (e: KeyboardEvent) => {
		if (e.key === 'Enter') {
			e.preventDefault();
			void handleCreate();
		}
	};
</script>

<Modal
	defaultClass="bg-secondary-700 dark:bg-space-900 overflow-y-auto"
	bodyClass="!border-t-0"
	backdropClass="fixed mt-8 inset-0 z-40 bg-gray-900 bg-opacity-50 dark:bg-opacity-80"
	dialogClass="fixed mt-8 top-0 start-0 end-0 h-modal md:inset-0 md:h-full z-50 w-full p-4 pb-12 flex"
	dismissable={!creating}
	size="sm"
	outsideclose
	open={open && !creating}
	on:close={handleCancel}
>
	<p class="text-xl text-primary-400">New branch</p>
	<div bind:this={inputWrapper}>
		<Input
			type="text"
			placeholder="my-feature"
			bind:value={input}
			on:input={onInput}
			on:keydown={onKeydown}
		/>
	</div>
	<p class="font-mono text-sm text-gray-300 break-all">{FEATURE_BRANCH_PREFIX}{slug}</p>
	{#if serverError}
		<p class="text-sm text-red-500">{serverError}</p>
	{:else if touched && validationError}
		<p class="text-sm text-red-500">{validationError}</p>
	{/if}
	<div class="flex flex-row-reverse gap-2">
		<Button class="bg-lime-700 dark:bg-lime-700" disabled={!canCreate} on:click={handleCreate}
			>Create</Button
		>
		<Button color="alternative" on:click={handleCancel}>Cancel</Button>
	</div>
</Modal>

<ProgressModal showModal={creating} title={progressTitle} />
