<script lang="ts">
	import {
		Badge,
		Button,
		ButtonGroup,
		Card,
		Spinner,
		Table,
		TableBody,
		TableBodyCell,
		TableBodyRow,
		TableHead,
		TableHeadCell
	} from 'flowbite-svelte';
	import { LinkOutline, RefreshOutline } from 'flowbite-svelte-icons';
	import { type Nullable, ProgressModal } from '@ethos/core';
	import { onMount } from 'svelte';
	import { emit } from '@tauri-apps/api/event';
	import { get } from 'svelte/store';
	import { generateSln, getMergeQueue, getRepoStatus, openUproject, syncLatest } from '$lib/repo';
	import type {
		ArtifactEntry,
		GameServerResult,
		MergeQueue,
		MergeQueueEntry,
		Playtest,
		SyncClientRequest
	} from '$lib/types';
	import PlaytestCard from '$lib/components/playtests/PlaytestCard.svelte';
	import {
		allModifiedFiles,
		appConfig,
		builds,
		nextPlaytest,
		playtests,
		repoStatus
	} from '$lib/stores';
	import { getPlaytestGroupForUser, getPlaytests } from '$lib/playtests';
	import { getBuilds, syncClient } from '$lib/builds';
	import { getServers } from '$lib/gameServers';

	let loadingMergeQueue = false;
	let loadingPlaytests = false;
	let loadingRepoStatus = false;
	let mergeQueue: Nullable<MergeQueue> = null;
	let syncing = false;
	let progressModalText = '';

	const refreshPlaytests = async () => {
		loadingPlaytests = true;
		$playtests = await getPlaytests();
		loadingPlaytests = false;
	};

	const refreshRepo = async () => {
		loadingRepoStatus = true;
		$repoStatus = await getRepoStatus();
		loadingRepoStatus = false;
	};

	const refreshMergeQueue = async () => {
		loadingMergeQueue = true;
		mergeQueue = await getMergeQueue();
		loadingMergeQueue = false;
	};

	const refresh = async () => {
		await Promise.all([refreshPlaytests(), refreshRepo(), refreshMergeQueue()]);
	};

	const shouldDisableLaunchButton = (): boolean => {
		const pt = get(nextPlaytest);
		if (pt) {
			const playtestAssignment = getPlaytestGroupForUser($nextPlaytest, $appConfig.userDisplayName);
			if (playtestAssignment && playtestAssignment.serverRef) {
				return false;
			}
		}

		return true;
	};

	const getMainButtonText = (): string => {
		const pt = get(nextPlaytest);
		if (pt) {
			const playtestAssignment = getPlaytestGroupForUser($nextPlaytest, $appConfig.userDisplayName);
			if (playtestAssignment && playtestAssignment.serverRef) {
				return `Sync & Join Playtest (${playtestAssignment.serverRef.name})`;
			}
		}

		return 'No playtest to join!';
	};

	const getMergeQueueEntryBadgeClass = (node: MergeQueueEntry): string => {
		if (node.state === 'AWAITING_CHECKS' || node.state === 'QUEUED') {
			return 'bg-yellow-500 dark:bg-yellow-500 animate-pulse';
		}

		if (node.state === 'UNMERGEABLE') {
			return 'bg-red-700 dark:bg-red-700';
		}

		return 'bg-secondary-500 dark:bg-secondary-500';
	};

	const handleSyncClicked = async () => {
		try {
			syncing = true;
			progressModalText = 'Pulling latest with git';
			await syncLatest();

			if ($appConfig.pullDlls === false) {
				progressModalText = 'Generating projects';
				await generateSln();
			} else if ($appConfig.openUprojectAfterSync === true) {
				progressModalText = 'Launching Unreal Engine';
				await openUproject();
			}

			await emit('success', 'Sync complete!');
		} catch (e) {
			await emit('error', e);
		}

		syncing = false;
	};

	const handleSyncClient = async (entry: Nullable<ArtifactEntry>, server: GameServerResult) => {
		if (!entry) {
			return;
		}

		syncing = true;
		progressModalText = 'Syncing client...';
		const req: SyncClientRequest = {
			artifactEntry: entry,
			methodPrefix: $builds.methodPrefix,
			launchOptions: {
				ip: server.ip,
				port: server.port,
				netimguiPort: server.netimguiPort
			}
		};

		try {
			await syncClient(req);
		} catch (e) {
			await emit('error', e);
		}

		syncing = false;
	};

	const isPlaytestJoinable = (playtest: Nullable<Playtest>): boolean => {
		if (!playtest) {
			return false;
		}

		const playtestAssignment = getPlaytestGroupForUser(playtest, $appConfig.userDisplayName);
		if (!playtestAssignment) {
			return false;
		}

		if (!playtestAssignment.serverRef) {
			return false;
		}

		return true;
	};

	$: enableLaunch = isPlaytestJoinable($nextPlaytest);

	const handleSyncAndLaunch = async () => {
		const playtest = get(nextPlaytest);
		if (playtest) {
			const playtestAssignment = getPlaytestGroupForUser(playtest, $appConfig.userDisplayName);
			if (playtestAssignment && playtestAssignment.serverRef) {
				const project = playtest.metadata.annotations['believer.dev/project'];
				const entry = await getBuilds(250, project).then((a) =>
					a.entries.find((b) => b.commit === playtest.spec.version)
				);

				const updatedServers = await getServers(playtest.spec.version);
				const playtestServer = updatedServers.find(
					(s) => s.name === playtestAssignment.serverRef?.name
				);

				if (playtestServer && entry) {
					await handleSyncClient(entry, playtestServer);
				}
			}
		}
	};

	onMount(() => {
		void refresh();

		const interval = setInterval(() => {
			void refresh();
		}, 10000);

		return () => clearInterval(interval);
	});
</script>

<div class="flex flex-col h-full gap-2">
	<div class="flex flex-row h-full gap-2 max-h-[60vh]">
		{#if $nextPlaytest !== null}
			<div class="flex flex-col gap-2 w-full h-full overflow-x-auto">
				<div class="flex mt-2 items-center gap-2">
					<p class="text-2xl text-primary-400 dark:text-primary-400">Next Playtest</p>
					<Button
						disabled={loadingPlaytests}
						class="!p-1.5"
						primary
						on:click={() => refreshPlaytests()}
					>
						{#if loadingPlaytests}
							<Spinner size="4" />
						{:else}
							<RefreshOutline class="w-4 h-4" />
						{/if}
					</Button>
				</div>

				<Card
					class="w-full h-full p-0 sm:p-0 max-w-full bg-secondary-700 dark:bg-space-900 border-0 shadow-none overflow-y-hidden"
				>
					<PlaytestCard playtest={$nextPlaytest} bind:loading={loadingPlaytests} compact />
					{#if getPlaytestGroupForUser(get(nextPlaytest), $appConfig.userDisplayName) !== null}
						<div class="flex justify-end gap-2 px-4 pb-4">
							<Button
								color="primary"
								class="w-full"
								disabled={!enableLaunch}
								on:click={handleSyncAndLaunch}>Sync & Launch</Button
							>
						</div>
					{/if}
				</Card>
			</div>
		{:else}
			<div class="flex gap-2 items-center">
				<p class="text-white">There are no playtests!</p>
				<Button size="xs" href="/playtests">Playtests<LinkOutline class="ml-2 h-4 w-4" /></Button>
			</div>
		{/if}
		<div class="flex flex-col gap-2 h-full max-w-[20rem] w-80">
			<div class="flex mt-2 items-center gap-2">
				<p class="text-2xl text-primary-400 dark:text-primary-400">Repo Status</p>
				<Button disabled={loadingRepoStatus} class="!p-1.5" primary on:click={refreshRepo}>
					{#if loadingRepoStatus}
						<Spinner size="4" />
					{:else}
						<RefreshOutline class="w-4 h-4" />
					{/if}
				</Button>
			</div>
			<Card
				class="w-full h-full p-4 sm:p-4 max-w-full bg-secondary-700 dark:bg-space-900 border-0 shadow-none"
			>
				<div class="flex flex-col h-full justify-between">
					<div class="flex flex-col gap-1">
						<div class="flex gap-2 items-center">
							<p class="w-60 text-white">Branch:</p>
							<p class="w-60 text-primary-400 dark:text-primary-400">{$repoStatus?.branch}</p>
						</div>
						<div class="flex gap-2 items-center">
							<p class="w-full text-white">Commits behind:</p>
							<p class="w-full text-primary-400 dark:text-primary-400">
								{$repoStatus?.commitsBehind}
							</p>
						</div>
						<div class="flex gap-2 items-center">
							<p class="w-full text-white">Modified files:</p>
							<p class="w-full text-primary-400 dark:text-primary-400">
								{$allModifiedFiles.length}
							</p>
						</div>
						<div class="flex gap-2 items-center">
							<p class="w-full text-white">Conflicting files:</p>
							<p class="w-full text-primary-400 dark:text-primary-400">
								{$repoStatus?.conflicts.length}
							</p>
						</div>
					</div>
					<ButtonGroup size="xs" class="space-x-px mt-4 w-full">
						<Button color="primary" class="w-full" on:click={handleSyncClicked}>Sync</Button>
						<Button color="primary" href="/source/submit" class="w-full"
							>Submit<LinkOutline class="ml-4 w-4 h-4l" /></Button
						>
					</ButtonGroup>
				</div>
			</Card>
		</div>
	</div>
	<div class="h-full flex flex-col overflow-hidden">
		<div class="flex items-center gap-2">
			<p class="text-2xl my-2 text-primary-400 dark:text-primary-400">Merge Queue</p>
			<Button disabled={loadingMergeQueue} class="!p-1.5" primary on:click={refreshMergeQueue}>
				{#if loadingMergeQueue}
					<Spinner size="4" />
				{:else}
					<RefreshOutline class="w-4 h-4" />
				{/if}
			</Button>
		</div>
		<Card
			class="w-full p-4 sm:p-4 max-w-full bg-secondary-700 dark:bg-space-900 border-0 shadow-none"
		>
			{#if mergeQueue !== null}
				{#if mergeQueue.entries.nodes.length === 0}
					<p class="text-white">No changes in queue!</p>
				{:else}
					<Table color="custom" divClass="w-full" class="w-full h-full" striped>
						<TableHead class="text-center border-b-0 p-2 bg-secondary-800 dark:bg-space-950">
							<TableHeadCell class="p-2">#</TableHeadCell>
							<TableHeadCell class="p-2">Message</TableHeadCell>
							<TableHeadCell class="p-2">Author</TableHeadCell>
							<TableHeadCell class="p-2">Submitted At</TableHeadCell>
							<TableHeadCell class="p-2">Status</TableHeadCell>
						</TableHead>
						<TableBody>
							{#each mergeQueue.entries.nodes as node, index}
								<TableBodyRow
									class="text-left border-b-0 p-2 {index % 2 === 0
										? 'bg-secondary-700 dark:bg-space-900'
										: 'bg-secondary-800 dark:bg-space-950'}"
								>
									<TableBodyCell id="pr-{index}" class="p-2">
										{index + 1}
									</TableBodyCell>
									<TableBodyCell
										class="p-2 text-primary-400 dark:text-primary-400 break-normal overflow-ellipsis overflow-hidden whitespace-nowrap w-1/2 max-w-[22vw]"
									>
										{node.headCommit.message.split('\n')[0]}
									</TableBodyCell>
									<TableBodyCell class="p-2 text-center"
										>{node.headCommit.author.name}</TableBodyCell
									>
									<TableBodyCell class="p-2 text-center"
										>{new Date(node.enqueuedAt).toLocaleString()}</TableBodyCell
									>
									<TableBodyCell class="p-2">
										<Badge
											class="text-white dark:text-white w-full {getMergeQueueEntryBadgeClass(node)}"
											>Queued
										</Badge>
									</TableBodyCell>
								</TableBodyRow>
							{/each}
						</TableBody>
					</Table>
				{/if}
			{/if}
		</Card>
	</div>
</div>

{#key $nextPlaytest?.status}
	<Button
		disabled={shouldDisableLaunchButton()}
		size="xl"
		class="fixed bottom-6 right-6 shadow-2xl"
		on:click={handleSyncAndLaunch}
		>{getMainButtonText()}
	</Button>
{/key}

<ProgressModal title={progressModalText} bind:showModal={syncing} />