<script lang="ts">
	import { Modal, Button, Input, Label, Spinner, Select } from 'flowbite-svelte';
	import { emit } from '@tauri-apps/api/event';
	import { createPromoteBuildWorkflow, type CreatePromoteBuildWorkflowRequest } from '$lib/builds';
	import { dynamicConfig } from '$lib/stores';

	export let showModal: boolean = false;
	export let commit: string = '';

	let loading: boolean = false;
	let error: string = '';
	let showConfirmation: boolean = false;
	let showSuccess: boolean = false;
	let successWorkflowName: string = '';
	let selectedShardName: string = '';

	$: shardOptions = $dynamicConfig?.promotableBuildShards ?? [];
	$: hasShardOptions = shardOptions.length > 0;
	$: selectItems = shardOptions.map((s) => ({ value: s.shard, name: s.displayName }));
	$: selectedShard = shardOptions.find((s) => s.shard === selectedShardName);
	$: if (hasShardOptions && !selectedShardName) {
		selectedShardName = shardOptions[0].shard;
	}

	const handleInitialSubmit = () => {
		if (!commit) {
			error = 'Commit SHA is required';
			return;
		}

		error = '';
		showConfirmation = true;
	};

	const handleConfirmedSubmit = async () => {
		loading = true;
		showConfirmation = false;

		try {
			const request: CreatePromoteBuildWorkflowRequest = {
				commit,
				shard: selectedShard?.shard,
				metadata_path: selectedShard?.metadataPath
			};

			const workflow = await createPromoteBuildWorkflow(request);
			await emit('info', `Successfully created promote build workflow: ${workflow.metadata.name}`);

			// Show success modal
			successWorkflowName = workflow.metadata.name;
			showSuccess = true;
		} catch (e) {
			error = `Failed to create promote build workflow: ${
				e instanceof Error ? e.message : String(e)
			}`;
			await emit('error', error);
		} finally {
			loading = false;
		}
	};

	const handleCancelConfirmation = () => {
		showConfirmation = false;
	};

	const handleCancel = () => {
		// Reset form
		error = '';
		showConfirmation = false;
		showSuccess = false;
		showModal = false;
	};

	const handleSuccessClose = () => {
		// Reset form and close modal
		commit = '';
		error = '';
		showConfirmation = false;
		showSuccess = false;
		successWorkflowName = '';
		selectedShardName = hasShardOptions ? shardOptions[0].shard : '';
		showModal = false;
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
	<div class="flex flex-col gap-4">
		<div class="flex items-center gap-2">
			<p class="text-xl text-primary-400">Promote Build</p>
		</div>

		{#if !showSuccess}
			{#if hasShardOptions}
				<div>
					<Label for="shard" class="text-primary-400 mb-2">Target Shard</Label>
					<Select
						id="shard"
						bind:value={selectedShardName}
						items={selectItems}
						class="bg-secondary-600 dark:bg-space-800 text-white border-gray-500"
						disabled={loading}
					/>
				</div>
			{/if}

			<div>
				<Label for="commit" class="text-primary-400 mb-2">Commit SHA</Label>
				<Input
					id="commit"
					bind:value={commit}
					placeholder="40-character commit SHA"
					class="bg-secondary-600 dark:bg-space-800 text-white border-gray-500 font-mono"
					disabled={loading}
				/>
			</div>
		{/if}

		{#if error}
			<div class="text-red-400 text-sm bg-red-900/20 p-2 rounded">
				{error}
			</div>
		{/if}

		{#if showSuccess}
			<!-- Success Dialog -->
			<div class="bg-green-900/20 border border-green-700 p-4 rounded">
				<div class="flex items-start gap-3">
					<div class="text-green-400">
						<svg class="w-6 h-6 mt-0.5" fill="currentColor" viewBox="0 0 20 20">
							<path
								fill-rule="evenodd"
								d="M10 18a8 8 0 100-16 8 8 0 000 16zm3.707-9.293a1 1 0 00-1.414-1.414L9 10.586 7.707 9.293a1 1 0 00-1.414 1.414l2 2a1 1 0 001.414 0l4-4z"
								clip-rule="evenodd"
							/>
						</svg>
					</div>
					<div class="flex-1">
						<h3 class="text-lg font-medium text-green-400 mb-2">
							Promotion Workflow Created Successfully!
						</h3>
						<div class="text-sm text-gray-300 space-y-2">
							<p>
								The promotion workflow <strong class="text-white">{successWorkflowName}</strong> has
								been created and should start soon.
							</p>
							<div class="bg-secondary-600 dark:bg-space-800 p-3 rounded font-mono text-xs">
								<div><strong>SHA to be promoted:</strong> {commit}</div>
								{#if selectedShard}
									<div><strong>Target shard:</strong> {selectedShard.displayName}</div>
								{/if}
							</div>
							<p class="text-green-300">
								<strong>What happens next:</strong> Once the workflow completes successfully, launcher
								users will receive this build when they next launch the game.
							</p>
						</div>
					</div>
				</div>
			</div>

			<div class="flex justify-center pt-2">
				<Button
					class="bg-lime-700 dark:bg-lime-700 hover:bg-lime-600 dark:hover:bg-lime-600"
					on:click={handleSuccessClose}
				>
					Close
				</Button>
			</div>
		{:else if !showConfirmation}
			<div class="flex flex-row-reverse gap-2 pt-2">
				<Button
					class="bg-lime-700 dark:bg-lime-700 hover:bg-lime-600 dark:hover:bg-lime-600"
					disabled={loading}
					on:click={handleInitialSubmit}
				>
					{#if loading}
						<Spinner size="4" class="me-2" />
						Creating...
					{:else}
						Promote Build
					{/if}
				</Button>
				<Button
					class="bg-gray-700 dark:bg-gray-700 hover:bg-gray-600 dark:hover:bg-gray-600"
					disabled={loading}
					on:click={handleCancel}
				>
					Cancel
				</Button>
			</div>
		{:else}
			<!-- Confirmation Dialog -->
			<div class="bg-yellow-900/20 border border-yellow-700 p-4 rounded">
				<div class="flex items-start gap-3">
					<div class="text-yellow-400">
						<svg class="w-6 h-6 mt-0.5" fill="currentColor" viewBox="0 0 20 20">
							<path
								fill-rule="evenodd"
								d="M8.257 3.099c.765-1.36 2.722-1.36 3.486 0l5.58 9.92c.75 1.334-.213 2.98-1.742 2.98H4.42c-1.53 0-2.493-1.646-1.743-2.98l5.58-9.92zM11 13a1 1 0 11-2 0 1 1 0 012 0zm-1-8a1 1 0 00-1 1v3a1 1 0 002 0V6a1 1 0 00-1-1z"
								clip-rule="evenodd"
							/>
						</svg>
					</div>
					<div class="flex-1">
						<h3 class="text-lg font-medium text-yellow-400 mb-2">
							Are you sure you want to promote this build?
						</h3>
						<div class="text-sm text-gray-300 space-y-2">
							{#if selectedShard}
								<p>
									<strong>Warning:</strong> This action will deploy a new build to the
									<strong class="text-yellow-400">{selectedShard.displayName}</strong> shard that launcher
									users will download and use.
								</p>
								<p class="text-yellow-300">
									<strong>Impact:</strong> All users on the
									<strong>{selectedShard.displayName}</strong>
									shard will be prompted to download this build when they next launch the game.
								</p>
							{:else}
								<p>
									<strong>Warning:</strong> This action will deploy a new build that launcher users will
									download and use.
								</p>
								<p class="text-yellow-300">
									<strong>Impact:</strong> All users with the launcher will be prompted to download this
									build when they next launch the game.
								</p>
							{/if}
						</div>
					</div>
				</div>
			</div>

			<div class="flex flex-row-reverse gap-2 pt-2">
				<Button
					class="bg-lime-700 dark:bg-lime-700 hover:bg-lime-600 dark:hover:bg-lime-600"
					disabled={loading}
					on:click={handleConfirmedSubmit}
				>
					{#if loading}
						<Spinner size="4" class="me-2" />
						Creating...
					{:else}
						Yes, Promote Build
					{/if}
				</Button>
				<Button
					class="bg-gray-700 dark:bg-gray-700 hover:bg-gray-600 dark:hover:bg-gray-600"
					disabled={loading}
					on:click={handleCancelConfirmation}
				>
					Go Back
				</Button>
			</div>
		{/if}
	</div>
</Modal>
