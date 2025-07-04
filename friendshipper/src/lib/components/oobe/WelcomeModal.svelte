<script lang="ts">
	import { Alert, Button, Card, Input, Modal, Spinner } from 'flowbite-svelte';
	import { ComputerSpeakerSolid, FolderOpenSolid } from 'flowbite-svelte-icons';
	import { get } from 'svelte/store';
	import { onMount } from 'svelte';
	import { open } from '@tauri-apps/plugin-dialog';
	import { emit, listen } from '@tauri-apps/api/event';
	import { updateAppConfig } from '$lib/config';
	import { appConfig, onboardingInProgress } from '$lib/stores';
	import UnrealEngineLogo from '$lib/icons/UnrealEngineLogo.svelte';
	import { configureGitUser, installGit, restart } from '$lib/system';
	import { cloneRepo } from '$lib/repo';
	import type { AppConfig } from '$lib/types';

	enum Page {
		ServerConfig = 1,
		Username,
		UserType,
		GitSetup,
		CloneSettings,
		CloneStatus,
		Done
	}

	export let showModal: boolean;
	export let currentConfig: AppConfig;
	export let onClose: () => Promise<void>;

	let disableNext = true;
	let currentPage: Page = Page.ServerConfig;

	// server config
	let serverUrl: string = '';

	let cloneLocation: string = '';
	let repoUrl: string = '';
	let installingGit = false;
	let cloning = false;
	let errorMessage: string = '';

	let isPlaytesterOnly: boolean = false;

	let userDisplayName: string = '';
	let repoPath: string = '';
	let githubPAT: string = '';

	let gitUsername: string = '';
	let gitEmail: string = '';

	let message: string = '';

	$: repoUrl,
		cloneLocation,
		() => {
			const repoName = repoUrl.split('/').pop()?.replace('.git', '');
			repoPath = `${cloneLocation}/${repoName}`;
		};

	const serverUrlIsValid = (): boolean => {
		// make sure this is a valid URL
		try {
			const url = new URL(serverUrl);
			return url.protocol === 'http:' || url.protocol === 'https:';
		} catch (_) {
			return false;
		}
	};

	const validate = () => {
		let valid = true;

		switch (currentPage) {
			case Page.ServerConfig:
				valid = serverUrlIsValid();
				serverUrl = serverUrl.trim().replace(/\/$/, '');
				break;
			case Page.Username:
				valid = userDisplayName !== '';
				break;
			case Page.GitSetup:
				valid = gitUsername !== '' && gitEmail !== '' && githubPAT !== '';
				gitEmail = gitEmail.trim();
				githubPAT = githubPAT.trim();
				break;
			case Page.CloneSettings:
				valid = repoUrl !== '' && cloneLocation !== '';
				repoUrl = repoUrl.trim();
				break;
			default:
				break;
		}

		// reset the error message
		disableNext = !valid;
		if (valid) {
			errorMessage = '';
		}
	};

	const updateRepoPath = () => {
		validate();
		if (repoUrl === '' || cloneLocation === '') {
			return;
		}

		const repoName = repoUrl.split('/').pop()?.replace('.git', '');
		repoPath = `${cloneLocation.replace(/\\/g, '/')}/${repoName}`;
	};

	const confirmGitInstallation = async () => {
		installingGit = true;
		await installGit();
		installingGit = false;
	};

	const gotoPrevPage = () => {
		validate();

		if (currentPage === Page.Done && isPlaytesterOnly) {
			currentPage = Page.UserType;
			return;
		}

		currentPage -= 1;
	};

	const handleUpdateAppConfig = async () => {
		const updatedAppConfig = get(appConfig);
		updatedAppConfig.userDisplayName = userDisplayName;
		updatedAppConfig.repoPath = repoPath;
		updatedAppConfig.repoUrl = repoUrl;
		updatedAppConfig.serverUrl = serverUrl;
		updatedAppConfig.githubPAT = githubPAT;

		try {
			await updateAppConfig(updatedAppConfig);
			await emit('success', 'Preferences saved.');
		} catch (e) {
			await emit('error', e);
		}
	};

	const initiateRepoClone = async () => {
		errorMessage = '';
		try {
			cloning = true;
			await handleUpdateAppConfig();

			await cloneRepo({ url: repoUrl, path: cloneLocation });
		} catch (e) {
			const error = e as Error;
			errorMessage = String(error.message);
		}
		cloning = false;
	};

	const onPlaytestSelected = async () => {
		isPlaytesterOnly = true;

		await handleUpdateAppConfig();

		currentPage = Page.Done;
	};

	const gotoNextPage = async () => {
		currentPage += 1;

		validate();

		if (currentPage === Page.GitSetup) {
			isPlaytesterOnly = false;

			if (!currentConfig.githubPAT) {
				await confirmGitInstallation();
			} else {
				// skip this page if this is a reconfigure
				await gotoNextPage();
			}
		}

		if (currentPage === Page.CloneSettings) {
			if (gitUsername !== '' && gitEmail !== '') {
				await configureGitUser(gitUsername, gitEmail);
			}
		}

		if (currentPage === Page.CloneStatus) {
			await initiateRepoClone();
			await restart();
		}
	};

	const handleOpen = () => {
		$onboardingInProgress = true;
	};

	const handleClose = async () => {
		try {
			$onboardingInProgress = false;

			await onClose();
		} catch (e) {
			await emit('error', e);
		}
		showModal = false;
	};

	const openCloneLocation = async () => {
		cloneLocation = await open({
			directory: true,
			multiple: false,
			defaultPath: '.',
			title: 'Select repo clone location'
		});

		updateRepoPath();
	};

	void listen('git-log', (event) => {
		message = event.payload as string;
	});

	onMount(() => {
		if (currentConfig.userDisplayName) {
			userDisplayName = currentConfig.userDisplayName;
		}

		if (currentConfig.serverUrl) {
			serverUrl = currentConfig.serverUrl;
		}

		if (currentConfig.repoUrl) {
			repoUrl = currentConfig.repoUrl;
		}

		if (currentConfig.githubPAT) {
			githubPAT = currentConfig.githubPAT;
		}

		if (currentConfig.repoPath) {
			repoPath = currentConfig.repoPath;
			cloneLocation = repoPath.split('/').slice(0, -1).join('/');
		}
	});
</script>

<Modal
	size="xl"
	defaultClass="bg-secondary-700 dark:bg-space-900 overflow-y-auto h-[70vh]"
	bodyClass="!border-t-0 h-full"
	backdropClass="fixed mt-8 inset-0 z-40 bg-gray-900 bg-opacity-50 dark:bg-opacity-80"
	dialogClass="fixed mt-8 top-0 start-0 end-0 h-modal md:inset-0 md:h-full z-50 w-full p-4 pb-12 flex"
	bind:open={showModal}
	on:close={handleClose}
	on:open={handleOpen}
	autoclose={false}
	dismissable={false}
>
	<div class="flex flex-col justify-between gap-2 h-full">
		<div class="flex flex-col items-center gap-2">
			<p class="text-2xl text-center my-2 text-primary-400 dark:text-primary-400 w-full">
				Welcome to Friendshipper!
			</p>
			{#if errorMessage}
				<Alert class="text-gray-300 dark:text-gray-300 bg-red-800 dark:bg-red-800">
					<span class="font-medium">Error!</span>
					{errorMessage}
					{#if currentPage === Page.CloneStatus}
						<Button primary size="xs" on:click={initiateRepoClone}>Retry</Button>
					{/if}
				</Alert>
			{/if}
		</div>
		{#if currentPage === Page.ServerConfig}
			<div>
				<p class="text-md text-center text-gray-300 dark:text-gray-300 w-full">
					To get started, you'll need to provide the URL of your Friendshipper server.
				</p>
			</div>
			<div class="flex flex-col justify-between items-center gap-2">
				<span class="text-md text-center text-gray-300 dark:text-gray-300 w-full"
					>Friendshipper Server URL</span
				>
				<Input
					size="lg"
					class="text-center bg-secondary-700 dark:bg-space-900 text-primary-400 dark:text-primary-400 border-primary-400 dark:border-primary-400 border-2 rounded-md p-2 w-1/2"
					type="text"
					spellcheck="false"
					bind:value={serverUrl}
					on:input={validate}
				/>
			</div>
		{:else if currentPage === Page.Username}
			<div class="flex flex-col justify-between gap-2">
				<div class="flex flex-col items-center gap-2 w-full">
					<span class="text-md text-center text-gray-300 dark:text-gray-300 w-full"
						>What would you like your playtest username to be? This can be changed later via the
						preferences menu!</span
					>
					<div class="flex items-center justify-around gap-2 w-full">
						<Input
							size="lg"
							class="text-center bg-secondary-700 dark:bg-space-900 text-primary-400 dark:text-primary-400 border-primary-400 dark:border-primary-400 border-2 rounded-md p-2 w-1/2"
							type="text"
							spellcheck="false"
							bind:value={userDisplayName}
							on:input={validate}
						/>
					</div>
				</div>
			</div>
		{:else if currentPage === Page.UserType}
			<div class="flex items-center gap-2">
				<span class="text-md text-center text-gray-300 dark:text-gray-300 w-full"
					>What will you need to do with Friendshipper?</span
				>
			</div>
			<div class="flex w-full justify-around">
				<Card class="bg-secondary-800 dark:bg-space-950 flex-grow !p-0">
					<Button
						outline
						class="h-full flex flex-col gap-4 text-gray-300 dark:text-gray-300 py-10"
						on:click={onPlaytestSelected}
					>
						<span class="text-lg">Playtest</span>
						<ComputerSpeakerSolid class="w-24 h-24" />
						<span class="text-sm font-medium">For playtesters!</span>
					</Button>
				</Card>
				<Card class="bg-secondary-800 dark:bg-space-950 flex-grow !p-0">
					<Button
						outline
						class="h-full flex flex-col gap-4 text-gray-300 dark:text-gray-300 py-10"
						on:click={gotoNextPage}
					>
						<span class="text-lg">Playtest + Manage Source Control</span>
						<div class="flex gap-4">
							<ComputerSpeakerSolid class="w-24 h-24" />
							<UnrealEngineLogo class="w-24 h-24" />
						</div>
						<span class="text-sm font-medium"
							>For designers, tech artists, engineers - anyone who needs access to source control!</span
						>
					</Button>
				</Card>
			</div>
		{:else if currentPage === Page.GitSetup}
			<Card class="bg-secondary-800 dark:bg-space-950 w-full max-w-full">
				<div class="flex flex-col items-center gap-2 w-full">
					{#if installingGit}
						<div class="flex flex-row gap-2">
							<span class="text-md text-center text-gray-300 dark:text-gray-300 w-full"
								>Making sure Git is installed.</span
							>
							<Spinner class="w-6 h-6" />
						</div>
					{:else}
						<span class="text-md text-center text-gray-300 dark:text-gray-300 w-full"
							>What's your name? (e.g. Bob Boberts)</span
						>
						<Input class="h-8 text-center w-1/2" bind:value={gitUsername} on:input={validate} />
						<span class="text-md text-center text-gray-300 dark:text-gray-300 w-full"
							>What's the email address you'd like associated with git? (e.g. boberts@example.com)</span
						>
						<Input class="h-8 text-center w-1/2" bind:value={gitEmail} on:input={validate} />
						<span class="text-md text-center text-gray-300 dark:text-gray-300 w-3/4"
							>Paste your GitHub Personal Access Token here.</span
						>
						<Input
							class="h-8 text-center"
							bind:value={githubPAT}
							type="password"
							on:input={validate}
						/>
					{/if}
				</div>
			</Card>
		{:else if currentPage === Page.CloneSettings}
			<div class="w-full">
				<p class="text-gray-300 dark:text-gray-300 text-sm text-center">
					<em>Note</em>: Clicking
					<span class="font-mono text-primary-400 dark:text-primary-400">Next</span>
					will start the repo cloning process. You may be prompted for Git credentials. If you are, your
					username is your <b>GitHub</b> username and your password is your
					<b>GitHub Personal Access Token</b>, so keep those things handy!
				</p>
			</div>
			<Card class="bg-secondary-800 dark:bg-space-950 w-full max-w-full">
				<div class="flex flex-col items-center gap-2 w-full">
					<span class="text-md text-center text-gray-300 dark:text-gray-300 w-full"
						>What's your Git repo's remote URL?</span
					>
					<Input class="h-8 text-center w-1/2" bind:value={repoUrl} on:input={updateRepoPath} />
					<span class="text-md text-center text-gray-300 dark:text-gray-300 w-full"
						>Where would you like to clone the project?</span
					>
					<div class="flex items-center gap-2 w-full">
						<Input disabled class="h-8" bind:value={cloneLocation} on:change={updateRepoPath} />
						<Button class="h-8 gap-2" on:click={openCloneLocation}>
							<FolderOpenSolid />
							Browse
						</Button>
					</div>
					{#if repoPath !== ''}
						<span class="text-center"
							>Repo will be configured at: <span
								class="font-mono text-primary-400 dark:text-primary-400">{repoPath}</span
							>.<br />If a Git repo already exists at this location, Friendshipper will skip the
							clone step.</span
						>
					{/if}
				</div>
			</Card>
		{:else if currentPage === Page.CloneStatus}
			<div class="flex flex-col items-center gap-2 w-full overflow-y-hidden">
				{#if cloning}
					<div class="flex gap-2 items-center justify-center w-full px-4 overflow-hidden">
						<span class="text-md text-center text-gray-300 dark:text-gray-300 w-full"
							>Cloning repo to <span class="font-mono text-primary-400 dark:text-primary-400"
								>{repoPath}</span
							>. <Spinner class="w-6 h-6" />
							<br /><br />
							This will take some time. Feel free to go get some coffee or have lunch!☕🌭💤</span
						>
					</div>
					{#if message}
						<div class="rounded-md p-2 bg-secondary-800 dark:bg-space-950">
							<p class="text-sm font-mono text-primary-400 dark:text-primary-400 m-0">{message}</p>
						</div>
					{/if}
				{:else if !errorMessage}
					<span class="text-md text-center text-gray-300 dark:text-gray-300 w-full"
						>Repo cloned to <span class="font-mono text-primary-400 dark:text-primary-400"
							>{repoPath}</span
						>!</span
					>
				{/if}
			</div>
		{:else if currentPage === Page.Done}
			<div class="flex flex-col items-center gap-2 w-full">
				<span class="text-md text-center text-gray-300 dark:text-gray-300 w-full"
					>🎉You're all set, <span class="font-mono text-primary-400 dark:text-primary-400"
						>{userDisplayName}</span
					>!🎉 Friendshipper will now restart.</span
				>
			</div>
		{/if}
		<div class="flex justify-between mt-2">
			{#if currentPage !== Page.ServerConfig}
				<Button primary on:click={gotoPrevPage}>Back</Button>
			{:else}
				<div />
			{/if}
			{#if currentPage === Page.Done}
				<Button primary on:click={handleClose}>Close</Button>
			{:else if currentPage !== Page.UserType}
				<Button disabled={disableNext || cloning} primary on:click={gotoNextPage}>Next</Button>
			{/if}
		</div>
	</div>
</Modal>
