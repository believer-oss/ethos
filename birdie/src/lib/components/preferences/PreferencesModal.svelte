<script lang="ts">
	import { Button, Label, Input, Modal, Tooltip, Spinner } from 'flowbite-svelte';
	import { FolderOpenSolid, CodeBranchSolid } from 'flowbite-svelte-icons';
	import { emit } from '@tauri-apps/api/event';
	import { open } from '@tauri-apps/api/dialog';
	import { appConfig } from '$lib/stores';
	import type { AppConfig } from '$lib/types';
	import { getAppConfig, updateAppConfig } from '$lib/config';

	export let showModal: boolean;
	export let requestInFlight: boolean;
	export let showProgressModal: boolean;
	export let handleCheckForUpdates: () => Promise<void>;

	let localAppConfig: AppConfig = {};
	let checkForUpdatesInFlight = false;

	const onOpen = () => {
		localAppConfig = structuredClone($appConfig);
	};

	const openRepoFolder = async () => {
		localAppConfig.repoPath = await open({
			directory: true,
			multiple: false,
			defaultPath: localAppConfig.repoPath || '.',
			title: 'Select game repository folder'
		});
	};

	const onApplyClicked = async () => {
		// show the progress modal if the repo URL has changed
		const shouldShowProgressModal = $appConfig.repoUrl !== localAppConfig.repoUrl;
		const internal = async () => {
			try {
				await updateAppConfig(localAppConfig);
				await emit('success', 'Preferences saved.');
			} catch (e) {
				await emit('error', e);
			}

			try {
				$appConfig = await getAppConfig();
			} catch (e) {
				await emit('error', e);
			}
			requestInFlight = false;
		};

		requestInFlight = true;
		showModal = false;
		await internal();

		await emit('preferences-closed');

		if (shouldShowProgressModal) {
			showProgressModal = true;
			await internal();
			showProgressModal = false;
		} else {
			await internal();
		}
	};

	const onDiscardClicked = () => {
		showModal = false;
		void emit('preferences-closed');
	};
</script>

<Modal
	defaultClass="dark:bg-secondary-800 overflow-y-auto"
	bodyClass="!border-t-0"
	backdropClass="fixed mt-8 inset-0 z-40 bg-gray-900 bg-opacity-50 dark:bg-opacity-80"
	dialogClass="fixed mt-8 top-0 start-0 end-0 h-modal md:inset-0 md:h-full z-50 w-full p-4 pb-12 flex"
	bind:open={showModal}
	dismissable
	autoclose={false}
	on:open={onOpen}
>
	<div class="flex items-center justify-between gap-2">
		<p class="text-2xl dark:text-primary-400 mb-2">Preferences</p>
		<Button
			outline
			class="mr-7"
			on:click={async () => {
				checkForUpdatesInFlight = true;
				await handleCheckForUpdates();
				checkForUpdatesInFlight = false;
			}}
		>
			<span>Check for updates</span>
			{#if checkForUpdatesInFlight}
				<Spinner class=" ml-2 w-4 h-4" />
			{/if}
		</Button>
	</div>

	<h1 class="text-primary-600 text-base font-semibold mt-8 mb-4 flex gap-2 items-center">
		<CodeBranchSolid />
		Source Control Options
	</h1>
	<div class="rounded-lg border border-white">
		<div class="mt-4 mb-4 ml-4 mr-4">
			<div class="flex flex-col gap-2">
				<Label>Repo Path</Label>
				<div class="flex gap-1 mb-2">
					<Button class="h-8 gap-2" on:click={openRepoFolder}><FolderOpenSolid />Browse</Button>
					<Input class="h-8" bind:value={localAppConfig.repoPath} />
				</div>
				<Tooltip class="text-sm" placement="bottom">
					Specified folder must be a game repository.
				</Tooltip>

				<Label class="text-white">Repo URL</Label>
				<div class="flex gap-1 mb-2">
					<Input
						class="h-8 text-white bg-secondary-800 dark:bg-space-950 border-gray-400"
						bind:value={localAppConfig.repoUrl}
					/>
				</div>
				<Tooltip class="text-sm" placement="bottom">
					Specified URL should be a git URL ending in <code>.git</code>.
				</Tooltip>

				<Label>Github PAT</Label>
				<Input class="h-8" bind:value={localAppConfig.githubPAT} type="password" />
				<Tooltip class="text-sm" placement="bottom">
					Copy and paste your GitHub Personal Access Token (PAT) here.
				</Tooltip>
			</div>
		</div>
	</div>

	<div class="flex flex-row-reverse gap-2">
		<Button outline on:click={onDiscardClicked}>Discard</Button>
		<Button on:click={onApplyClicked}>Apply</Button>
	</div>
</Modal>
