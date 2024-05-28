<script lang="ts">
	import { Button, Helper, Input, Label, Modal, Select, Toggle } from 'flowbite-svelte';
	import { emit } from '@tauri-apps/api/event';
	import { get } from 'svelte/store';
	import { launchServer } from '$lib/gameServers';
	import type { ArtifactEntry, Nullable } from '$lib/types';
	import { activeProjectConfig, builtCommits, selectedCommit, workflowMap } from '$lib/stores';

	export let showModal: boolean;
	export let handleServerCreate: () => Promise<void> = async () => {};
	export let handleAutoLaunch: (serverName: string) => Promise<void>;
	export let initialEntry: Nullable<ArtifactEntry> = null;

	let busy = false;
	let serverName = '';
	let map = $activeProjectConfig?.maps[0] || '';
	let hasError = false;
	let autoLaunch = false;

	const maps = $activeProjectConfig?.maps.map((m) => ({ name: m, value: m }));

	let selected: Nullable<ArtifactEntry> = get(selectedCommit);
	let recentCommits = get(builtCommits);

	const validateServerName = (name: string): boolean => {
		if (name === '') return true;

		const regexp = /^[a-zA-Z0-9-_]+$/;

		return regexp.test(name);
	};

	const handleValidation = () => {
		hasError = !validateServerName(serverName);
	};

	const handleSubmit = async () => {
		if (selected === null) {
			return;
		}

		busy = true;

		try {
			await launchServer({
				commit: selected.commit,
				displayName: serverName,
				checkForExisting: false,
				map
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

	const handleCommitChange = (newCommit: Nullable<ArtifactEntry>) => {
		if (newCommit === null) {
			return;
		}

		selectedCommit.set(newCommit);
	};

	$: void handleCommitChange(selected);

	$: $builtCommits,
		() => {
			recentCommits = get(builtCommits);
		};
</script>

<Modal
	size="xs"
	defaultClass="bg-secondary-700 dark:bg-space-900 overflow-y-auto overflow-x-hidden"
	bodyClass="!border-t-0"
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
				on:input={() => handleValidation()}
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
		<Toggle class="text-white" bind:checked={autoLaunch} name="launch">
			Sync client and join server immediately
		</Toggle>
		<Button disabled={busy || hasError} type="submit" class="w-full">Submit</Button>
	</form>
</Modal>
