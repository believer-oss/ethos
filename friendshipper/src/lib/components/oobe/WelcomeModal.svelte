<script lang="ts">
	import { Alert, Button, Card, Input, Modal, Spinner } from 'flowbite-svelte';
	import { ComputerSpeakerSolid, FolderOpenSolid } from 'flowbite-svelte-icons';
	import { get } from 'svelte/store';
	import { onMount } from 'svelte';
	import { open } from '@tauri-apps/api/dialog';
	import { emit, listen } from '@tauri-apps/api/event';
	import { getAppConfig, updateAppConfig } from '$lib/config';
	import { appConfig, onboardingInProgress, repoStatus } from '$lib/stores';
	import UnrealEngineLogo from '$lib/icons/UnrealEngineLogo.svelte';
	import { configureGitUser, installGit } from '$lib/system';
	import {
		cloneRepo,
		forceDownloadDlls,
		forceDownloadEngine,
		getRepoStatus,
		SkipDllCheck,
		SkipOfpaTranslation
	} from '$lib/repo';

	enum Page {
		AWSConfig = 1,
		Username,
		UserType,
		GitSetup,
		CloneSettings,
		CloneStatus,
		Done
	}

	export let showModal: boolean;
	export let onClose: () => Promise<void>;

	let disableNext = true;
	let currentPage: Page = Page.AWSConfig;

	// aws config
	let awsAccountId: string = '';
	let awsSsoStartUrl: string = '';
	let awsRoleName: string = '';
	let artifactBucket: string = '';

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

	const validate = () => {
		let valid = true;

		switch (currentPage) {
			case Page.AWSConfig:
				valid =
					awsAccountId !== '' &&
					awsSsoStartUrl !== '' &&
					awsRoleName !== '' &&
					artifactBucket !== '';
				break;
			case Page.Username:
				valid = userDisplayName !== '';
				break;
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
		updatedAppConfig.githubPAT = githubPAT;

		updatedAppConfig.awsConfig = {
			accountId: awsAccountId,
			ssoStartUrl: awsSsoStartUrl,
			roleName: awsRoleName,
			artifactBucketName: artifactBucket
		};

		if (!isPlaytesterOnly) {
			updatedAppConfig.gitConfig = {
				username: gitUsername,
				email: gitEmail
			};
		}

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
			$repoStatus = await getRepoStatus(SkipDllCheck.False, SkipOfpaTranslation.True);

			// run initial fetch of DLLs - it may be worth moving this and the engine fetch
			// to the clone endpoint on the backend
			message = 'Performing initial fetch of DLLs...';
			await forceDownloadDlls();

			// run initial fetch of Engine binaries
			message = 'Performing initial fetch of Engine binaries...';
			await forceDownloadEngine();
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
			await confirmGitInstallation();
		}

		if (currentPage === Page.CloneSettings) {
			await configureGitUser(gitUsername, gitEmail);
		}

		if (currentPage === Page.CloneStatus) {
			await initiateRepoClone();
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
		$appConfig = get(appConfig);
		if ($appConfig.userDisplayName) {
			userDisplayName = $appConfig.userDisplayName;
		}

		if ($appConfig.repoPath) {
			repoPath = $appConfig.repoPath;
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
		{#if currentPage === Page.AWSConfig}
			<div>
				<p class="text-md text-center text-gray-300 dark:text-gray-300 w-full">
					To get started, you'll need to provide some information about your AWS account and the S3
					bucket where your game's artifacts are stored.
				</p>
			</div>
			<div class="flex flex-col justify-between items-center gap-2">
				<span class="text-md text-center text-gray-300 dark:text-gray-300 w-full"
					>AWS Account ID</span
				>
				<Input class="h-8 text-center w-1/2" bind:value={awsAccountId} on:input={validate} />
				<span class="text-md text-center text-gray-300 dark:text-gray-300 w-full"
					>AWS SSO Start URL</span
				>
				<Input class="h-8 text-center w-1/2" bind:value={awsSsoStartUrl} on:input={validate} />
				<span class="text-md text-center text-gray-300 dark:text-gray-300 w-3/4">AWS Role Name</span
				>
				<Input class="h-8 text-center w-1/2" bind:value={awsRoleName} on:input={validate} />
				<span class="text-md text-center text-gray-300 dark:text-gray-300 w-3/4"
					>S3 Artifact Bucket Name</span
				>
				<Input class="h-8 text-center w-1/2" bind:value={artifactBucket} on:input={validate} />
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
					>!🎉</span
				>
			</div>
		{/if}
		<div class="flex justify-between mt-2">
			{#if currentPage !== Page.AWSConfig}
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
