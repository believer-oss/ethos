<script lang="ts">
	import { Button, Card, Spinner, TabItem, Tabs, Tooltip } from 'flowbite-svelte';
	import { onMount } from 'svelte';
	import { RefreshOutline } from 'flowbite-svelte-icons';
	import { getWorkflows } from '$lib/builds';
	import type { Nullable, Workflow } from '$lib/types';
	import WorkflowLogsModal from '$lib/components/workflows/WorkflowLogsModal.svelte';
	import PromoteBuildModal from '$lib/components/PromoteBuildModal.svelte';
	import { appConfig, engineWorkflows, workflows } from '$lib/stores';
	import WorkflowTable from '$lib/components/workflows/WorkflowTable.svelte';

	let loading: boolean = false;
	let selectedCommit: string = '';

	let showWorkflowLogsModal: boolean = false;
	let selectedWorkflow: Nullable<Workflow> = null;

	let showPromoteBuildModal: boolean = false;
	let promoteBuildCommit: string = '';

	const refreshWorkflows = async () => {
		loading = true;
		const res = await getWorkflows();
		$workflows = res.commits;

		if ($appConfig.engineRepoUrl !== '') {
			const engineRes = await getWorkflows(true);
			$engineWorkflows = engineRes.commits;
		}

		loading = false;
	};

	onMount(() => {
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

<PromoteBuildModal bind:showModal={showPromoteBuildModal} commit={promoteBuildCommit} />
