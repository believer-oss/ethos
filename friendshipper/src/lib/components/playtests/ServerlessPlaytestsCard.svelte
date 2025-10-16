<script lang="ts">
	import { Button, ButtonGroup, Card, Hr } from 'flowbite-svelte';
	import { EditOutline } from 'flowbite-svelte-icons';
	import { emit } from '@tauri-apps/api/event';
	import { ProgressModal } from '@ethos/core';
	import type { ArtifactEntry, Nullable, Playtest, SyncClientRequest } from '$lib/types';
	import { LaunchMode } from '$lib/types';
	import { appConfig, backgroundSyncInProgress, builds, currentSyncedVersion } from '$lib/stores';
	import { handleError } from '$lib/utils';
	import { cancelDownload, getBuild, getBuilds, syncClient } from '$lib/builds';

	export let playtests: Playtest[];
	export let showTitle: boolean = true;
	export let handleEditPlaytest: ((playtest: Playtest | null) => void) | null = null;

	let syncing = false;
	let backgroundSyncing = false;
	let progressModalText = '';
	let progressModalCancellable = false;

	const handleProgressModalCancel = async () => {
		try {
			await cancelDownload();
		} catch (e) {
			await handleError(e);
		}

		progressModalCancellable = false;
		syncing = false;
	};

	const handleSyncClient = async (playtest: Playtest, entry: Nullable<ArtifactEntry>) => {
		if (!entry) {
			return;
		}

		progressModalCancellable = true;
		progressModalText = 'Syncing client...';
		const req: SyncClientRequest = {
			artifactEntry: entry,
			methodPrefix: $builds.methodPrefix
		};

		if ($appConfig.groupDownloadedBuildsByPlaytest) {
			req.subPath = playtest.metadata.name;
		}

		syncing = true;
		backgroundSyncing = true;
		await emit('background-sync-start');

		try {
			if (await syncClient(req)) {
				currentSyncedVersion.set(entry.commit);
			}
		} catch (e) {
			await emit('error', e);
		}

		syncing = false;

		if (backgroundSyncing) {
			await emit('background-sync-end');
			backgroundSyncing = false;
		}

		progressModalCancellable = false;
	};

	const handleSyncAndLaunch = async (playtest: Playtest, launch: boolean = false) => {
		try {
			if (playtest.metadata.annotations) {
				const project = playtest.metadata.annotations['believer.dev/project'];
				let entry = await getBuilds(250, project).then((a) =>
					a.entries.find((b) => b.commit === playtest.spec.version)
				);

				if (!entry) {
					entry = await getBuild(playtest.spec.version, project);

					if (!entry) {
						await emit('error', 'No build found for playtest');
						return;
					}
				}

				if (entry) {
					if (launch) {
						const req: SyncClientRequest = {
							artifactEntry: entry,
							methodPrefix: $builds.methodPrefix,
							launchOptions: {
								name: '',
								launchMode: LaunchMode.WithoutServer
							}
						};

						if ($appConfig.groupDownloadedBuildsByPlaytest) {
							req.subPath = playtest.metadata.name;
						}

						try {
							await syncClient(req);
						} catch (e) {
							await emit('error', e);
						}
					} else {
						await handleSyncClient(playtest, entry);
					}
				} else {
					await emit('error', 'No build found for playtest');
				}
			}
		} catch (e) {
			await emit('error', e);
		}
	};

	const getPlaytestStartString = (item: Playtest): string => {
		const date = new Date(item.spec.startTime);
		return `${date.toLocaleDateString()} ${date.toLocaleTimeString()}`;
	};

	// Sort playtests by start time (most recent first)
	$: sortedPlaytests = [...playtests].sort((a, b) => {
		const timeA = new Date(a.spec.startTime).getTime();
		const timeB = new Date(b.spec.startTime).getTime();
		return timeB - timeA;
	});
</script>

<Card class="w-full p-4 sm:p-4 max-w-full bg-secondary-700 dark:bg-space-900 border-0 shadow-none">
	{#if showTitle}
		<div class="flex items-center gap-2 mb-4">
			<h3 class="text-2xl font-light tracking-tight text-primary-400">Serverless Playtests</h3>
			<span class="text-sm text-gray-400">({sortedPlaytests.length} scheduled)</span>
		</div>
	{/if}

	<div class="flex flex-col">
		{#each sortedPlaytests as playtest, index}
			<div class="flex items-center justify-between gap-4 py-3">
				<div class="flex flex-col gap-1 flex-1">
					<div class="flex items-center gap-2">
						<span class="text-lg font-medium text-primary-400">{playtest.spec.displayName}</span>
						<span class="text-xs text-gray-400">
							<code>{playtest.spec.version.substring(0, 8)}</code>
						</span>
						{#if playtest.metadata.annotations && playtest.metadata.annotations['believer.dev/owner']}
							<span class="text-xs text-gray-400">
								by {playtest.metadata.annotations['believer.dev/owner']}
							</span>
						{/if}
					</div>
					<div class="text-sm text-gray-400">
						created: {getPlaytestStartString(playtest)}
					</div>
				</div>

				<ButtonGroup class="space-x-px">
					{#if handleEditPlaytest !== null}
						<Button
							color="primary"
							size="xs"
							class="py-1"
							on:click={() => {
								if (handleEditPlaytest !== null) handleEditPlaytest(playtest);
							}}
						>
							<EditOutline class="w-3 h-3" />
						</Button>
					{/if}
					{#key playtest}
						{#if $currentSyncedVersion === playtest.spec.version}
							<Button
								size="xs"
								class="text-xs py-1"
								disabled={$backgroundSyncInProgress}
								color="primary"
								on:click={() => handleSyncAndLaunch(playtest, true)}
							>
								Launch
							</Button>
						{/if}
						<Button
							size="xs"
							class="text-xs py-1"
							disabled={$backgroundSyncInProgress ||
								$currentSyncedVersion === playtest.spec.version}
							color="primary"
							on:click={() => handleSyncAndLaunch(playtest, false)}
						>
							{$currentSyncedVersion === playtest.spec.version ? 'Synced' : 'Sync Client'}
						</Button>
					{/key}
				</ButtonGroup>
			</div>
			{#if index < sortedPlaytests.length - 1}
				<Hr classHr="my-0 bg-gray-600 dark:bg-gray-600" />
			{/if}
		{/each}
	</div>
</Card>

<ProgressModal
	title={progressModalText}
	showModal={syncing && !backgroundSyncing}
	cancellable={progressModalCancellable}
	onCancel={handleProgressModalCancel}
/>
