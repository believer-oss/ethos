<script lang="ts">
	import {
		Button,
		Modal,
		Spinner,
		Table,
		TableBody,
		TableBodyCell,
		TableBodyRow,
		TableHead,
		TableHeadCell,
		Tooltip
	} from 'flowbite-svelte';
	import { emit } from '@tauri-apps/api/event';
	import { GithubSolid, RefreshOutline } from 'flowbite-svelte-icons';
	import { getActiveBuilds } from '$lib/builds';
	import { activeBuilds, repoStatus } from '$lib/stores';
	import { commitUrl, openUrl } from '$lib/utils';
	import type { ActiveBuild } from '$lib/types';

	export let showModal: boolean = false;

	let loading = false;
	let rows: ActiveBuild[] = [];
	let loadError = '';
	let wasOpen = false;

	const load = async () => {
		loading = true;
		loadError = '';
		try {
			rows = await getActiveBuilds();
			// Shared so the table's arrows update without waiting for the 30s poll.
			$activeBuilds = rows;
		} catch (e) {
			// Distinct from a per-row error, which arrives inside `rows`.
			loadError = e instanceof Error ? e.message : String(e);
		} finally {
			loading = false;
		}
	};

	// Fetch on the closed->open transition only. Keying off the transition rather than a
	// loaded flag means closing mid-load cannot leave the guard stuck and skip the next
	// open's fetch.
	$: if (showModal !== wasOpen) {
		wasOpen = showModal;
		if (showModal) void load();
	}

	// deployedAt arrives as an ISO 8601 string, not a Date - parse before formatting.
	const formatDeployedAt = (deployedAt: string | undefined): string => {
		if (!deployedAt) return '';
		const parsed = new Date(deployedAt);
		if (Number.isNaN(parsed.getTime())) return deployedAt;
		return parsed.toLocaleString();
	};

	const openOnGithub = async (url: string) => {
		try {
			await openUrl(url);
		} catch (e) {
			await emit('error', e);
		}
	};
</script>

<Modal
	bind:open={showModal}
	size="lg"
	color="none"
	class="bg-secondary-700 dark:bg-space-900"
	bodyClass="!border-t-0 flex-1 overflow-y-auto overscroll-contain"
	backdropClass="fixed inset-0 z-40 bg-gray-900 bg-opacity-50 dark:bg-opacity-80"
	dialogClass="fixed top-0 start-0 end-0 h-modal md:inset-0 md:h-full z-50 w-full p-4 pb-12 flex"
	dismissable
	outsideclose
>
	<svelte:fragment slot="header">
		<div class="flex items-center gap-2 w-full">
			<h3 class="text-primary-400 text-xl">Active Builds (Game)</h3>
			<Button
				outline
				size="xs"
				class="ml-auto !p-1.5"
				disabled={loading}
				on:click={() => {
					void load();
				}}
			>
				{#if loading}
					<Spinner size="4" />
				{:else}
					<RefreshOutline class="w-4 h-4" />
				{/if}
			</Button>
		</div>
	</svelte:fragment>

	{#if loading && rows.length === 0}
		<div class="flex items-center gap-2 py-4">
			<Spinner size="5" />
			<span class="text-gray-300">Loading active builds…</span>
		</div>
	{:else if loadError}
		<p class="text-red-400 p-2">{loadError}</p>
	{:else if rows.length === 0}
		<p class="text-gray-300 p-2">No promotion destinations are configured.</p>
	{:else}
		<div class="max-h-96 overflow-y-auto bg-secondary-800 dark:bg-space-950 rounded-md">
			<Table color="custom" striped={true}>
				<TableHead class="w-full border-b-0 p-2 bg-secondary-900 dark:bg-space-950 text-white">
					<TableHeadCell class="p-2">Destination</TableHeadCell>
					<TableHeadCell class="p-2">Commit</TableHeadCell>
					<TableHeadCell class="p-2">Deployed</TableHeadCell>
					<TableHeadCell class="p-2">Action</TableHeadCell>
				</TableHead>
				<TableBody>
					{#each rows as row, i (`${row.displayName}-${row.steamBranch ?? ''}`)}
						<TableBodyRow
							class="text-left border-b-0 {i % 2 === 0
								? 'bg-secondary-800 dark:bg-space-950'
								: 'bg-secondary-700 dark:bg-space-900'}"
						>
							<TableBodyCell class="p-2">
								<div class="text-gray-200">{row.displayName}</div>
								{#if row.steamBranch}
									<div class="text-xs text-gray-400">{row.steamBranch}</div>
								{/if}
							</TableBodyCell>
							<TableBodyCell class="p-2 font-mono text-xs">
								{#if row.status === 'resolved' && row.sha}
									{@const { sha } = row}
									<code class="text-gray-200">{sha.substring(0, 8)}</code>
									<Tooltip
										class="w-auto bg-secondary-600 dark:bg-space-800 font-semibold shadow-2xl"
										>{sha}</Tooltip
									>
								{:else if row.status === 'tbd'}
									<span class="text-gray-400 font-sans">TBD</span>
									{#if row.error}
										<div class="text-xs text-gray-500 font-sans">{row.error}</div>
									{/if}
								{:else}
									<span class="text-yellow-400 font-sans">{row.error ?? 'Unable to resolve'}</span>
								{/if}
							</TableBodyCell>
							<TableBodyCell class="p-2 text-xs text-gray-300">
								{formatDeployedAt(row.deployedAt)}
							</TableBodyCell>
							<TableBodyCell class="p-2">
								<!-- Computed inline so $repoStatus is a template dependency; a helper
								     closing over the store would not re-evaluate when it arrives. -->
								{@const githubUrl =
									row.status === 'resolved'
										? commitUrl($repoStatus?.repoOwner, $repoStatus?.repoName, row.sha)
										: null}
								{#if githubUrl}
									<Button
										outline
										size="xs"
										class="p-1 border-0 focus-within:ring-0 dark:focus-within:ring-0"
										on:click={() => openOnGithub(githubUrl)}
									>
										<GithubSolid class="w-4 h-4" />
									</Button>
									<Tooltip
										class="w-auto bg-secondary-600 dark:bg-space-800 font-semibold shadow-2xl"
										>Open on GitHub</Tooltip
									>
								{/if}
							</TableBodyCell>
						</TableBodyRow>
					{/each}
				</TableBody>
			</Table>
		</div>
	{/if}
</Modal>
