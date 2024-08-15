<script lang="ts">
	import { Button, ButtonGroup, Card, Label, Spinner, Textarea } from 'flowbite-svelte';
	import { RotateOutline } from 'flowbite-svelte-icons';
	import { onMount } from 'svelte';
	import { emit } from '@tauri-apps/api/event';
	import { ModifiedFilesCard, ProgressModal } from '@ethos/core';
	import { get } from 'svelte/store';
	import type { PushRequest, RevertFilesRequest } from '$lib/types';
	import { getRepoStatus, lockFiles, revertFiles, submit } from '$lib/repo';
	import { allModifiedFiles, commitMessage, repoStatus, selectedFiles } from '$lib/stores';
	import { openUrl } from '$lib/utils';

	let loading = false;
	let submitting = false;

	let selectAll = false;

	$: canSubmit = $selectedFiles.length > 0 && get(commitMessage) !== '' && !loading;

	const handleOpenDirectory = async (path: string) => {
		const parent = path.split('/').slice(0, -1).join('/');

		// Birdie opens up the Y drive
		const fullPath = `Y:/${parent}`;

		await openUrl(fullPath);
	};

	const refreshFiles = async (triggerLoading: boolean) => {
		if (triggerLoading) {
			loading = true;
		}

		try {
			$repoStatus = await getRepoStatus();
		} catch (e) {
			await emit('error', e);
		}

		// clear selected files if they no longer exist
		$selectedFiles = $selectedFiles.filter(
			(file) =>
				$repoStatus?.modifiedFiles.some((f) => f.path === file.path) ||
				$repoStatus?.untrackedFiles.some((f) => f.path === file.path)
		);

		if (triggerLoading) {
			loading = false;
		}
	};

	const handleRevertFiles = async () => {
		loading = true;

		await refreshFiles(false);

		const req: RevertFilesRequest = {
			files: $selectedFiles.map((file) => file.path),
			skipEngineCheck: false
		};

		try {
			await revertFiles(req);

			$repoStatus = await getRepoStatus();

			$selectedFiles = [];
			selectAll = false;
		} catch (e) {
			await emit('error', e);
		}

		loading = false;
	};

	const handleSubmit = async () => {
		loading = true;
		submitting = true;

		await refreshFiles(false);

		const req: PushRequest = {
			commitMessage: $commitMessage,
			files: $selectedFiles.map((file) => file.path)
		};

		try {
			await submit(req);

			$repoStatus = await getRepoStatus();

			$commitMessage = '';
			$selectedFiles = [];
			selectAll = false;
		} catch (e) {
			await emit('error', e);
		}

		submitting = false;
		loading = false;
	};

	const refreshLocks = async () => {
		loading = true;
		try {
			repoStatus.set(await getRepoStatus());
		} catch (e) {
			await emit('error', e);
		}
		loading = false;
	};

	const handleLockSelected = async () => {
		loading = true;

		try {
			const selectedPaths = $selectedFiles.map((file) => file.path);
			await lockFiles(selectedPaths);
			await emit('success', 'Files locked successfully');
			await refreshLocks();
		} catch (e) {
			await emit('error', e);
		}
		loading = false;
	};

	onMount(() => {
		void refreshFiles(true);

		const interval = setInterval(async () => {
			if (!submitting) {
				await refreshFiles(true);
			}
		}, 10000);

		return () => {
			clearInterval(interval);
		};
	});
</script>

<div class="flex items-center w-full justify-between gap-2">
	<div class="flex items-center gap-2">
		<p class="text-2xl my-2 dark:text-primary-400">Submit Changes</p>
		<Button disabled={loading} class="!p-1.5" primary on:click={() => refreshFiles(true)}>
			{#if loading}
				<Spinner size="4" />
			{:else}
				<RotateOutline class="w-4 h-4" />
			{/if}
		</Button>
	</div>
</div>
<div class="flex h-full w-full gap-2 overflow-hidden">
	<div class="flex flex-col overflow-hidden w-full">
		<ModifiedFilesCard
			disabled={loading}
			bind:selectedFiles={$selectedFiles}
			bind:selectAll
			onOpenDirectory={handleOpenDirectory}
			modifiedFiles={$allModifiedFiles}
			onRevertFiles={handleRevertFiles}
			snapshotsEnabled={false}
			onLockSelected={handleLockSelected}
		/>
	</div>
	<div class="flex flex-col h-full gap-2 w-full max-w-[24rem]">
		<Card
			class="w-full h-13 p-4 sm:p-4 max-w-full max-h-16 dark:bg-secondary-600 border-0 shadow-none"
		>
			<div class="flex flex-row items-center justify-between gap-2">
				<p class="font-semibold text-sm">
					On branch: <span class="font-normal text-primary-400">{$repoStatus?.branch}</span>
				</p>
			</div>
		</Card>
		<Card
			class="w-full p-4 sm:p-4 max-w-full h-full max-h-[20rem] dark:bg-secondary-600 border-0 shadow-none"
		>
			<div class="flex flex-col w-full h-full gap-2">
				<Label for="commit-message" class="mb-2">Commit Message</Label>
				<Textarea
					id="commit-message"
					bind:value={$commitMessage}
					class="dark:bg-secondary-800 min-h-[4rem] h-full"
				/>
				<div class="flex flex-row w-full align-middle justify-end">
					<ButtonGroup class="space-x-px">
						<Button color="primary" disabled={!canSubmit} on:click={handleSubmit}>Submit</Button>
					</ButtonGroup>
				</div>
			</div>
		</Card>
	</div>
</div>
<ProgressModal bind:showModal={submitting} title="Submitting" />
