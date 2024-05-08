<script lang="ts">
	import { Button, Modal } from 'flowbite-svelte';
	import { emit } from '@tauri-apps/api/event';
	import type { GameServerResult } from '$lib/types';
	import { getServer } from '$lib/gameServers';
	import { builds } from '$lib/stores';

	export let serverName: string;
	export let showModal = false;

	const handleLaunch = async (server: GameServerResult) => {
		const s3Entry = $builds.entries.find((e) => e.commit === server.version);

		if (!s3Entry) {
			return;
		}

		await emit('quick-launch', { s3Entry, server });
		showModal = false;
	};
</script>

<Modal
	size="xs"
	defaultClass="bg-secondary-700 dark:bg-space-900 overflow-y-auto"
	bodyClass="!border-t-0"
	bind:open={showModal}
	autoclose={false}
	dismissable={false}
	outsideclose
>
	{#await getServer(serverName)}
		<p class="text-md text-primary-500">Loading...</p>
	{:then serverInfo}
		<p class="text-md text-primary-500">
			Are you sure you want to join server <span class="font-mono text-gray-200"
				>{serverInfo.displayName}</span
			>?
		</p>
		<div class="w-full flex gap-2 justify-end">
			<Button
				size="sm"
				on:click={async () => {
					await handleLaunch(serverInfo);
				}}
				>Launch
			</Button>
			<Button
				size="sm"
				on:click={() => {
					showModal = false;
				}}
				>Cancel
			</Button>
		</div>
	{:catch error}
		<p class="text-md text-primary-500">
			We couldn't find the server you requested: {error.message}
		</p>
	{/await}
</Modal>
