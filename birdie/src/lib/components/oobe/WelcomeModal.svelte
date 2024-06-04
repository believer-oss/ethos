<script lang="ts">
	import { Alert, Button, Card, Input, Modal, Spinner } from 'flowbite-svelte';
	import { FolderOpenSolid } from 'flowbite-svelte-icons';
	import { get } from 'svelte/store';
	import { onMount } from 'svelte';
	import { open } from '@tauri-apps/api/dialog';
	import { emit, listen } from '@tauri-apps/api/event';
	import { getAppConfig, updateAppConfig } from '$lib/config';
	import { appConfig, onboardingInProgress, repoStatus } from '$lib/stores';
	import { configureGitUser, installGit } from '$lib/system';
	import { cloneRepo, getRepoStatus } from '$lib/repo';

	enum Page {
		GitSetup = 1,
		CloneSettings,
		CloneStatus,
		Done
	}

	export let showModal: boolean;
	export let onClose: () => void;

	let disableNext = true;
	let currentPage: Page = Page.GitSetup;
	let cloneLocation: string = '';
	let repoUrl: string = '';
	let installingGit = false;
	let cloning = false;
	let errorMessage: string = '';

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

	const validate = () => {
		let valid = true;

		switch (currentPage) {
			case Page.GitSetup:
				valid = gitUsername !== '' && gitEmail !== '' && githubPAT !== '';
				break;
			case Page.CloneSettings:
				valid = repoUrl !== '' && cloneLocation !== '';
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
		try {
			await installGit();
		} catch (e) {
			await emit('error', e);
		}
		installingGit = false;
	};

	const gotoPrevPage = () => {
		validate();

		currentPage -= 1;
	};

	const handleUpdateAppConfig = async () => {
		const updatedAppConfig = get(appConfig);
		updatedAppConfig.repoPath = repoPath;
		updatedAppConfig.repoUrl = repoUrl;
		updatedAppConfig.githubPAT = githubPAT;

		try {
			await updateAppConfig(updatedAppConfig);
			await emit('success', 'Preferences saved.');
		} catch (e) {
			await emit('error', e);
		}

		try {
			$appConfig = await getAppConfig();
		} catch (e) {
			await emit('error', e);
		}
	};

	const initiateRepoClone = async () => {
		errorMessage = '';
		cloning = true;
		try {
			await handleUpdateAppConfig();

			await cloneRepo({ url: repoUrl, path: cloneLocation });

			// force update of repo status
			message = 'Updating repo status...';
			$repoStatus = await getRepoStatus();
		} catch (e) {
			const error = e as Error;
			errorMessage = String(error.message);
		}
		cloning = false;
	};

	const gotoNextPage = async () => {
		currentPage += 1;

		validate();

		if (currentPage === Page.GitSetup) {
			await confirmGitInstallation();
		}

		if (currentPage === Page.CloneSettings) {
			await configureGitUser(gitUsername, gitEmail);
		}

		if (currentPage === Page.CloneStatus) {
			await initiateRepoClone();
		}
	};

	const handleClose = async () => {
		try {
			$onboardingInProgress = false;
			onClose();
		} catch (e) {
			await emit('error', e);
		}
		showModal = false;
	};

	const openCloneLocation = async () => {
		cloneLocation = (await open({
			directory: true,
			multiple: false,
			defaultPath: '.',
			title: 'Select repo clone location'
		})) as string;

		updateRepoPath();
	};

	void listen('git-log', (event) => {
		message = event.payload as string;
	});

	const onOpen = async () => {
		$onboardingInProgress = true;
		await confirmGitInstallation();
	};

	onMount(() => {
		$appConfig = get(appConfig);
		if ($appConfig.repoPath) {
			repoPath = $appConfig.repoPath;
		}
	});
</script>

<Modal
	size="lg"
	defaultClass="dark:bg-secondary-800 overflow-y-auto h-[70vh]"
	bodyClass="!border-t-0 h-full"
	backdropClass="fixed mt-8 inset-0 z-40 bg-gray-900 bg-opacity-50 dark:bg-opacity-80"
	dialogClass="fixed mt-8 top-0 start-0 end-0 h-modal md:inset-0 md:h-full z-50 w-full p-4 pb-12 flex"
	bind:open={showModal}
	on:open={onOpen}
	on:close={handleClose}
	autoclose={false}
	dismissable={false}
>
	<div class="flex flex-col justify-between gap-2 h-full">
		<div class="flex flex-col items-center gap-2">
			<p class="text-2xl text-center my-2 dark:text-primary-400 w-full">Welcome to Birdie!</p>
			{#if errorMessage}
				<Alert class="dark:text-gray-300 dark:bg-red-800">
					<span class="font-medium">Error!</span>
					{errorMessage}
					{#if currentPage === Page.CloneStatus}
						<Button primary size="xs" on:click={initiateRepoClone}>Retry</Button>
					{/if}
				</Alert>
			{/if}
		</div>
		{#if currentPage === Page.GitSetup}
			<Card class="dark:bg-secondary-700 w-full max-w-full">
				<div class="flex flex-col items-center gap-2 w-full">
					{#if installingGit}
						<div class="flex flex-row gap-2">
							<span class="text-md text-center dark:text-gray-300 w-full"
								>Making sure Git is installed.</span
							>
							<Spinner class="w-6 h-6" />
						</div>
					{:else}
						<span class="text-md text-center dark:text-gray-300 w-full"
							>What's your name? (e.g. Bob Boberts)</span
						>
						<Input class="h-8 text-center w-1/2" bind:value={gitUsername} on:input={validate} />
						<span class="text-md text-center dark:text-gray-300 w-full"
							>What's the email address you'd like associated with git? (e.g. boberts@example.com)</span
						>
						<Input class="h-8 text-center w-1/2" bind:value={gitEmail} on:input={validate} />
						<span class="text-md text-center dark:text-gray-300 w-3/4"
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
				<p class="dark:text-gray-300 text-sm text-center">
					<em>Note</em>: Clicking <span class="font-mono dark:text-primary-400">Next</span> will
					start the repo cloning process. You may be prompted for Git credentials. If you are, your
					username is your <b>GitHub</b> username and your password is your
					<b>GitHub Personal Access Token</b>, so keep those things handy!
				</p>
			</div>
			<Card class="dark:bg-secondary-700 w-full max-w-full">
				<div class="flex flex-col items-center gap-2 w-full">
					<span class="text-md text-center dark:text-gray-300 w-full"
						>What's your Git repo's remote URL?</span
					>
					<Input class="h-8 text-center w-1/2" bind:value={repoUrl} on:input={updateRepoPath} />
					<span class="text-md text-center dark:text-gray-300 w-full"
						>Where would you like to clone the project?</span
					>
					<div class="flex items-center gap-2 w-full">
						<Input disabled class="h-8" bind:value={cloneLocation} on:change={updateRepoPath} />
						<Button class="h-8 gap-2" on:click={openCloneLocation}>
							<FolderOpenSolid />Browse</Button
						>
					</div>
					{#if repoPath !== ''}
						<span class="text-center"
							>Repo will be configured at: <span class="font-mono dark:text-primary-400"
								>{repoPath}</span
							>.<br />If a Git repo already exists at this location, Birdie will skip the clone
							step.</span
						>
					{/if}
				</div>
			</Card>
		{:else if currentPage === Page.CloneStatus}
			<div class="flex flex-col items-center gap-2 w-full overflow-y-hidden">
				{#if cloning}
					<div class="flex gap-2 items-center justify-center w-full px-4 overflow-hidden">
						<span class="text-md text-center dark:text-gray-300 w-full"
							>Cloning repo to <span class="font-mono dark:text-primary-400">{repoPath}</span>. <Spinner
								class="w-6 h-6"
							/>
							<br /><br />
							This will take some time. Feel free to go get some coffee or have lunch!☕🌭💤</span
						>
					</div>
					{#if message}
						<div class="rounded-md p-2 dark:bg-secondary-800">
							<p class="text-sm font-mono dark:text-primary-400 m-0">{message}</p>
						</div>
					{/if}
				{:else if !errorMessage}
					<span class="text-md text-center dark:text-gray-300 w-full"
						>Repo cloned to <span class="font-mono dark:text-primary-400">{repoPath}</span>!</span
					>
				{/if}
			</div>
		{:else if currentPage === Page.Done}
			<div class="flex flex-col items-center gap-2 w-full">
				<span class="text-md text-center dark:text-gray-300 w-full">🎉You're all set!🎉</span>
			</div>
		{/if}
		<div class="flex justify-between mt-2">
			{#if currentPage !== Page.GitSetup}
				<Button primary on:click={gotoPrevPage}>Back</Button>
			{:else}
				<div />
			{/if}
			{#if currentPage === Page.Done}
				<Button primary on:click={handleClose}>Close</Button>
			{:else}
				<Button disabled={disableNext || cloning} primary on:click={gotoNextPage}>Next</Button>
			{/if}
		</div>
	</div>
</Modal>
