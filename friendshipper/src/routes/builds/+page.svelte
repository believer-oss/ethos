<script lang="ts">
	import { Button, Card, Spinner, TabItem, Tabs, Tooltip } from 'flowbite-svelte';
	import { onMount } from 'svelte';
	import { RefreshOutline, RocketOutline } from 'flowbite-svelte-icons';
	import { getWorkflows, getActiveBuilds } from '$lib/builds';
	import { logError } from '$lib/utils';
	import type { Nullable, Workflow } from '$lib/types';
	import WorkflowLogsModal from '$lib/components/workflows/WorkflowLogsModal.svelte';
	import PromoteBuildModal from '$lib/components/PromoteBuildModal.svelte';
	import CommitInfoModal from '$lib/components/CommitInfoModal.svelte';
	import ActiveBuildsModal from '$lib/components/ActiveBuildsModal.svelte';
	import { activeBuilds, appConfig, engineWorkflows, workflows } from '$lib/stores';
	import WorkflowTable from '$lib/components/workflows/WorkflowTable.svelte';

	let loading: boolean = false;
	let selectedCommit: string = '';

	let showWorkflowLogsModal: boolean = false;
	let selectedWorkflow: Nullable<Workflow> = null;

	let showPromoteBuildModal: boolean = false;
	let promoteBuildCommit: string = '';

	let showActiveBuildsModal: boolean = false;

	let commitInfoModalOpen = false;
	let commitInfoSha: string | null = null;

	// The rich CommitInfoModal needs a local clone of the game repo to run `git show`.
	// Playtester configs without a clone fall back to the clickable short-SHA link (which
	// already opens the commit on GitHub), so we just hide the info button for those users.
	$: gameCommitInfoHandler = $appConfig.repoPath?.trim()
		? (sha: string) => {
				commitInfoSha = sha;
				commitInfoModalOpen = true;
		  }
		: null;

	// Deliberately not fatal: an unreachable metadata bucket must not blank the builds
	// list, so on failure the arrows simply go away and the table renders as before.
	const refreshActiveBuilds = async () => {
		try {
			$activeBuilds = await getActiveBuilds();
		} catch (e) {
			await logError('Failed to refresh active build promotion status', e);
			$activeBuilds = [];
		}
	};

	const refreshWorkflows = async () => {
		loading = true;
		const res = await getWorkflows();
		$workflows = res.commits;

		if ($appConfig.engineRepoUrl !== '') {
			const engineRes = await getWorkflows(true);
			$engineWorkflows = engineRes.commits;
		}

		// Promotion status tracks the same cadence as the rows it annotates.
		await refreshActiveBuilds();

		loading = false;
	};

	onMount(() => {
		// The workflow list is already populated by the root layout at startup, so the
		// table has rows on arrival — but promotion status is fetched only here. Without
		// this first call the arrows would be missing for up to 30 seconds every time the
		// tab is opened, which reads as "nothing is promoted" rather than "not loaded yet".
		// Only the promotion status is fetched; re-fetching workflows would duplicate what
		// the layout already did.
		void refreshActiveBuilds();

		// refresh every 30 seconds
		const interval = setInterval(() => {
			void refreshWorkflows();
		}, 30000);

		return () => {
			clearInterval(interval);
		};
	});
</script>

<div class="flex items-center gap-2">
	<p class="text-2xl my-2 text-primary-600 dark:text-primary-400">Builds</p>
	<Button disabled={loading} class="!p-1.5" primary on:click={refreshWorkflows}>
		{#if loading}
			<Spinner size="4" />
		{:else}
			<RefreshOutline class="w-4 h-4" />
		{/if}
	</Button>
	<Button
		class="ml-auto"
		primary
		on:click={() => {
			showActiveBuildsModal = true;
		}}
	>
		<RocketOutline class="w-4 h-4 mr-2" />
		Active Builds
	</Button>
</div>
<Card
	class="w-full p-0 sm:p-0 px-2 sm:px-2 max-w-full bg-secondary-700 dark:bg-space-900 h-full overflow-y-hidden border-0 shadow-none flex flex-col gap-0 overflow-auto"
>
	<Tabs style="underline" contentClass="bg-secondary-700 dark:bg-space-900 mt-2">
		<TabItem open title="Game">
			<WorkflowTable
				commits={$workflows}
				bind:showWorkflowLogsModal
				bind:selectedWorkflow
				bind:selectedCommit
				bind:showPromoteBuildModal
				bind:promoteBuildCommit
				onShowCommitInfo={gameCommitInfoHandler}
				showPromotionStatus
			/>
		</TabItem>
		<TabItem
			id="engine-tab"
			title="Engine"
			disabled={$appConfig.engineRepoUrl === '' || $engineWorkflows.length === 0}
		>
			<WorkflowTable
				commits={$engineWorkflows}
				bind:showWorkflowLogsModal
				bind:selectedWorkflow
				bind:selectedCommit
			/>
		</TabItem>
	</Tabs>
</Card>
{#if $appConfig.engineRepoUrl === ''}
	<Tooltip
		triggeredBy="#engine-tab"
		class="w-auto text-xs  text-primary-400 bg-secondary-800 dark:bg-space-950"
		placement="right"
		>Set Engine Repo URL in preferences to see Engine builds!
	</Tooltip>
{/if}

{#if selectedWorkflow}
	<WorkflowLogsModal workflow={selectedWorkflow} bind:showModal={showWorkflowLogsModal} />
{/if}

<PromoteBuildModal bind:showModal={showPromoteBuildModal} bind:commit={promoteBuildCommit} />

<CommitInfoModal bind:open={commitInfoModalOpen} sha={commitInfoSha} />

<ActiveBuildsModal bind:showModal={showActiveBuildsModal} />
