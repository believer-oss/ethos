<script lang="ts">
	import { open } from '@tauri-apps/api/shell';
	import { Button, ButtonGroup, Card, Hr, Tooltip } from 'flowbite-svelte';
	import {
		DiscordSolid,
		EditOutline,
		FileCopyOutline,
		LinkOutline,
		UserAddOutline,
		UserRemoveOutline
	} from 'flowbite-svelte-icons';
	import { emit } from '@tauri-apps/api/event';
	import type { Group, GroupStatus, Playtest } from '$lib/types';
	import Countdown from '$lib/components/playtests/Countdown.svelte';
	import { assignUserToGroup, getPlaytests, unassignUserFromPlaytest } from '$lib/playtests';
	import { appConfig, dynamicConfig, playtests } from '$lib/stores';
	import { openUrl } from '$lib/utils';

	export let playtest: Playtest;
	export let handleEditPlaytest: ((playtest: Playtest | null) => void) | null = null;
	export let compact = false;

	export let loading: boolean;

	let countdownFinished = false;

	// if the start time changes, reset the countdown
	$: playtest.spec.startTime, (countdownFinished = false);

	const handleCountdownFinished = () => {
		countdownFinished = true;
	};

	const handleAssign = async (item: Playtest, group: Group, user: string) => {
		loading = true;

		try {
			await assignUserToGroup({ playtest: item.metadata.name, group: group.name, user });
		} catch (e) {
			await emit('error', e);
		}

		playtests.set(await getPlaytests());
		loading = false;
	};

	const handleRandomAssign = async (item: Playtest, user: string) => {
		loading = true;

		try {
			await assignUserToGroup({ playtest: item.metadata.name, user });
		} catch (e) {
			await emit('error', e);
		}

		playtests.set(await getPlaytests());
		loading = false;
	};

	const handleUnassign = async (item: Playtest, user: string) => {
		loading = true;
		await unassignUserFromPlaytest(item.metadata.name, user);

		playtests.set(await getPlaytests());
		loading = false;
	};

	// If the group has the current user in it, color the text lime green
	const getUserClass = (user: string) =>
		`col-span-1 row-span-1 px-2 text-sm text-left ${
			user === $appConfig.userDisplayName ? 'text-lime-500' : 'text-white'
		}`;

	// If the group has the current user in it, give the group a colorful border
	const groupContainsUser = (group: Group) => group.users?.includes($appConfig.userDisplayName);
	const getGroupClass = (group: Group) =>
		`col-span-1 row-span-1 sm:p-2 bg-secondary-700 dark:bg-space-900 flex flex-col justify-between ${
			groupContainsUser(group)
				? 'border-primary-500 dark:border-primary-500'
				: 'border-gray-300 dark:border-gray-300'
		}`;

	const getSortedGroups = (item: Playtest): GroupStatus[] =>
		item.status?.groups?.sort((a, b) => {
			const x = parseInt(a.name.replace('Group ', ''), 10);
			const y = parseInt(b.name.replace('Group ', ''), 10);
			if (x > y) return 1;
			if (x < y) return -1;
			return 0;
		}) ?? [];

	const getPlaytestStartString = (item: Playtest): string => {
		const date = new Date(item.spec.startTime);

		return `${date.toLocaleDateString()} ${date.toLocaleTimeString()}`;
	};

	// Targeting a couple characters commonly used in Playtest names but not allowed in CSS selectors.
	// This could become a more general purpose helper in the future, but for now it's just for this component.
	const getPlaytestQuerySelector = (pt: Playtest): string =>
		pt.metadata.name.replace(/[.|/]/g, '-');
</script>

<Card
	class="w-full p-4 sm:p-4 max-w-full bg-secondary-700 dark:bg-space-900 border-0 shadow-none h-full"
>
	<div class="flex items-center justify-between gap-2 mb-1">
		<div class="flex items-center gap-2">
			<h5 class="text-2xl font-light tracking-tight text-primary-400">
				{playtest.spec.displayName}
			</h5>
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
				<Button
					color="primary"
					size="xs"
					class="text-xs py-1 pl-1 text-center"
					on:click={() => handleRandomAssign(playtest, $appConfig.userDisplayName)}
				>
					🎲 Join random group
				</Button>
			</ButtonGroup>
		</div>

		<!-- If the playtest's start time changes, reset the countdown -->
		{#key playtest.spec.startTime}
			{#if countdownFinished}
				<span class="text-lime-500">Playtest in progress!</span>
			{:else}
				<Countdown
					from={playtest.spec.startTime}
					onFinished={handleCountdownFinished}
					let:remaining
				>
					<span id={`countdown-${getPlaytestQuerySelector(playtest)}`}
						>Playtest starts in {remaining.string}</span
					>
				</Countdown>
			{/if}
		{/key}
	</div>
	<div class="flex items-center justify-between gap-2 mb-4">
		<div class="flex items-center gap-2">
			<span class="text-center text-sm font-bold"
				>version: <span class="text-primary-400 font-normal"
					><code>{playtest.spec.version.substring(0, 8)}</code></span
				>
			</span>
			<span class="text-center font-bold text-sm"
				>map: <span class="text-primary-400 font-normal">{playtest.spec.map}</span>
			</span>
			{#if !compact}
				<span class="text-center font-bold text-sm"
					>group size: <span class="text-primary-400 font-normal"
						>{playtest.spec.playersPerGroup}</span
					>
				</span>
			{/if}
		</div>
		{#if playtest.spec.feedbackURL !== '' && !compact}
			<div class="flex gap-1">
				<Button
					outline
					size="sm"
					class="p-2 py-0 flex gap-1 border-none text-white dark:text-white hover:bg-blue-500 dark:hover:bg-blue-500 border-r-2"
					on:click={() => open(playtest.spec.feedbackURL)}
				>
					<LinkOutline class="w-3 h-3" />
					Submit feedback
				</Button>
				<Button
					color="blue"
					class="!p-1.5 bg-blue-500 dark:bg-blue-500 hover:bg-blue-600 dark:hover:bg-blue-600"
					on:click={() => navigator.clipboard.writeText(playtest.spec.feedbackURL)}
				>
					<FileCopyOutline class="w-4 h-4" />
				</Button>
				<Tooltip
					class="w-auto text-xs text-primary-400 bg-secondary-600 dark:bg-space-800"
					placement="bottom"
					>Copy feedback URL
				</Tooltip>
			</div>
		{/if}
	</div>
	<div
		class="grid xl:grid-cols-2 gap-4 max-h-[12rem] overflow-y-auto pb-2 pr-2 w-full"
		class:grid-cols-2={compact}
		class:xl:grid-cols-4={compact}
		class:grid-cols-4={!compact}
		class:xl:grid-cols-2={!compact}
	>
		{#if playtest.status != null}
			{#each getSortedGroups(playtest) as group, index}
				<Card class={getGroupClass(group)}>
					<div>
						<div class="flex items-center justify-between gap-2">
							<p class="text-base text-primary-400 font-semibold m-2 my-0">{group.name}</p>
							<span class="text-sm">server {group.serverRef ? '🟢' : '🔴'}</span>
						</div>
						<Hr classHr="my-2 bg-gray-300 dark:bg-gray-300" />
						<div class="grid grid-cols-2 gap-1 mb-2">
							{#if group.users != null}
								{#each group.users as user}
									<div class={getUserClass(user)}>{user}</div>
								{:else}
									<div class={getUserClass('')}>No users</div>
								{/each}
							{:else}
								<div class={getUserClass('')}>No users</div>
							{/if}
						</div>
					</div>
					<ButtonGroup>
						{#if groupContainsUser(group)}
							<Button
								outline
								class="!p-2 w-full border-gray-300 dark:border-gray-300 hover:bg-primary-600 dark:hover:bg-primary-600"
								on:click={() => handleUnassign(playtest, $appConfig.userDisplayName)}
							>
								<UserRemoveOutline class="text-white w-5 h-5" />
							</Button>
						{:else}
							<Button
								outline
								disabled={group.users && group.users.length >= playtest.spec.playersPerGroup}
								class="!p-2 w-full border-gray-300 dark:border-gray-300 hover:bg-primary-600 dark:hover:bg-primary-600"
								on:click={() => handleAssign(playtest, group, $appConfig.userDisplayName)}
							>
								<UserAddOutline class="text-white w-5 h-5" />
							</Button>
							{#if group.users && group.users.length >= playtest.spec.playersPerGroup}
								<Tooltip
									class="w-auto text-xs text-primary-400 bg-secondary-600 dark:bg-space-900"
									placement="top"
									>Group is full
								</Tooltip>
							{/if}
						{/if}
						<Button
							disabled={$dynamicConfig.playtestDiscordChannels != null &&
								$dynamicConfig.playtestDiscordChannels.length <= index}
							outline
							class="!p-2 w-full border-gray-300 dark:border-gray-300 hover:bg-primary-600 dark:hover:bg-primary-600"
							on:click={() => openUrl($dynamicConfig.playtestDiscordChannels[index].url)}
						>
							<DiscordSolid class="text-white w-5 h-5" />
						</Button>
						{#if $dynamicConfig.playtestDiscordChannels != null && $dynamicConfig.playtestDiscordChannels.length > index}
							<Tooltip
								class="w-auto text-xs text-primary-400 bg-secondary-600 dark:bg-space-800"
								placement="top">{$dynamicConfig.playtestDiscordChannels[index].name}</Tooltip
							>
						{/if}
					</ButtonGroup>
				</Card>
			{:else}
				<Card
					class="w-full h-full p-0 sm:p-0 max-w-full bg-secondary-700 dark:bg-space-900 border-0 shadow-none"
				>
					<div class="flex gap-2 items-center">
						<p class="text-white">You haven't joined this playtest.</p>
						<Button size="xs" href="/playtests"
							>Playtests<LinkOutline class="ml-2 h-4 w-4" /></Button
						>
					</div>
				</Card>
			{/each}
		{/if}
	</div>
</Card>
<Tooltip
	triggeredBy={`#countdown-${getPlaytestQuerySelector(playtest)}`}
	class="w-auto text-xs text-primary-400 bg-secondary-600 dark:bg-space-800"
	placement="top"
>
	<div class="space-y-2 text-primary-400">
		<span>{getPlaytestStartString(playtest)}</span>
	</div>
</Tooltip>
