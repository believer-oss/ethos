<script lang="ts">
	import {
		Button,
		ButtonGroup,
		Card,
		Spinner,
		Tooltip,
		Dropdown,
		DropdownItem
	} from 'flowbite-svelte';
	import { onMount } from 'svelte';
	import { ChevronDownOutline, RefreshOutline, FileCodeSolid } from 'flowbite-svelte-icons';
	import { emit } from '@tauri-apps/api/event';
	import { CommitTable, ProgressModal } from '@ethos/core';
	import {
		getAllCommits,
		syncLatest,
		openUproject,
		generateSln,
		openSln,
		forceDownloadDlls,
		forceDownloadEngine,
		reinstallGitHooks,
		syncEngineCommitWithUproject,
		syncUprojectWithEngineCommit,
		getRepoStatus,
		showCommitFiles
	} from '$lib/repo';
	import { appConfig, commits, latestLocalCommit, repoStatus } from '$lib/stores';
	import UnrealEngineLogoNoCircle from '$lib/icons/UnrealEngineLogoNoCircle.svelte';

	let loading = false;
	let inAsyncOperation = false;
	let asyncModalText = '';

	$: conflictsDetected = $repoStatus?.conflicts && $repoStatus.conflicts.length > 0;

	const refresh = async () => {
		loading = true;
		try {
			repoStatus.set(await getRepoStatus());
			commits.set(await getAllCommits());
		} catch (e) {
			await emit('error', e);
		}
		loading = false;
	};

	const handleSyncClicked = async () => {
		try {
			inAsyncOperation = true;
			asyncModalText = 'Pulling latest with git';
			await syncLatest();
			await refresh();

			if ($appConfig.pullDlls === false) {
				asyncModalText = 'Generating projects';
				await generateSln();
			} else if ($appConfig.openUprojectAfterSync === true) {
				asyncModalText = 'Launching Unreal Engine';
				await openUproject();
			}

			await emit('success', 'Sync complete!');
		} catch (e) {
			await emit('error', e);
		}

		inAsyncOperation = false;
	};

	const handleOpenUprojectClicked = async () => {
		try {
			inAsyncOperation = true;
			asyncModalText = 'Launching Unreal Engine';
			await openUproject();
		} catch (e) {
			await emit('error', e);
		}

		inAsyncOperation = false;
	};

	const handleOpenSolutionClicked = async () => {
		try {
			inAsyncOperation = true;
			asyncModalText = 'Opening Solution';
			await openSln();
		} catch (e) {
			await emit('error', e);
		}

		inAsyncOperation = false;
	};

	const handleGenerateProjectFiles = async () => {
		try {
			inAsyncOperation = true;
			asyncModalText = 'Generating project files';
			await generateSln();
			await emit('success', 'Visual Studio projects generated successfully');
		} catch (e) {
			await emit('error', e);
		}

		inAsyncOperation = false;
	};

	const handleForceDownloadGameDllsClicked = async () => {
		try {
			inAsyncOperation = true;
			asyncModalText = 'Downloading game DLLs';
			await forceDownloadDlls();
		} catch (e) {
			await emit('error', e);
		}

		inAsyncOperation = false;
	};

	const handleForceDownloadEngineClicked = async () => {
		try {
			inAsyncOperation = true;
			asyncModalText = 'Downloading engine DLLs';
			await forceDownloadEngine();
		} catch (e) {
			await emit('error', e);
		}

		inAsyncOperation = false;
	};

	const handleSyncUprojectWithEngineRepo = async () => {
		try {
			inAsyncOperation = true;
			asyncModalText = 'Syncing uproject with engine commit...';
			const commit = await syncUprojectWithEngineCommit();
			await emit('success', `UProject EngineAssociation synced to commit ${commit}`);
		} catch (e) {
			await emit('error', e);
		}

		inAsyncOperation = false;
	};

	const handleSyncEngineRepoWithUproject = async () => {
		try {
			inAsyncOperation = true;
			asyncModalText = 'Syncing uproject with engine commit...';
			const commit = await syncEngineCommitWithUproject();
			await emit('success', `Engine repo synced to commit ${commit}`);
		} catch (e) {
			await emit('error', e);
		}

		inAsyncOperation = false;
	};

	const handleReinstallGitHooksClicked = async () => {
		try {
			await reinstallGitHooks();
			await emit('success', 'Git hooks installed successfully.');
		} catch (e) {
			await emit('error', e);
		}
	};

	const refreshAndWait = async () => {
		await refresh();
	};

	onMount(() => {
		void refresh();

		const interval = setInterval(() => {
			void refresh();
		}, 30000);

		return () => {
			clearInterval(interval);
		};
	});
</script>

<div class="flex items-center justify-between gap-2">
	<div class="flex items-center gap-2">
		<p class="text-2xl my-2 text-primary-400 dark:text-primary-400">Source Repository</p>
		<Button disabled={loading || inAsyncOperation} class="!p-1.5" primary on:click={refreshAndWait}>
			{#if loading || inAsyncOperation}
				<Spinner size="4" />
			{:else}
				<RefreshOutline class="w-4 h-4" />
			{/if}
		</Button>
	</div>
	<ButtonGroup size="xs" class="space-x-px">
		<Button
			size="xs"
			color="primary"
			disabled={inAsyncOperation || conflictsDetected}
			on:click={async () => handleSyncClicked()}
		>
			<RefreshOutline class="w-3 h-3 mr-2" />
			Sync
		</Button>
		{#if conflictsDetected}
			<Tooltip
				class="ml-2 w-36 text-sm text-primary-400 bg-secondary-800 dark:bg-space-950"
				placement="bottom"
				>Conflicts detected!
			</Tooltip>
		{/if}
		<Button
			size="xs"
			color="primary"
			disabled={inAsyncOperation}
			on:click={async () => handleOpenUprojectClicked()}
		>
			<UnrealEngineLogoNoCircle class="w-3 h-3 mr-2" />
			Open Editor
		</Button>
		{#if $appConfig.pullDlls === false}
			<Button
				size="xs"
				color="primary"
				disabled={inAsyncOperation}
				on:click={async () => handleOpenSolutionClicked()}
			>
				<FileCodeSolid class="w-3.5 h-3.5 mr-2" />
				Open .sln
			</Button>
		{/if}
		<Button size="xs" color="primary" id="advancedDropdown" disabled={inAsyncOperation}>
			<ChevronDownOutline size="xs" />
		</Button>
	</ButtonGroup>
	<Dropdown placement="bottom-start" triggeredBy="#advancedDropdown">
		{#if $appConfig.pullDlls}
			<DropdownItem class="text-xs" on:click={handleForceDownloadGameDllsClicked}
				>Redownload game DLLs</DropdownItem
			>
			<Tooltip class="text-xs w-[22rem]" placement="left"
				>Downloads game DLLs for your current commit and installs them into the game repo. Use if
				you are getting incompatible binaries errors.</Tooltip
			>
		{:else}
			<DropdownItem class="text-xs" on:click={handleGenerateProjectFiles}
				>Generate project files</DropdownItem
			>
			<Tooltip class="text-xs w-[22rem]" placement="left"
				>Generates Visual Studio solution and project files for the uproject.</Tooltip
			>
		{/if}
		{#if $appConfig.engineType === 'Prebuilt'}
			<DropdownItem class="text-xs" on:click={handleForceDownloadEngineClicked}
				>Redownload engine</DropdownItem
			>
			<Tooltip class="text-xs w-[22rem]" placement="left"
				>Redownloads the entire engine archive. Use if you suspect you have a corrupt engine
				install.</Tooltip
			>
		{:else}
			<DropdownItem class="text-xs" on:click={handleSyncUprojectWithEngineRepo}
				>Sync UProject with engine commit</DropdownItem
			>
			<Tooltip class="text-xs w-[22rem]" placement="left"
				>Updates the EngineAssociation item in the .uproject to reflect the current engine commit.</Tooltip
			>
			<DropdownItem class="text-xs" on:click={handleSyncEngineRepoWithUproject}
				>Sync engine commit with UProject</DropdownItem
			>
			<Tooltip class="text-xs w-[22rem]" placement="left"
				>Updates the engine commit to the version currently set in the .uproject's EngineAssociation
				item.</Tooltip
			>
		{/if}
		<DropdownItem class="text-xs" on:click={handleReinstallGitHooksClicked}
			>Reinstall Git hooks</DropdownItem
		>
		<Tooltip class="text-xs w-[22rem]" placement="left"
			>For engineers. Helps iterate on the git hooks workflow.</Tooltip
		>
	</Dropdown>
</div>
<Card
	class="w-full p-4 sm:p-4 max-w-full bg-secondary-700 dark:bg-space-900 h-full overflow-y-hidden border-0 shadow-none"
>
	<div class="flex flex-row items-center justify-between pb-2">
		<h3 class="text-primary-400 text-xl">Commit History</h3>
		<div class="flex flex-row items-center justify-between gap-2">
			<p class="font-semibold text-sm">
				On branch: <span class="font-normal text-primary-400">{$repoStatus?.branch}</span>
			</p>
		</div>
	</div>
	<CommitTable
		commits={$commits}
		latestLocalCommit={$latestLocalCommit}
		showFilesHandler={showCommitFiles}
	/>
</Card>

<ProgressModal bind:showModal={inAsyncOperation} bind:title={asyncModalText} />
