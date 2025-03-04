<script lang="ts">
	import {
		Accordion,
		AccordionItem,
		Button,
		ButtonGroup,
		Card,
		Checkbox,
		DarkMode,
		Input,
		Label,
		Modal,
		Radio,
		Select,
		Spinner,
		Toggle,
		Tooltip
	} from 'flowbite-svelte';
	import {
		AtomOutline,
		CloudArrowUpSolid,
		CodeBranchSolid,
		ExclamationCircleOutline,
		FolderOpenSolid,
		UserSolid,
		TerminalSolid
	} from 'flowbite-svelte-icons';
	import { emit } from '@tauri-apps/api/event';
	import { open } from '@tauri-apps/plugin-dialog';
	import { onDestroy } from 'svelte';
	import { ProgressModal } from '@ethos/core';
	import {
		appConfig,
		commits,
		changeSets,
		dynamicConfig,
		oktaAuth,
		playtests,
		repoStatus,
		startTime,
		engineWorkflows
	} from '$lib/stores';
	import { getAppConfig, resetConfig, updateAppConfig } from '$lib/config';
	import { resetLongtail, wipeClientData, getWorkflows } from '$lib/builds';
	import { openTerminalToPath, restart } from '$lib/system';
	import {
		resetRepo,
		refetchRepo,
		getRepoStatus,
		getAllCommits,
		saveChangeSet,
		loadChangeSet
	} from '$lib/repo';
	import { getPlaytests } from '$lib/playtests';
	import { regions } from '$lib/regions';
	import type { AppConfig, Nullable } from '$lib/types';

	export let showModal: boolean;
	export let requestInFlight: boolean;
	export let showProgressModal: boolean;
	export let progressModalTitle: string;
	export let handleCheckForUpdates: () => Promise<void>;

	let checkForUpdatesInFlight: boolean = false;
	let localAppConfig: AppConfig = {};
	let isEngineTypePrebuilt: boolean = false;
	let isEngineTypeSource: boolean = false;
	let configuringNewRepo: boolean = false;
	let repoName: string = '';
	let parentRepoPath: string = '';

	$: isEngineTypePrebuilt = localAppConfig.engineType === 'Prebuilt';
	$: isEngineTypeSource = localAppConfig.engineType === 'Source';
	let uptime = Math.floor((Date.now() - $startTime) / 1000);
	let uptimeInterval: ReturnType<typeof setInterval>;

	const formatUptime = (input: number) => {
		const hours = Math.floor(input / 3600)
			.toString()
			.padStart(2, '0');
		const minutes = Math.floor((input % 3600) / 60)
			.toString()
			.padStart(2, '0');
		const seconds = (input % 60).toString().padStart(2, '0');
		return `${hours}:${minutes}:${seconds}`;
	};

	const onOpen = () => {
		// refresh uptime in interval
		uptimeInterval = setInterval(() => {
			uptime = Math.floor((Date.now() - $startTime) / 1000);
		}, 1000);
		localAppConfig = structuredClone($appConfig);
		// initialize config types to empty object if needed
		if (!localAppConfig.oktaConfig) {
			localAppConfig.oktaConfig = {
				clientId: '',
				issuer: ''
			};
		}
		parentRepoPath = localAppConfig.repoPath.split('/').slice(0, -1).join('/');
		repoName = localAppConfig.repoPath.split('/').pop() || '';
	};

	const OnClose = () => {
		configuringNewRepo = false;
	};

	onDestroy(() => {
		clearInterval(uptimeInterval);
	});

	const onNewProjectClicked = () => {
		configuringNewRepo = true;
		localAppConfig.projects['new-project'] = {
			repoPath: '',
			repoUrl: ''
		};
		localAppConfig.selectedArtifactProject = 'new-project';
		repoName = '';
		parentRepoPath = '';
	};

	const onRepoUrlInput = (e: Event) => {
		const input = (e.target as HTMLInputElement).value;
		const githubUrlPattern = /[^/]+\/[^/]+\.git$/;

		if (githubUrlPattern.test(input)) {
			// Extract repo name from URL and use it as project name
			const parsedRepoName = input.split('/').pop()?.replace('.git', '');
			if (parsedRepoName) {
				repoName = parsedRepoName;
				// Create new project with owner-repo name
				const projectData = localAppConfig.projects[localAppConfig.selectedArtifactProject];

				// eslint-disable-next-line @typescript-eslint/no-dynamic-delete
				delete localAppConfig.projects[localAppConfig.selectedArtifactProject];

				// Get just the last two parts of the path (foo/bar.git)
				const parts = input.split('/');
				const owner = parts[parts.length - 2];
				const repo = parts[parts.length - 1].replace('.git', '');
				const projectName = `${owner}-${repo}`.toLowerCase();
				localAppConfig.projects[projectName] = projectData;
				localAppConfig.projects[projectName].repoPath = `${parentRepoPath}/${repoName}`;
				localAppConfig.selectedArtifactProject = projectName.toLowerCase();
			}
		}
	};

	const openRepoFolder = async () => {
		const openDir = await open({
			directory: true,
			multiple: false,
			defaultPath: localAppConfig.repoPath || '.',
			title: 'Select game repository folder'
		});

		if (openDir && typeof openDir === 'string') {
			parentRepoPath = openDir.replaceAll('\\', '/');
			localAppConfig.projects[
				localAppConfig.selectedArtifactProject
			].repoPath = `${parentRepoPath}/${repoName}`;
		}
	};

	const openEnginePrebuiltFolder = async () => {
		localAppConfig.enginePrebuiltPath = await open({
			directory: true,
			multiple: false,
			defaultPath: localAppConfig.enginePrebuiltPath || '.',
			title: 'Select prebuilt engine download location'
		});
	};

	const openEngineSourceFolder = async () => {
		localAppConfig.engineSourcePath = await open({
			directory: true,
			multiple: false,
			defaultPath: localAppConfig.engineSourcePath || '.',
			title: 'Select engine repository folder'
		});
	};

	const openTerminalToRepo = async () => {
		await openTerminalToPath(localAppConfig.repoPath);
	};

	const onApplyClicked = async () => {
		// localAppConfig.repoPath gets reconciled with the full path in updateAppConfig
		localAppConfig.repoPath = parentRepoPath;
		localAppConfig.repoUrl =
			localAppConfig.projects[localAppConfig.selectedArtifactProject].repoUrl;

		const hasRepoUrlChanged = $appConfig.repoUrl !== localAppConfig.repoUrl;
		showProgressModal = hasRepoUrlChanged;

		const internal = async () => {
			requestInFlight = true;

			progressModalTitle = 'Saving preferences...';
			await saveChangeSet($changeSets);

			const accessToken = $oktaAuth?.getAccessToken();
			if (accessToken) {
				await updateAppConfig(localAppConfig, accessToken, true);
			} else {
				await emit('error', 'Failed to save preferences. No access token found.');
				requestInFlight = false;
			}

			const regionChanged = $appConfig.playtestRegion !== localAppConfig.playtestRegion;

			try {
				$appConfig = await getAppConfig();
			} catch (e) {
				await emit('error', e);
			}

			let playtestPromise: Nullable<Promise> = null;
			let statusPromise: Nullable<Promise> = null;
			let commitsPromise: Nullable<Promise> = null;
			let workflowsPromise: Nullable<Promise> = null;

			if (regionChanged) {
				playtestPromise = getPlaytests();
			}

			if (hasRepoUrlChanged) {
				statusPromise = getRepoStatus();
				commitsPromise = getAllCommits();

				if (localAppConfig.repoUrl !== '') {
					workflowsPromise = await getWorkflows();
				}
			}

			const { playtestResponse, statusResponse, commitsResponse, workflowsResponse } =
				await Promise.all([playtestPromise, statusPromise, commitsPromise, workflowsPromise]);

			if (playtestResponse) {
				playtests.set(playtestResponse);
			}

			if (statusResponse) {
				repoStatus.set(statusResponse);
			}

			if (commitsResponse) {
				commits.set(commitsResponse);
			}

			if (workflowsResponse) {
				$engineWorkflows = workflowsResponse.commits;
			}

			$changeSets = await loadChangeSet();
			void emit('preferences-closed');
			requestInFlight = false;
		};

		showModal = false;

		if (showProgressModal) {
			await internal();
		} else {
			await internal();
		}
		configuringNewRepo = false;
		showProgressModal = false;
	};

	const onDiscardClicked = () => {
		configuringNewRepo = false;
		showModal = false;
		void emit('preferences-closed');
	};

	const onLogoutClicked = async () => {
		try {
			showProgressModal = true;
			progressModalTitle = 'Logging out...';

			localStorage.removeItem('oktaRefreshToken');
			localStorage.removeItem('oktaAccessToken');
			localStorage.clear();

			await restart();
			showModal = false;

			// wait 5 seconds before closing the modal
			setTimeout(() => {
				showProgressModal = false;
			}, 5000);
		} catch (e) {
			await emit('error', e);
		}
	};

	const handleResetConfig = async () => {
		try {
			await resetConfig();
		} catch (e) {
			await emit('error', e);
		}
	};

	const handleWipeClientData = async () => {
		try {
			await wipeClientData();

			showModal = false;

			await emit('success', 'Client data wiped.');
		} catch (e) {
			await emit('error', e);
		}
	};

	const handleResetLongtail = async () => {
		try {
			await resetLongtail();
		} catch (e) {
			await emit('error', e);
		}
	};

	const handleResetRepo = async () => {
		try {
			showModal = false;
			showProgressModal = true;
			progressModalTitle = 'Resetting repo...';
			await resetRepo();
			$repoStatus = await getRepoStatus();

			await emit('success', 'Repo reset to main.');
		} catch (e) {
			showModal = false;
			await emit('error', e);
		}
		showProgressModal = false;
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
	defaultClass="bg-secondary-700 dark:bg-space-900 overflow-y-auto mb-16"
	bodyClass="!border-t-0"
	backdropClass="fixed mt-8 inset-0 z-40 bg-gray-900 bg-opacity-50 dark:bg-opacity-80"
	dialogClass="fixed mt-8 top-0 start-0 end-0 h-modal md:inset-0 md:h-full z-50 w-full p-4 pb-12 flex"
	bind:open={showModal}
	dismissable
	autoclose={false}
	on:open={onOpen}
	on:close={OnClose}
>
	<div class="flex items-center justify-between gap-2">
		<div class="flex items-center gap-2">
			<p class="text-2xl text-primary-400 dark:text-primary-400 mb-2">Preferences</p>
			<DarkMode
				size="sm"
				btnClass="text-gray-300 dark:text-gray-300 hover:bg-gray-100 dark:hover:bg-gray-100 hover:text-gray-800 dark:hover:text-gray-800 focus:outline-none rounded-lg text-sm p-2.5"
			/>
		</div>
		<Button
			outline
			class="mr-5"
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

	<h1 class="text-primary-600 text-base font-semibold mb-2 flex gap-2 items-center">
		<UserSolid />
		Playtest Options
	</h1>
	<div class="rounded-lg border border-gray-300 dark:border-gray-300">
		<div class="flex flex-col gap-4 m-4">
			<div class="flex flex-col gap-2">
				<Label class="text-white">Display name</Label>
				<Input
					class="h-8 text-white bg-secondary-800 dark:bg-space-950 border-gray-400:"
					bind:value={localAppConfig.userDisplayName}
				/>
			</div>
			<Tooltip class="text-sm" placement="bottom">
				Set your display name for joining playtests inside Friendshipper.
			</Tooltip>
			<div class="flex flex-row gap-2 items-center">
				<Checkbox
					bind:checked={localAppConfig.groupDownloadedBuildsByPlaytest}
					class="w-8 h-8 bg-secondary-800 dark:bg-space-950 text-4xl"
				/>
				<Label class="text-gray-400">Group downloaded builds by playtest</Label>
			</div>
			<Tooltip class="text-sm items-center" placement="bottom">
				Group downloaded builds by playtest. This will allow you to keep multiple playtests synced
				at once. However, your initial sync of each playtest <span class="font-bold"
					>will take longer</span
				>. This option also uses significantly more disk space.
			</Tooltip>
			<div class="flex flex-row gap-2">
				<Checkbox
					bind:checked={localAppConfig.gameClientDownloadSymbols}
					class="w-8 h-8 bg-secondary-800 dark:bg-space-950 text-4xl"
				/>
				<Label class="text-gray-400">Download Debug Symbols</Label>
			</div>
			<Tooltip class="text-sm" placement="bottom">
				For engineers. Enable if you want to debug the game client locally. Increases download size.
			</Tooltip>
		</div>
		{#if $dynamicConfig && $dynamicConfig.playtestRegions}
			{#if $dynamicConfig.playtestRegions.length > 1}
				<div class="flex flex-col gap-2 m-4">
					<Label class="text-white">Playtest Region</Label>
					<Select
						size="sm"
						bind:value={localAppConfig.playtestRegion}
						class="text-white bg-secondary-800 dark:bg-space-950 border-gray-400"
					>
						{#each $dynamicConfig.playtestRegions as region}
							<option value={region}>{regions[region] || region}</option>
						{/each}
					</Select>
				</div>
			{/if}
		{/if}
		<div class="m-4">
			<Accordion>
				<AccordionItem
					class="p-2 hover:bg-secondary-700 dark:hover:bg-space-900"
					activeClass="p-2 bg-secondary-700 dark:bg-space-900"
					paddingDefault="p-2"
				>
					<span slot="header" class="text-base text-gray-300 flex gap-2">
						<ExclamationCircleOutline class="mt-0.5" />
						<span>Experimental</span>
					</span>

					<div class="flex flex-row gap-2 items-center mb-2">
						<Label class="text-white">Automatically record play through OBS</Label>
						<Toggle bind:checked={localAppConfig.recordPlay} class="w-8 h-8 text-4xl" />
					</div>
					<p class="text-sm text-primary-400">
						Automatically start recording your playtest through OBS. This will only work if you have
						OBS installed and running. You can get OBS <a
							href="https://obsproject.com/download"
							target="_blank"
							rel="noopener noreferrer"
							class="text-blue-500 hover:underline">here</a
						>. Configure it like so:

						<br /><br />

						1. Create a scene named <code>friendshipper</code> configured to capture video/audio<br
						/>
						2. Open OBS and go to <code>Tools > WebSocket Server Settings</code><br />
						3. Check <code>Enable WebSocket server</code><br />
						4. Make sure <code>Enable Authentication</code> is <b>unchecked</b>

						<br /><br />

						Videos will be saved wherever you have OBS configured to save them. By default, this is
						<code>C:\Users\YOUR_USER\Videos</code>.
					</p>
				</AccordionItem>
			</Accordion>
		</div>
	</div>

	<h1 class="text-primary-600 text-base font-semibold mt-8 mb-4 flex gap-2 items-center">
		<CodeBranchSolid />
		Source Control Options
	</h1>
	<div class="rounded-lg border border-gray-300 dark:border-gray-300">
		<div class="mt-4 mb-4 ml-4 mr-4">
			<div class="flex flex-col gap-2">
				<div class="flex gap-2 items-end">
					<div class="flex-1">
						<Label class="text-white">Project</Label>
						<Select
							size="sm"
							bind:value={localAppConfig.selectedArtifactProject}
							disabled={configuringNewRepo}
							class="text-white bg-secondary-800 dark:bg-space-950 border-gray-400"
							on:change={() => {
								const selectedProject =
									localAppConfig.projects[localAppConfig.selectedArtifactProject];
								parentRepoPath = selectedProject.repoPath.split('/').slice(0, -1).join('/');
								repoName = selectedProject.repoPath.split('/').pop() || '';
								console.log(parentRepoPath);
								console.log(repoName);
							}}
						>
							{#each Object.keys(localAppConfig.projects) as project}
								<option value={project}>{project}</option>
							{/each}
						</Select>
					</div>
					{#if configuringNewRepo}
						<Button
							color="red"
							class="h-9 mb-0.5"
							on:click={() => {
								// eslint-disable-next-line @typescript-eslint/no-dynamic-delete
								delete localAppConfig.projects[localAppConfig.selectedArtifactProject];

								localAppConfig.selectedArtifactProject = $appConfig.selectedArtifactProject;
								configuringNewRepo = false;
							}}>Cancel</Button
						>
					{:else}
						<Button class="h-9 mb-0.5" on:click={onNewProjectClicked}>New Project</Button>
					{/if}
				</div>
				<Label class="text-white">Repo Path</Label>
				<div class="flex gap-1 mb-2">
					<Button class="h-8 gap-2" on:click={openRepoFolder}>
						<FolderOpenSolid />
						Browse
					</Button>
					<ButtonGroup class="w-full">
						<Input
							class="h-8 text-white bg-secondary-800 dark:bg-space-950 border-gray-400"
							bind:value={localAppConfig.projects[localAppConfig.selectedArtifactProject].repoPath}
						/>
						<Tooltip class="text-sm" placement="bottom">
							Specified folder must be a game repository.
						</Tooltip>
						<Button
							class="h-8 bg-primary-700 hover:bg-primary-800 dark:bg-primary-600 hover:dark:bg-primary-700"
							disabled={!localAppConfig.projects[localAppConfig.selectedArtifactProject].repoPath}
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
						bind:value={localAppConfig.projects[localAppConfig.selectedArtifactProject].repoUrl}
						on:input={onRepoUrlInput}
					/>
				</div>
				<Tooltip class="text-sm" placement="bottom">
					Specified URL should be a git URL ending in <code>.git</code>.
				</Tooltip>

				{#if localAppConfig.projects[localAppConfig.selectedArtifactProject].repoUrl}
					<div class="flex flex-col gap-2">
						<Label class="text-white">Conflict Strategy</Label>
						<Select
							class="text-white bg-secondary-800 dark:bg-space-950 border-gray-400"
							bind:value={localAppConfig.conflictStrategy}
						>
							<option value="Error">Error</option>
							<option value="KeepOurs">Keep Ours</option>
							<option value="KeepTheirs">Keep Theirs</option>
						</Select>
						<Tooltip class="text-sm" placement="bottom">
							How to handle merge conflicts during sync. <code>Error</code> will block you from
							syncing.
							<code>KeepOurs</code> will keep your local changes and overwrite incoming upstream
							changes. <code>KeepTheirs</code> will keep the remote changes and overwrite your local
							changes.
						</Tooltip>
					</div>
				{/if}

				<div class="flex gap-4 pt-1">
					<div class="flex flex-row gap-2">
						<Checkbox
							bind:checked={localAppConfig.pullDlls}
							class="w-8 h-8 text-4xl mb-2 bg-secondary-800 dark:bg-space-950"
						/>
						<Label class="text-white">Download DLLs</Label>
					</div>
					<Tooltip class="text-sm" placement="bottom">
						Syncing latest will download associated game DLLs from AWS if there was a binary update.
						Content creators: leave this on.
					</Tooltip>
					<div class="flex flex-row gap-2">
						{#if localAppConfig.pullDlls}
							<Checkbox
								id="gameclientDownloadSymbolsCheckbox"
								bind:checked={localAppConfig.editorDownloadSymbols}
								class="w-8 h-8 text-4xl mb-2 bg-secondary-800 dark:bg-space-950"
							/>
							<Label class="text-gray-400">Download Debug Symbols</Label>
						{:else}
							<Checkbox
								id="gameclientDownloadSymbolsCheckbox"
								bind:checked={localAppConfig.editorDownloadSymbols}
								class="w-8 h-8 text-4xl mb-2 text-gray-500 bg-secondary-800 dark:bg-space-950"
								disabled
							/>
							<Label class="text-gray-400">Download Debug Symbols</Label>
						{/if}
					</div>
					<Tooltip class="text-sm" placement="bottom">
						For engineers. Enable if you want to debug the game client locally. Increases download
						size.
					</Tooltip>
				</div>
				<div class="flex flex-row gap-2">
					{#if localAppConfig.pullDlls}
						<Checkbox
							id="pullDllCheckbox"
							bind:checked={localAppConfig.openUprojectAfterSync}
							class="w-8 h-8 text-4xl mb-2 bg-secondary-800 dark:bg-space-950"
						/>
					{:else}
						<Checkbox
							id="pullDllCheckbox"
							bind:checked={localAppConfig.openUprojectAfterSync}
							class="w-8 h-8 text-4xl mb-2 text-gray-500 bg-secondary-800 dark:bg-space-950"
							disabled
						/>
					{/if}
					<Label class="text-white">Launch editor after sync</Label>
				</div>
				<Tooltip class="text-sm" placement="bottom">
					The editor will be launched automatically after syncing latest. Disable if you prefer to
					launch it manually.
				</Tooltip>

				<Label class="text-white">Github PAT</Label>
				<Input
					class="h-8 text-white bg-secondary-800 dark:bg-space-950 border-gray-400"
					bind:value={localAppConfig.githubPAT}
					type="password"
				/>
				<Tooltip class="text-sm" placement="bottom">
					Copy and paste your GitHub Personal Access Token (PAT) here.
				</Tooltip>
			</div>
		</div>
	</div>

	<h1 class="text-primary-600 text-base font-semibold mt-8 mb-4 flex gap-2 items-center">
		<AtomOutline />
		Engine Options
	</h1>
	<div class="rounded-lg border border-gray-300 dark:border-gray-300">
		<div class="m-4 flex flex-col gap-4">
			<div>
				<div class="flex flex-row gap-2">
					<Checkbox
						bind:checked={localAppConfig.engineAllowMultipleProcesses}
						class="w-8 h-8 text-4xl mb-2 bg-secondary-800 dark:bg-space-950"
					/>
					<Label class="text-gray-400">Allow launching multiple editors</Label>
				</div>
				<Tooltip class="text-sm" placement="top">
					When unchecked, will only allow one instance of the editor to be open. When checked,
					multiple editor processes will be launched. Use at your own risk!
				</Tooltip>
			</div>

			<div>
				<Radio
					name="engineType"
					bind:group={localAppConfig.engineType}
					class="mb-2 text-white"
					value="Prebuilt"
					>Prebuilt
				</Radio>
				<div class="flex gap-1">
					<Button
						class="h-8 gap-2"
						on:click={openEnginePrebuiltFolder}
						bind:disabled={isEngineTypeSource}
					>
						<FolderOpenSolid />
						Browse
					</Button>
					<Input
						class="h-8 text-white bg-secondary-800 dark:bg-space-950 border-gray-400"
						bind:value={localAppConfig.enginePrebuiltPath}
						bind:disabled={isEngineTypeSource}
					/>
				</div>
				<Tooltip class="text-sm" placement="top">
					For content creators. Prebuilt engine archive is downloaded from AWS into specified
					directory.
				</Tooltip>
			</div>

			<div class="flex flex-row gap-2">
				{#if isEngineTypePrebuilt}
					<Checkbox
						id="gameclientDownloadSymbolsCheckbox"
						bind:checked={localAppConfig.engineDownloadSymbols}
						class="w-8 h-8 text-4xl mb-2 bg-secondary-800 dark:bg-space-950"
					/>
					<Label class="text-gray-400">Download Debug Symbols</Label>
				{:else}
					<Checkbox
						id="gameclientDownloadSymbolsCheckbox"
						bind:checked={localAppConfig.engineDownloadSymbols}
						class="w-8 h-8 text-4xl mb-2 text-gray-500 bg-secondary-800 dark:bg-space-950"
						disabled
					/>
					<Label class="text-gray-400">Download Debug Symbols</Label>
				{/if}
			</div>
			<Tooltip class="text-sm" placement="bottom">
				For engineers. Greatly increases download size (10+GB).
			</Tooltip>

			<div>
				<Radio
					name="engineType"
					bind:group={localAppConfig.engineType}
					class="mb-2 text-white"
					value="Source"
					>Source
				</Radio>
				<div class="flex gap-1">
					<Button
						class="h-8 gap-2"
						on:click={openEngineSourceFolder}
						bind:disabled={isEngineTypePrebuilt}
					>
						<FolderOpenSolid />
						Browse
					</Button>
					<Input
						class="h-8 text-white bg-secondary-800 dark:bg-space-950 border-gray-400"
						bind:value={localAppConfig.engineSourcePath}
						bind:disabled={isEngineTypePrebuilt}
					/>
				</div>
			</div>
			<Tooltip class="text-sm" placement="bottom">
				For engineers. Specified folder must be an engine repository.
			</Tooltip>

			<div class="flex flex-col gap-2">
				<Label class="text-white">Engine Repo URL</Label>
				<Input
					class="h-8 text-white bg-secondary-800 dark:bg-space-950 border-gray-400"
					bind:value={localAppConfig.engineRepoUrl}
				/>
				<Tooltip class="text-sm" placement="bottom">
					Specified URL should be a git URL ending in <code>.git</code>. For displaying engine
					builds.
				</Tooltip>
			</div>
		</div>
	</div>

	<h1 class="text-primary-600 text-base font-semibold mt-8 mb-4 flex gap-2 items-center">
		<CloudArrowUpSolid />
		Server Configuration
	</h1>
	<div class="rounded-lg border border-gray-300 dark:border-gray-300">
		<div class="mt-4 mb-4 ml-4 mr-4 flex flex-col gap-4">
			<div class="flex flex-col gap-2">
				<Label class="text-white">Friendshipper Server URL</Label>
				<Input
					class="h-8 text-white bg-secondary-800 dark:bg-space-950 border-gray-400"
					bind:value={localAppConfig.serverUrl}
				/>
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
							on:click={handleResetRepo}
							>Reset Repo to Main
						</Button>
						<span class="w-full"
							>Hard reset to <code>main</code> (will revert all local changes)</span
						>
					</div>
					<div class="flex gap-2 items-center">
						<Button
							outline
							class="w-1/2 border-white dark:border-white text-white dark:text-white hover:bg-red-900 dark:hover:bg-red-900"
							on:click={handleRefetchRepo}
							>Refresh Repo and Commit Graph
						</Button>
						<span class="w-full">Refetch the repo from github and rebuild the commit-graph</span>
					</div>
					<div class="flex gap-2 items-center">
						<Button
							outline
							class="w-1/2 border-white dark:border-white text-white dark:text-white hover:bg-red-900 dark:hover:bg-red-900"
							on:click={handleResetConfig}
							>Reset Config
						</Button>
						<span class="w-full"
							>Delete local <code>config.yaml</code> and start fresh (requires app restart)</span
						>
					</div>
					<div class="flex gap-2 items-center">
						<Button
							outline
							class="w-1/2 border-white dark:border-white text-white dark:text-white hover:bg-red-900 dark:hover:bg-red-900"
							on:click={handleWipeClientData}
							>Wipe Data Directory
						</Button>
						<span class="w-full">Delete previously downloaded game clients</span>
					</div>
					<div class="flex gap-2 items-center">
						<Button
							outline
							class="w-1/2 border-white dark:border-white text-white dark:text-white hover:bg-red-900 dark:hover:bg-red-900"
							on:click={handleResetLongtail}
							>Re-install Longtail
						</Button>
						<span class="w-full">Reset Longtail installation (requires app restart)</span>
					</div>
					<div class="flex gap-2 items-center">
						<Button
							outline
							class="w-1/2 border-white dark:border-white text-white dark:text-white hover:bg-red-900 dark:hover:bg-red-900"
							on:click={restart}
							>Restart
						</Button>
						<span class="w-full">Restart Friendshipper</span>
					</div>
				</div>
			</AccordionItem>
		</Accordion>
	</Card>

	<div
		class="absolute bottom-0 left-0 w-full p-4 rounded-b-lg border-t bg-secondary-700 dark:bg-space-900"
	>
		<div class="flex flex-row-reverse justify-between gap-2 h-full">
			<div class="flex gap-2">
				<Button on:click={onApplyClicked}>Apply</Button>
				<Button outline on:click={onDiscardClicked}>Discard</Button>
			</div>
			<div class="flex gap-2 justify-center h-full">
				<div class="flex items-center h-full">
					<Button color="red" on:click={onLogoutClicked}>Logout</Button>
					<code class="ml-2 text-sm">Uptime: {formatUptime(uptime)}</code>
				</div>
			</div>
		</div>
	</div>
</Modal>

<ProgressModal title={progressModalTitle} showModal={showProgressModal} />
