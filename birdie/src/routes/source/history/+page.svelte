<script lang="ts">
	import { Button, ButtonGroup, Card, Spinner } from 'flowbite-svelte';
	import { onMount } from 'svelte';
	import { RotateOutline } from 'flowbite-svelte-icons';
	import { emit } from '@tauri-apps/api/event';
	import { CommitTable, ProgressModal } from '@ethos/core';
	import { getAllCommits, syncLatest, getRepoStatus, showCommitFiles, cloneRepo } from '$lib/repo';
	import { appConfig, commits, latestLocalCommit, repoStatus } from '$lib/stores';

	import { runSetEnv, syncTools } from '$lib/tools';
	import { getAppConfig } from '$lib/config';

	let loading = false;
	let inAsyncOperation = false;
	let asyncModalText = '';

	const refresh = async () => {
		loading = true;
		repoStatus.set(await getRepoStatus());
		commits.set(await getAllCommits());
		loading = false;
	};

	const handleSyncClicked = async () => {
		try {
			inAsyncOperation = true;
			asyncModalText = 'Pulling latest with git';

			await syncLatest();
			await refresh();
		} catch (e) {
			await emit('error', e);
		}

		inAsyncOperation = false;
	};

	const handleSyncToolsClicked = async () => {
		try {
			inAsyncOperation = true;
			asyncModalText = 'Pulling tools with git';

			try {
				$appConfig = await getAppConfig();
			} catch (e) {
				await emit('error', e);
			}

			const didSync: boolean = await syncTools();
			if (!didSync && $appConfig.toolsPath && $appConfig.toolsUrl) {
				await cloneRepo({ url: $appConfig.toolsUrl, path: $appConfig.toolsPath });
				await runSetEnv();
			}
		} catch (e) {
			await emit('error', e);
		}

		inAsyncOperation = false;
	};

	const refreshAndWait = async () => {
		await refresh();
	};

	onMount(() => {
		void refresh();

		const interval = setInterval(refresh, 30000);

		return () => {
			clearInterval(interval);
		};
	});
</script>

<div class="flex items-center justify-between gap-2">
	<div class="flex items-center gap-2">
		<p class="text-2xl my-2 dark:text-primary-400">Source Repository</p>
		<Button disabled={loading || inAsyncOperation} class="!p-1.5" primary on:click={refreshAndWait}>
			{#if loading || inAsyncOperation}
				<Spinner size="4" />
			{:else}
				<RotateOutline class="w-4 h-4" />
			{/if}
		</Button>
	</div>
	<ButtonGroup size="xs" class="space-x-px">
		<Button
			size="xs"
			color="primary"
			disabled={inAsyncOperation}
			on:click={async () => handleSyncClicked()}
		>
			<RotateOutline class="w-3 h-3 mr-2" />
			Sync
		</Button>
		<Button
			size="xs"
			color="primary"
			disabled={inAsyncOperation}
			on:click={async () => handleSyncToolsClicked()}
		>
			<RotateOutline class="w-3 h-3 mr-2" />
			Tools
		</Button>
	</ButtonGroup>
</div>
<Card
	class="w-full p-4 sm:p-4 max-w-full dark:bg-secondary-600 h-full overflow-y-hidden border-0 shadow-none"
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
