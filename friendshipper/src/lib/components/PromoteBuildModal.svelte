<script lang="ts">
	import { Modal, Button, Input, Label, Spinner } from 'flowbite-svelte';
	import { emit } from '@tauri-apps/api/event';
	import { createPromoteBuildWorkflow, type CreatePromoteBuildWorkflowRequest } from '$lib/builds';

	export let showModal: boolean = false;
	export let commit: string = '';

	let gameRepo: string = '';
	let gameConfig: string = '';
	let metadataPath: string = '';
	let loading: boolean = false;
	let error: string = '';

	const handleSubmit = async () => {
		if (!gameRepo || !gameConfig || !metadataPath || !commit) {
			error = 'All fields are required';
			return;
		}

		loading = true;
		error = '';

		try {
			const request: CreatePromoteBuildWorkflowRequest = {
				game_repo: gameRepo,
				game_config: gameConfig,
				metadata_path: metadataPath,
				commit
			};

			const workflow = await createPromoteBuildWorkflow(request);
			await emit('info', `Successfully created promote build workflow: ${workflow.metadata.name}`);

			// Reset form and close modal
			gameRepo = '';
			gameConfig = '';
			metadataPath = '';
			commit = '';
			showModal = false;
		} catch (e) {
			error = `Failed to create promote build workflow: ${
				e instanceof Error ? e.message : String(e)
			}`;
			await emit('error', error);
		} finally {
			loading = false;
		}
	};

	const handleCancel = () => {
		// Reset form
		gameRepo = '';
		gameConfig = '';
		metadataPath = '';
		error = '';
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

		<div class="flex flex-col gap-3">
			<div>
				<Label for="gameRepo" class="text-primary-400 mb-2">Game Repository</Label>
				<Input
					id="gameRepo"
					bind:value={gameRepo}
					placeholder="e.g., fellowship"
					class="bg-secondary-600 dark:bg-space-800 text-white border-gray-500"
					disabled={loading}
				/>
			</div>

			<div>
				<Label for="gameConfig" class="text-primary-400 mb-2">Game Configuration</Label>
				<Input
					id="gameConfig"
					bind:value={gameConfig}
					placeholder="e.g., development"
					class="bg-secondary-600 dark:bg-space-800 text-white border-gray-500"
					disabled={loading}
				/>
			</div>

			<div>
				<Label for="metadataPath" class="text-primary-400 mb-2">Metadata Path</Label>
				<Input
					id="metadataPath"
					bind:value={metadataPath}
					placeholder="e.g., latest-2.0"
					class="bg-secondary-600 dark:bg-space-800 text-white border-gray-500"
					disabled={loading}
				/>
			</div>
		</div>

		{#if error}
			<div class="text-red-400 text-sm bg-red-900/20 p-2 rounded">
				{error}
			</div>
		{/if}

		<div class="flex flex-row-reverse gap-2 pt-2">
			<Button
				class="bg-lime-700 dark:bg-lime-700 hover:bg-lime-600 dark:hover:bg-lime-600"
				disabled={loading}
				on:click={handleSubmit}
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
	</div>
</Modal>
