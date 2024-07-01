<script lang="ts">
	import {
		Accordion,
		AccordionItem,
		Button,
		Card,
		Checkbox,
		DarkMode,
		Input,
		Label,
		Modal,
		Radio,
		Spinner,
		Toggle,
		Tooltip
	} from 'flowbite-svelte';
	import {
		AtomOutline,
		CloudArrowUpSolid,
		CodeBranchSolid,
		DatabaseSolid,
		ExclamationCircleOutline,
		FolderOpenSolid,
		UserSolid
	} from 'flowbite-svelte-icons';
	import { emit } from '@tauri-apps/api/event';
	import { open } from '@tauri-apps/api/dialog';
	import { appConfig, dynamicConfig } from '$lib/stores';
	import type { AppConfig } from '$lib/types';
	import { getAppConfig, resetConfig, updateAppConfig } from '$lib/config';
	import { resetLongtail, wipeClientData } from '$lib/builds';
	import { restart } from '$lib/system';
	import { resetRepo } from '$lib/repo';

	export let showModal: boolean;
	export let requestInFlight: boolean;
	export let showProgressModal: boolean;
	export let handleCheckForUpdates: () => Promise<void>;

	let checkForUpdatesInFlight: boolean = false;
	let localAppConfig: AppConfig = { awsConfig: {} };
	let isEngineTypePrebuilt: boolean = false;
	let isEngineTypeSource: boolean = false;

	$: isEngineTypePrebuilt = localAppConfig.engineType === 'Prebuilt';
	$: isEngineTypeSource = localAppConfig.engineType === 'Source';
	$: isLudosEndpointNotCustom = localAppConfig.ludosEndpointType !== 'Custom';

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

		void emit('preferences-closed');

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
			await resetRepo();

			showModal = false;
			await emit('success', 'Repo reset to main.');
		} catch (e) {
			showModal = false;
			await emit('error', e);
		}
	};
</script>

<Modal
	defaultClass="bg-secondary-700 dark:bg-space-900 overflow-y-auto"
	bodyClass="!border-t-0"
	backdropClass="fixed mt-8 inset-0 z-40 bg-gray-900 bg-opacity-50 dark:bg-opacity-80"
	dialogClass="fixed mt-8 top-0 start-0 end-0 h-modal md:inset-0 md:h-full z-50 w-full p-4 pb-12 flex"
	bind:open={showModal}
	dismissable
	autoclose={false}
	on:open={onOpen}
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
				<Label class="text-white">Repo Path</Label>
				<div class="flex gap-1 mb-2">
					<Button class="h-8 gap-2" on:click={openRepoFolder}>
						<FolderOpenSolid />
						Browse
					</Button>
					<Input
						class="h-8 text-white bg-secondary-800 dark:bg-space-950 border-gray-400"
						bind:value={localAppConfig.repoPath}
					/>
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

				<div class="flex gap-4">
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

	{#if $dynamicConfig.ludosEnabled}
		<h1 class="text-primary-600 text-base font-semibold mt-8 mb-4 flex gap-2 items-center">
			<DatabaseSolid />
			Ludos
		</h1>
		<div class="rounded-lg border border-gray-300 dark:border-gray-300">
			<div class="mt-4 mb-4 ml-4 mr-4 flex flex-col gap-4">
				<div class="flex flex-row gap-2">
					<Checkbox
						id="ludosShowUICheckbox"
						bind:checked={localAppConfig.ludosShowUI}
						class="w-8 h-8 text-4xl mb-2 bg-secondary-800 dark:bg-space-950"
					/>
					<Label class="text-white">Show UI</Label>
				</div>
				<Tooltip>Only engineers should need this functionality.</Tooltip>
				<div class="flex gap-2">
					<Radio
						name="endpoint"
						bind:group={localAppConfig.ludosEndpointType}
						class="mb-2 text-white"
						value="Local"
						>Local
					</Radio>
					{#if localAppConfig.ludosEndpointType === 'Local'}
						<Label class="text-primary-400 dark:text-primary-400">http://localhost:18080</Label>
					{:else}
						<Label class="text-primary-400">http://localhost:18080</Label>
					{/if}
				</div>
				<Tooltip class="text-sm" placement="top">
					Default settings for a Ludos instance running in a local docker container.
				</Tooltip>

				<div class="flex gap-2">
					<Radio
						name="endpoint"
						bind:group={localAppConfig.ludosEndpointType}
						class="mb-2 text-white"
						value="Custom"
						>Custom
					</Radio>
					<Input
						class="h-8 text-white bg-secondary-800 dark:bg-space-950 border-gray-400"
						bind:value={localAppConfig.ludosCustomEndpoint}
						bind:disabled={isLudosEndpointNotCustom}
					/>
				</div>
				<Tooltip class="text-sm" placement="bottom">
					Escape hatch if you want to use a custom endpoint.
				</Tooltip>
			</div>
		</div>
	{/if}

	<h1 class="text-primary-600 text-base font-semibold mt-8 mb-4 flex gap-2 items-center">
		<CloudArrowUpSolid />
		AWS
	</h1>
	<div class="rounded-lg border border-gray-300 dark:border-gray-300">
		<div class="mt-4 mb-4 ml-4 mr-4 flex flex-col gap-4">
			<div class="flex flex-col gap-2">
				<Label class="text-white">AWS Account ID</Label>
				<Input
					class="h-8 text-white bg-secondary-800 dark:bg-space-950 border-gray-400"
					bind:value={localAppConfig.awsConfig.accountId}
				/>
			</div>
			<div class="flex flex-col gap-2">
				<Label class="text-white">AWS SSO Start URL</Label>
				<Input
					class="h-8 text-white bg-secondary-800 dark:bg-space-950 border-gray-400"
					bind:value={localAppConfig.awsConfig.ssoStartUrl}
				/>
			</div>
			<div class="flex flex-col gap-2">
				<Label class="text-white">Playtest IAM Role</Label>
				<Input
					class="h-8 text-white bg-secondary-800 dark:bg-space-950 border-gray-400"
					bind:value={localAppConfig.awsConfig.roleName}
				/>
			</div>
			<div class="flex flex-col gap-2">
				<Label class="text-white">S3 Artifact Bucket Name</Label>
				<Input
					class="h-8 text-white bg-secondary-800 dark:bg-space-950 border-gray-400"
					bind:value={localAppConfig.awsConfig.artifactBucketName}
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

	<div class="flex flex-row-reverse gap-2">
		<Button outline on:click={onDiscardClicked}>Discard</Button>
		<Button on:click={onApplyClicked}>Apply</Button>
	</div>
</Modal>
