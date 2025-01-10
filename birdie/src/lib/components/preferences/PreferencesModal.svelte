<script lang="ts">
	import {
		Accordion,
		AccordionItem,
		Button,
		Card,
		Label,
		Input,
		Modal,
		Tooltip,
		Spinner,
		ButtonGroup
	} from 'flowbite-svelte';
	import { FolderOpenSolid, CodeBranchSolid, TerminalSolid } from 'flowbite-svelte-icons';
	import { emit } from '@tauri-apps/api/event';
	import { open } from '@tauri-apps/api/dialog';
	import { appConfig } from '$lib/stores';
	import type { BirdieConfig } from '$lib/types';
	import { getAppConfig, updateAppConfig } from '$lib/config';
	import { openTerminalToPath } from '$lib/system';
	import { refetchRepo } from '$lib/repo';

	export let showModal: boolean;
	export let requestInFlight: boolean;
	export let showProgressModal: boolean;
	export let handleCheckForUpdates: () => Promise<void>;
	export let progressModalTitle: string;

	let localAppConfig: BirdieConfig = {
		repoPath: '',
		repoUrl: '',
		toolsPath: '',
		toolsUrl: '',
		userDisplayName: '',
		githubPAT: '',
		initialized: false
	};
	let checkForUpdatesInFlight = false;

	const onOpen = () => {
		localAppConfig = structuredClone($appConfig);
	};

	const openRepoFolder = async () => {
		const repoPath = await open({
			directory: true,
			multiple: false,
			defaultPath: localAppConfig.repoPath || '.',
			title: 'Select game repository folder'
		});
		if (typeof repoPath === 'string') {
			localAppConfig.repoPath = repoPath;
		}
	};

	const openToolsFolder = async () => {
		const toolsPath = await open({
			directory: true,
			multiple: false,
			defaultPath: localAppConfig.toolsPath || '.',
			title: 'Select game repository folder'
		});
		if (typeof toolsPath === 'string') {
			localAppConfig.toolsPath = toolsPath;
		}
	};

	const openTerminalToRepo = async () => {
		await openTerminalToPath(localAppConfig.repoPath);
	};

	const openTerminalToTools = async () => {
		await openTerminalToPath(localAppConfig.toolsPath);
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

	const handleRefetchRepo = async () => {
		try {
			showModal = false;
			showProgressModal = true;
			progressModalTitle = 'Refetching repo...';
			await refetchRepo();

			await emit('success', 'Repo fetch complete.');
		} catch (e) {
			showModal = false;
			await emit('error', e);
		}
		showProgressModal = false;
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
					<Button class="h-8 gap-2" on:click={openRepoFolder}>
						<FolderOpenSolid />
						Browse
					</Button>
					<ButtonGroup class="w-full">
						<Input class="h-8" bind:value={localAppConfig.repoPath} />
						<Tooltip class="text-sm" placement="bottom">
							Specified folder must be a game repository.
						</Tooltip>
						<Button
							class="h-8 bg-primary-700 hover:bg-primary-800 dark:bg-primary-600 hover:dark:bg-primary-700"
							disabled={!localAppConfig.repoPath}
							on:click={openTerminalToRepo}
						>
							<TerminalSolid class="w-4 h-4" color="white" />
						</Button>
						<Tooltip class="text-sm w-max" placement="bottom">
							Open powershell to git repo path.
						</Tooltip>
					</ButtonGroup>
				</div>

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

				<div class="rounded-lg border border-white mt-4">
					<div class="mt-2 mb-2 ml-2 mr-2">
						<Label>Tools Path</Label>
						<div class="flex gap-1 mb-2">
							<Button class="h-8 gap-2" on:click={openToolsFolder}>
								<FolderOpenSolid />
								Browse
							</Button>
							<ButtonGroup class="w-full">
								<Input class="h-8" bind:value={localAppConfig.toolsPath} />
								<Tooltip class="text-sm" placement="bottom">
									Specified folder must be a game repository.
								</Tooltip>
								<Button
									class="h-8 bg-primary-700 hover:bg-primary-800 dark:bg-primary-600 hover:dark:bg-primary-700"
									disabled={!localAppConfig.toolsPath}
									on:click={openTerminalToTools}
								>
									<TerminalSolid class="w-4 h-4" color="white" />
								</Button>
								<Tooltip class="text-sm w-max" placement="bottom">
									Open powershell to tools path.
								</Tooltip>
							</ButtonGroup>
						</div>

						<Label class="text-white">Tools URL</Label>
						<div class="flex gap-1 mb-2">
							<Input
								class="h-8 text-white bg-secondary-800 dark:bg-space-950 border-gray-400"
								bind:value={localAppConfig.toolsUrl}
							/>
						</div>
						<Tooltip class="text-sm" placement="bottom">
							Specified URL should be a git URL ending in <code>.git</code>.
						</Tooltip>
					</div>
				</div>
			</div>
		</div>
	</div>

	<Card
		class="w-full p-2 sm:p-2 max-w-full bg-red-800 dark:bg-red-800 max-h-screen overflow-auto border-0 shadow-none"
	>
		<Accordion
			border={false}
			activeClass="hover:bg-red-900 focus:ring-0 text-white overflow-auto py-2 border-0 rounded-xl"
			inactiveClass="hover:bg-red-900 text-white py-2 border-0 border-t-0 rounded-xl"
			class="w-full"
		>
			<AccordionItem
				class="w-full"
				borderClass="border-0"
				borderOpenClass="border-0"
				borderBottomClass="border-0"
			>
				<div slot="header" class="flex items-center justify-between w-full pr-2">
					<div class="w-1/3">Danger Zone</div>
					<span class="text-xs text-gray-300 font-mono w-3/4">In case of emergency...</span>
				</div>
				<div class="flex flex-col gap-2 text-white">
					<div class="flex gap-2 items-center">
						<Button
							outline
							class="w-1/2 border-white dark:border-white text-white dark:text-white hover:bg-red-900 dark:hover:bg-red-900"
							on:click={handleRefetchRepo}
							>Refresh Repo and Commit Graph
						</Button>
						<span class="w-full">Refetch the repo from github and rebuild the commit-graph</span>
					</div>
				</div>
			</AccordionItem>
		</Accordion>
	</Card>

	<div class="flex flex-row-reverse gap-2">
		<Button outline on:click={onDiscardClicked}>Discard</Button>
		<Button on:click={onApplyClicked}>Apply</Button>
	</div>
</Modal>
