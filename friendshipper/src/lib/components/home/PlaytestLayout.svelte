<script lang="ts">
	import { Button, Card, Label, Select, Spinner, Tooltip } from 'flowbite-svelte';
	import { invoke } from '@tauri-apps/api/tauri';
	import { onMount } from 'svelte';
	import { get } from 'svelte/store';
	import { CirclePlusOutline, FolderOpenOutline } from 'flowbite-svelte-icons';
	import { emit, listen } from '@tauri-apps/api/event';
	import { ProgressModal } from '@ethos/core';
	import {
		activeProjectConfig,
		appConfig,
		backgroundSyncInProgress,
		builds,
		builtCommits,
		currentSyncedVersion,
		playtests,
		selectedCommit,
		workflowMap
	} from '$lib/stores';
	import PlaytestCard from '$lib/components/playtests/PlaytestCard.svelte';
	import type {
		ArtifactEntry,
		GameServerResult,
		LaunchRequest,
		Nullable,
		Playtest,
		QuickLaunchEvent,
		SyncClientRequest,
		TauriError
	} from '$lib/types';
	import { getServers, launchServer, openLogsFolder } from '$lib/gameServers';
	import { syncClient, getBuilds, getBuild } from '$lib/builds';
	import ServerModal from '$lib/components/servers/ServerModal.svelte';
	import { getPlaytestGroupForUser } from '$lib/playtests';
	import ServerTable from '$lib/components/servers/ServerTable.svelte';
	import { restart } from '$lib/system';
	import { checkLoginRequired } from '$lib/auth';

	// Loading states
	let playtestLoading = false;
	let syncing = false;
	let verifyingServer = true;
	let buildVerified = false;
	let fetchingServers = false;
	let fetchingBuilds = false;
	let showServerModal = false;

	let servers: GameServerResult[] = [];
	let selected: Nullable<ArtifactEntry> = get(selectedCommit);
	let recentCommits = get(builtCommits);

	const getNextPlaytest = (pts: Playtest[]): Nullable<Playtest> => {
		if (pts.length > 0) {
			const nextAssigned = pts.find(
				(p) => getPlaytestGroupForUser(p, $appConfig.userDisplayName) != null
			);

			return nextAssigned || pts[0];
		}

		return null;
	};

	$: nextPlaytest = getNextPlaytest($playtests);

	const verifyBuild = async (commit: ArtifactEntry) => {
		verifyingServer = true;
		try {
			buildVerified = await invoke('verify_build', { commit: commit.commit });
		} catch (e) {
			await emit('error', e);
		}
		verifyingServer = false;
	};

	const updateServers = async (commit: string) => {
		if ($appConfig.initialized) {
			fetchingServers = true;
			try {
				servers = await getServers(commit);
			} catch (e) {
				await emit('error', e);

				const error = e as TauriError;
				if (error.status_code === 401) {
					// check auth status
					const loginRequired = await checkLoginRequired();
					if (loginRequired) {
						await restart();
					}
				}
			}
			fetchingServers = false;
		}
	};

	const handleCommitChange = async (newCommit: Nullable<ArtifactEntry>) => {
		if (newCommit === null) {
			return;
		}

		selectedCommit.set(newCommit);

		const buildsPromise = verifyBuild(newCommit);
		const serversPromise = updateServers(newCommit.commit);

		try {
			await Promise.all([buildsPromise, serversPromise]);
		} catch (e) {
			await emit('error', e);
		}
	};

	$: void handleCommitChange(selected);

	$: $builtCommits,
		() => {
			recentCommits = get(builtCommits);
		};

	$: $selectedCommit,
		() => {
			selected = get(selectedCommit);
		};

	const handleServerCreate = async () => {
		if (selected === null) {
			return;
		}

		try {
			await updateServers(selected.commit);
			selected = get(selectedCommit);
		} catch (e) {
			await emit('error', e);
		}
	};

	const handleSyncClient = async (entry: Nullable<ArtifactEntry>, server: GameServerResult) => {
		if (entry === null) {
			return;
		}

		syncing = true;
		const req: SyncClientRequest = {
			artifactEntry: entry,
			methodPrefix: $builds.methodPrefix,
			launchOptions: {
				name: server.name
			}
		};

		try {
			await syncClient(req);
		} catch (e) {
			await emit('error', e);
		}

		currentSyncedVersion.set(entry.commit);
		syncing = false;
	};

	const shouldDisableLaunchButton = (): boolean => {
		if (nextPlaytest !== null) {
			const playtestAssignment = getPlaytestGroupForUser(nextPlaytest, $appConfig.userDisplayName);
			if (playtestAssignment && playtestAssignment.serverRef) {
				return false;
			}
		}
		return syncing || !buildVerified;
	};

	const getMainButtonText = (): string => {
		if (nextPlaytest !== null) {
			const playtestAssignment = getPlaytestGroupForUser(nextPlaytest, $appConfig.userDisplayName);
			if (playtestAssignment && playtestAssignment.serverRef) {
				return `Sync & Join Playtest (${playtestAssignment.serverRef.name})`;
			}
		}
		return `Sync & Launch (${
			servers.length > 0 ? servers[0].displayName || servers[0].name : 'New Server'
		})`;
	};

	const handleSyncAndLaunch = async () => {
		if (selected === null) {
			return;
		}

		if (nextPlaytest !== null) {
			const playtestAssignment = getPlaytestGroupForUser(nextPlaytest, $appConfig.userDisplayName);
			if (playtestAssignment && playtestAssignment.serverRef) {
				const project = nextPlaytest.metadata.annotations?.['believer.dev/project'];
				let entry = project
					? await getBuilds(250, project).then((a) =>
							a.entries.find((b) => b.commit === nextPlaytest.spec.version)
					  )
					: null;

				if (!entry) {
					entry = await getBuild(nextPlaytest.spec.version, project);

					if (!entry) {
						await emit('error', 'No build found for playtest');
						return;
					}
				}

				if (entry !== selected) {
					const updatedServers = await getServers(nextPlaytest?.spec.version);
					const playtestServer = updatedServers.find(
						(s) => s.name === playtestAssignment.serverRef?.name
					);

					if (playtestServer && entry) {
						if (playtestServer.ready) {
							await handleSyncClient(entry, playtestServer);
						} else {
							await emit('error', 'Playtest server is not ready. Try again shortly.');
						}
						return;
					}
				} else {
					const playtestServer = servers.find((s) => s.name === playtestAssignment.serverRef?.name);

					if (playtestServer && entry) {
						if (playtestServer.ready) {
							await handleSyncClient(entry, playtestServer);
						} else {
							await emit('error', 'Playtest server is not ready. Try again shortly.');
						}
						return;
					}
				}
			}
		}
		if (servers.length > 0) {
			if (servers[0].ready) {
				await handleSyncClient(selected, servers[0]);
			} else {
				await emit('error', 'Server is not ready. Try again shortly.');
			}

			return;
		}

		const name = 'DefaultServerName';

		const launchRequest: LaunchRequest = {
			commit: selected.commit,
			displayName: name,
			checkForExisting: false
		};

		if ($activeProjectConfig) {
			[launchRequest.map] = $activeProjectConfig.maps;
		}

		try {
			await launchServer(launchRequest);
		} catch (e) {
			await emit('error', e);
		}

		await updateServers(selected.commit);

		const server = servers.find((s) => s.displayName === name);
		if (server) {
			if (server.ready) {
				await handleSyncClient(selected, server);
			} else {
				await emit('error', 'Server is not ready. Try again shortly.');
			}
		}
	};

	void listen('quick-launch', (event) => {
		const quickLaunchEvent = event.payload as QuickLaunchEvent;
		void handleSyncClient(quickLaunchEvent.artifactEntry, quickLaunchEvent.server);
		if (quickLaunchEvent.server.ready) {
			void handleSyncClient(quickLaunchEvent.artifactEntry, quickLaunchEvent.server);
		} else {
			void emit('error', 'Server is not ready. Try again shortly.');
		}
	});

	onMount(() => {
		if (selected !== null) {
			void verifyBuild(selected);
			void updateServers(selected.commit);
		}

		const interval = setInterval(() => {
			if (selected !== null) {
				void updateServers(selected.commit);
			}
		}, 30000);

		return () => {
			clearInterval(interval);
		};
	});
</script>

<div class="grid grid-cols-1 xl:grid-cols-2 gap-2 overflow-auto">
	<div class="flex flex-col h-full min-h-full">
		<div class="flex items-center gap-2">
			<p class="text-2xl my-2 text-primary-400 dark:text-primary-400">Recent Game Versions</p>
		</div>
		{#if selected === null}
			<Card
				class="w-full h-full p-4 sm:p-4 max-w-full bg-secondary-700 dark:bg-space-900 border-0 shadow-none"
			>
				<div class="text-center text-xl">No builds were found for this repository.</div>
			</Card>
		{:else}
			<Card
				class="w-full h-full p-4 sm:p-4 max-w-full bg-secondary-700 dark:bg-space-900 border-0 shadow-none"
			>
				<div class="flex items-center justify-between gap-2">
					<Label class="flex items-center gap-2 align-middle text-xs">
						<span class="align-middle text-white">Version</span>
						<Select
							size="sm"
							class="bg-secondary-700 dark:bg-space-900 w-56 border-white dark:border-white text-white dark:text-white"
							placeholder="Select a commit"
							bind:value={selected}
							required
						>
							{#if recentCommits}
								{#each recentCommits as commit}
									<option value={commit.value}
										>{commit.name.substring(0, 8)}
										{$workflowMap.get(commit.name)?.message || ''}</option
									>
								{/each}
							{/if}
						</Select>
						{#if recentCommits.length > 0 && selected.commit === recentCommits[0].value.commit}
							<span class="text-white">✅ Latest</span>
						{/if}
					</Label>
					<div class="flex flex-col">
						<p class="text-sm text-gray-400 dark:text-gray-400 align-middle">
							Build timestamp: {new Date(selected.lastModified * 1000).toLocaleString()}
						</p>
						{#if verifyingServer}
							<p class="text-sm text-gray-400 dark:text-gray-400 align-middle text-right">
								Verifying build...
							</p>
						{:else if buildVerified}
							<p class="text-sm text-green-400 dark:text-lime-500 align-middle text-right">
								Server build verified ✅
							</p>
						{:else}
							<p class="text-sm text-red-500 dark:text-red-500 align-middle text-right">
								Server build not verified ❌
							</p>
						{/if}
						{#if $workflowMap.get(selected.commit)}
							<p class="text-sm text-gray-400 dark:text-gray-400 align-middle text-right">
								Committed by: {$workflowMap.get(selected.commit)?.pusher}
							</p>
						{/if}
					</div>
				</div>

				<div class="flex items-center justify-between gap-2 mt-2">
					<div class="flex items-center gap-2 mt-2">
						<p class="text-xl text-primary-400">Servers</p>
						<Button
							disabled={syncing || !buildVerified || fetchingBuilds}
							class="!p-1.5"
							size="xs"
							on:click={async () => {
								fetchingBuilds = true;
								builds.set(await getBuilds(250));
								fetchingBuilds = false;
								showServerModal = true;
							}}
						>
							<CirclePlusOutline class="w-4 h-4" />
						</Button>
						<Tooltip
							class="w-auto text-xs text-primary-400 bg-secondary-600 dark:bg-space-800"
							placement="right"
							>Launch a new server
						</Tooltip>
						{#if fetchingServers}
							<Spinner size="4" />
						{/if}
					</div>
					<Button outline class="!p-1.5" size="xs" on:click={openLogsFolder}>
						<FolderOpenOutline class="w-4 h-4 mr-1" />
						Server logs
					</Button>
					<Tooltip
						class="w-auto text-xs text-primary-400 bg-secondary-600 dark:bg-space-800"
						placement="bottom"
						>Open server logs folder
					</Tooltip>
				</div>

				<ServerModal
					bind:showModal={showServerModal}
					initialEntry={selected}
					{handleServerCreate}
				/>

				<div class="max-h-[20vh] mt-2 p-2 border dark:border rounded-lg">
					<ServerTable
						{servers}
						onUpdateServers={async () => {
							if (selected !== null) {
								await updateServers(selected.commit);
							}
						}}
					/>
				</div>
			</Card>
		{/if}
	</div>
	<div class="h-full min-h-full flex flex-col overflow-hidden">
		<div class="flex items-center gap-2">
			<p class="text-2xl my-2 text-primary-400 dark:text-primary-400">Next Playtest</p>
			{#if playtestLoading}
				<Spinner size="4" />
			{/if}
		</div>

		<div class="pb-24 xl:pb-0 overflow-y-hidden h-full">
			{#if nextPlaytest !== null}
				<PlaytestCard playtest={nextPlaytest} bind:loading={playtestLoading} />
			{:else}
				<p class="text-xl my-2 text-primary-400 dark:text-primary-400">No playtests scheduled</p>
			{/if}
		</div>
	</div>
</div>

{#key (nextPlaytest?.status, servers)}
	<Button
		disabled={shouldDisableLaunchButton() || $backgroundSyncInProgress}
		size="xl"
		class="fixed bottom-6 right-6 shadow-2xl"
		on:click={handleSyncAndLaunch}
		>{getMainButtonText()}
	</Button>
{/key}

<ProgressModal bind:showModal={syncing} />
