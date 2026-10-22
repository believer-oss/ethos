<script lang="ts">
	import { Alert, Button, Card } from 'flowbite-svelte';
	import { ExclamationCircleSolid, InfoCircleSolid, PlusOutline } from 'flowbite-svelte-icons';
	import { ProgressModal } from '@ethos/core';
	import { repoConfig, repoStatus } from '$lib/stores';
	import {
		isFeatureBranch,
		isProtectedBranch,
		isQuickSubmitBranch,
		performSwitch
	} from '$lib/branches';
	import BranchSelector from '$lib/components/BranchSelector.svelte';
	import CreateBranchModal from '$lib/components/CreateBranchModal.svelte';

	let busy = false;
	let showProgressModal = false;
	let progressModalTitle = '';
	let createOpen = false;

	const infoAlertClass =
		'bg-secondary-700 dark:bg-space-900 border border-primary-500 text-white dark:text-white';
	const warningAlertClass =
		'bg-yellow-900 dark:bg-yellow-900 border border-yellow-700 text-yellow-100 dark:text-yellow-100';

	$: branch = $repoStatus?.branch ?? '';

	const openCreate = () => {
		createOpen = true;
	};

	const handleBranchSelected = async (target: string) => {
		if (target === $repoStatus?.branch) return;
		busy = true;
		showProgressModal = true;
		progressModalTitle = `Switching to ${target}`;
		try {
			await performSwitch(target);
		} finally {
			showProgressModal = false;
			busy = false;
		}
	};
</script>

<div class="flex flex-col gap-2 w-full">
	<Card
		class="w-full p-4 sm:p-4 max-w-full bg-secondary-700 dark:bg-space-900 border-0 shadow-none"
	>
		<div class="flex flex-row items-center gap-2">
			<BranchSelector
				current={branch}
				disabled={busy}
				on:select={(e) => handleBranchSelected(e.detail)}
			/>
			<Button size="xs" disabled={busy} on:click={openCreate}>
				<PlusOutline class="w-4 h-4 me-2" />
				New branch
			</Button>
		</div>
	</Card>

	{#if isProtectedBranch(branch, $repoConfig)}
		<Alert class={infoAlertClass}>
			<InfoCircleSolid slot="icon" class="w-5 h-5 shrink-0 text-primary-400" />
			You're on <code>{branch}</code>. Create or select a feature branch to manage it.
		</Alert>
	{:else if isQuickSubmitBranch(branch)}
		<Alert class={infoAlertClass}>
			<InfoCircleSolid slot="icon" class="w-5 h-5 shrink-0 text-primary-400" />
			You're on <code>{branch}</code>, a Quick Submit branch. Create or select a feature branch to
			manage it.
		</Alert>
	{:else if branch && !isFeatureBranch(branch)}
		<Alert class={warningAlertClass}>
			<ExclamationCircleSolid slot="icon" class="w-5 h-5 shrink-0 text-yellow-300" />
			<code>{branch}</code> was not created by Friendshipper. It may be shared or protected differently,
			but actions are allowed.
		</Alert>
	{/if}
</div>

<CreateBranchModal bind:open={createOpen} />

<ProgressModal showModal={showProgressModal} title={progressModalTitle} />
