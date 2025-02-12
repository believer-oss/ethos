<script lang="ts">
	import { Button, Helper, Input, Label, Modal, Select, Toggle } from 'flowbite-svelte';
	import { emit } from '@tauri-apps/api/event';
	import { get } from 'svelte/store';
	import { ProgressModal } from '@ethos/core';
	import { getServers, launchServer, getServerArgsDisplayString } from '$lib/gameServers';

	import type {
		ArtifactEntry,
		GameServerResult,
		Nullable,
		SyncClientRequest,
		PlaytestProfile
	} from '$lib/types';
	import {
		activeProjectConfig,
		repoConfig,
		builds,
		builtCommits,
		selectedCommit,
		workflowMap
	} from '$lib/stores';
	import { syncClient } from '$lib/builds';

	export let showModal: boolean;
	export let handleServerCreate: () => Promise<void> = async () => {};
	export let initialEntry: Nullable<ArtifactEntry> = null;

	let busy = false;
	let serverName = '';
	let map = $activeProjectConfig?.maps[0] || '';
	let profile: string = $repoConfig?.playtestProfiles[0].name; // the backend ensures there's always at least one valid entry here
	let hasError = false;
	let autoLaunch = false;

	const maps = $activeProjectConfig?.maps.map((m) => ({ name: m, value: m }));

	const profiles = $repoConfig?.playtestProfiles.map((p) => ({
		name: p.name,
		value: p
	}));

	let selected: Nullable<ArtifactEntry> = get(selectedCommit);
	let recentCommits = get(builtCommits);

	let syncing = false;

	const validateServerName = (name: string): boolean => {
		if (name === '') return true;

		const regexp = /^[a-zA-Z0-9-_]+$/;

		return regexp.test(name);
	};

	const handleValidation = () => {
		hasError = !validateServerName(serverName);
	};

	const handleCommitChange = (newCommit: Nullable<ArtifactEntry>) => {
		if (newCommit === null) {
			return;
		}

		selectedCommit.set(newCommit);
	};

	$: handleCommitChange(selected);

	$: $builtCommits,
		() => {
			recentCommits = get(builtCommits);
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

		syncing = false;
	};

	const handleAutoLaunch = async (name: string) => {
		if (selected === null) {
			return;
		}
		const servers = await getServers();
		const server = servers.find((s) => s.displayName === name);
		if (server) {
			try {
				if (server.ready) {
					await handleSyncClient(selected, server);
				} else {
					await emit('error', 'Server is not ready. Try again shortly.');
				}
				selected = get(selectedCommit);
			} catch (e) {
				await emit('error', e);
			}
		}
	};

	const handleSubmit = async () => {
		if (selected === null) {
			return;
		}

		busy = true;

		try {
			let cmdArgs = [];
			if (profile !== undefined) {
				const selectedProfile: PlaytestProfile = profiles.find((p) => p.name === profile).value;
				cmdArgs = selectedProfile.args.split(' ');
			}

			await launchServer({
				commit: selected.commit,
				displayName: serverName,
				checkForExisting: false,
				map,
				includeReadinessProbe: false,
				cmdArgs
			});
		} catch (e) {
			await emit('error', e);

			return;
		}

		showModal = false;
		busy = false;
		await handleServerCreate();

		if (autoLaunch) {
			await handleAutoLaunch(serverName);
		}

		autoLaunch = false;
	};
</script>

<Modal
	size="xs"
	defaultClass="bg-secondary-700 dark:bg-space-900 overflow-y-auto overflow-x-hidden"
	bodyClass="!border-t-0"
	backdropClass="fixed mt-8 inset-0 z-40 bg-gray-900 bg-opacity-50 dark:bg-opacity-80"
	dialogClass="fixed mt-8 top-0 start-0 end-0 h-modal md:inset-0 md:h-full z-50 w-full p-4 pb-12 flex"
	bind:open={showModal}
	on:open={() => {
		selected = initialEntry !== null ? initialEntry : get(selectedCommit);
	}}
	autoclose={false}
	outsideclose
>
	<form class="flex flex-col space-y-4" action="#" on:submit|preventDefault={handleSubmit}>
		<h4 class="text-lg font-semibold text-primary-400">Launch Server</h4>
		<Label color={hasError ? 'red' : 'gray'} class="space-y-2 text-xs text-white">
			<span>Version</span>
			<Select
				size="sm"
				class="bg-secondary-700 dark:bg-space-900 border-white dark:border-white text-white dark:text-white"
				placeholder="Select a commit"
				bind:value={selected}
				required
			>
				{#if recentCommits}
					{#each recentCommits as commit}
						<option value={commit.value}>
							{commit.name.substring(0, 8)}
							{$workflowMap.get(commit.name)?.message?.substring(0, 55) || ''}
						</option>
					{/each}
				{/if}
			</Select>
		</Label>
		<Label color={hasError ? 'red' : 'gray'} class="space-y-2 text-xs text-white">
			<span>Name</span>
			<Input
				class="text-white bg-secondary-700 dark:bg-space-900"
				type="text"
				size="sm"
				bind:value={serverName}
				on:input={() => {
					handleValidation();
				}}
				placeholder="Server Name"
				color={hasError ? 'red' : 'base'}
				required
			/>
		</Label>
		{#if hasError}
			<Helper class="mt-2" color="red">
				<span class="font-medium">Error!</span>
				Server names can only include A-Z, a-z, 0-9, -, and _.
			</Helper>
		{/if}
		<Label class="space-y-2 text-xs text-white">
			<span>Map</span>
			<Select
				size="sm"
				name="map"
				class="text-white bg-secondary-700 dark:bg-space-900"
				items={maps}
				bind:value={map}
				required
			/>
		</Label>
		{#if profiles !== null && profiles !== undefined && profiles.length > 0}
			<Label class="space-y-2 text-xs text-white">
				<span>Profile</span>
				<Select
					size="sm"
					name="profile"
					class="text-white bg-secondary-700 dark:bg-space-900"
					bind:value={profile}
					required
				>
					{#each profiles as profileItem}
						<option value={profileItem.name}>
							<span>{profileItem.name}</span>
							<span>{getServerArgsDisplayString(profileItem.value.args)}</span>
						</option>
					{/each}
				</Select>
			</Label>
		{/if}
		<Toggle class="text-white" bind:checked={autoLaunch} name="launch">
			Sync client and join server immediately
		</Toggle>
		<Button disabled={busy || hasError} type="submit" class="w-full">Submit</Button>
	</form>
</Modal>

<ProgressModal bind:showModal={syncing} />
