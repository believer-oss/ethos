<script lang="ts">
	import { Modal, Button, Checkbox, Input, Label, Spinner, Select, Tooltip } from 'flowbite-svelte';
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
	let selectedDestinationName: string = '';
	let deployBackend: boolean = true;
	let selectedSteamBranch: string = '';
	let lastDestinationName: string = '';

	$: destinations = $dynamicConfig?.promotableBuildShards ?? [];
	$: hasDestinations = destinations.length > 0;
	$: destinationItems = destinations.map((d) => ({ value: d.displayName, name: d.displayName }));
	$: selectedDestination = destinations.find((d) => d.displayName === selectedDestinationName);
	$: steamBranches = selectedDestination?.steamBranches ?? [];
	$: steamBranchItems = steamBranches.map((b) => ({ value: b, name: b }));

	// Two independent concerns, deliberately not conflated:
	//   isSteam              - drives Steam-specific UI and wording only.
	//   backendDeployDisabled - a config override that forces the backend deploy
	//                          off for this destination. Any destination can set
	//                          it; it is not a property of being Steam.
	$: isSteam = (selectedDestination?.distribution ?? '').toLowerCase() === 'steam';
	$: backendDeployDisabled = selectedDestination?.disableBackendDeploy === true;
	$: showSteamBranch = isSteam && steamBranches.length > 0;
	$: if (hasDestinations && !selectedDestinationName) {
		selectedDestinationName = destinations[0].displayName;
	}

	// Reset the backend deploy toggle and Steam branch whenever the destination
	// changes. Guarded on the previous name so toggling the checkbox does not
	// re-trigger this block and immediately revert the user's choice.
	$: if (selectedDestinationName !== lastDestinationName) {
		lastDestinationName = selectedDestinationName;
		deployBackend = !backendDeployDisabled;
		selectedSteamBranch = steamBranches[0] ?? '';
	}

	// The backend environment is deliberately three-state:
	//   undefined -> omitted, so the Argo workflow template's default applies
	//   ''        -> present and empty, suppressing the backend deploy
	//   value     -> present with the destination's configured environment
	// Never collapse undefined into '' - a destination that configures no
	// backend environment relies on the template default. The wire key stays
	// `shard`, matching the Argo template parameter.
	const computeEffectiveBackendEnvironment = (
		disabled: boolean,
		deploy: boolean,
		configuredEnvironment: string | undefined
	): string | undefined => {
		if (disabled) return '';
		if (!deploy) return '';
		return configuredEnvironment;
	};

	const computeBackendDeployLabel = (
		disabled: boolean,
		configuredEnvironment: string | undefined
	): string => {
		if (disabled) return 'Backend deploy (disabled for this destination)';
		if (configuredEnvironment) return `Backend deploy (${configuredEnvironment})`;
		return 'Backend deploy (workflow default)';
	};

	const computeBackendDeploySummary = (
		disabled: boolean,
		deploy: boolean,
		configuredEnvironment: string | undefined
	): string => {
		if (disabled) return 'none - disabled for this destination';
		if (!deploy) return 'none - no backend deploy';
		return configuredEnvironment ?? 'workflow default';
	};

	$: effectiveBackendEnvironment = computeEffectiveBackendEnvironment(
		backendDeployDisabled,
		deployBackend,
		selectedDestination?.shard
	);
	$: backendDeployLabel = computeBackendDeployLabel(
		backendDeployDisabled,
		selectedDestination?.shard
	);
	$: backendDeploySummary = computeBackendDeploySummary(
		backendDeployDisabled,
		deployBackend,
		selectedDestination?.shard
	);

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
				shard: effectiveBackendEnvironment,
				metadata_path: selectedDestination?.metadataPath,
				distribution: selectedDestination?.distribution,
				game_config: selectedDestination?.gameConfig,
				steam_branch: showSteamBranch ? selectedSteamBranch : undefined
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
		// Reset form. Clearing lastDestinationName re-arms the reset block so the
		// backend deploy toggle returns to its default instead of persisting an
		// uncheck into the next promotion of the same destination.
		error = '';
		showConfirmation = false;
		showSuccess = false;
		lastDestinationName = '';
		showModal = false;
	};

	const handleSuccessClose = () => {
		// Reset form and close modal
		commit = '';
		error = '';
		showConfirmation = false;
		showSuccess = false;
		successWorkflowName = '';
		selectedDestinationName = hasDestinations ? destinations[0].displayName : '';
		lastDestinationName = '';
		deployBackend = true;
		selectedSteamBranch = '';
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
			{#if hasDestinations}
				<div>
					<Label for="destination" class="text-primary-400 mb-2">Destination</Label>
					<Select
						id="destination"
						bind:value={selectedDestinationName}
						items={destinationItems}
						class="bg-secondary-600 dark:bg-space-800 text-white border-gray-500"
						disabled={loading}
					/>
				</div>

				<div class="flex flex-row gap-2">
					<Label class="flex flex-row text-xs text-white">
						<Checkbox
							name="deployBackend"
							bind:checked={deployBackend}
							disabled={backendDeployDisabled || loading}
						/>
						<span>{backendDeployLabel}</span>
						<Tooltip>
							When unchecked, the build ships without redeploying the backend - use this for
							client/server bug fixes on a long-lived backend environment.
						</Tooltip>
					</Label>
				</div>

				{#if showSteamBranch}
					<div>
						<Label for="steamBranch" class="text-primary-400 mb-2">Steam Branch</Label>
						<Select
							id="steamBranch"
							bind:value={selectedSteamBranch}
							items={steamBranchItems}
							class="bg-secondary-600 dark:bg-space-800 text-white border-gray-500"
							disabled={loading}
						/>
					</div>
				{/if}
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
								{#if selectedDestination}
									<div><strong>Destination:</strong> {selectedDestination.displayName}</div>
									<div><strong>Backend deploy:</strong> {backendDeploySummary}</div>
									{#if showSteamBranch}
										<div><strong>Steam branch:</strong> {selectedSteamBranch}</div>
									{/if}
								{/if}
							</div>
							<p class="text-green-300">
								<strong>What happens next:</strong>
								{#if isSteam}
									Once the workflow completes successfully, this build will be published to the
									<strong>{selectedSteamBranch}</strong> branch on Steam.
								{:else}
									Once the workflow completes successfully, launcher users will receive this build
									when they next launch the game.
								{/if}
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
							{#if selectedDestination}
								<p>
									<strong>Warning:</strong> This action will deploy a new build to the
									<strong class="text-yellow-400">{selectedDestination.displayName}</strong> destination
									that users will download and use.
								</p>
								<p class="text-yellow-300">
									<strong>Impact:</strong>
									{#if isSteam}
										This build will be published to the <strong>{selectedSteamBranch}</strong> branch
										on Steam.
									{:else}
										All users on the <strong>{selectedDestination.displayName}</strong> destination will
										be prompted to download this build when they next launch the game.
									{/if}
								</p>
								<div class="bg-secondary-600 dark:bg-space-800 p-3 rounded font-mono text-xs">
									<div><strong>Destination:</strong> {selectedDestination.displayName}</div>
									<div><strong>Backend deploy:</strong> {backendDeploySummary}</div>
									{#if showSteamBranch}
										<div><strong>Steam branch:</strong> {selectedSteamBranch}</div>
									{/if}
								</div>
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
