<script lang="ts">
	import { onMount } from 'svelte';
	import { Badge, Button, Card, Indicator, Tooltip } from 'flowbite-svelte';
	import {
		ChevronDownOutline,
		ChevronUpOutline,
		CloseOutline,
		CodeOutline,
		CodeBranchOutline,
		ArrowUpOutline
	} from 'flowbite-svelte-icons';
	import { emit, listen } from '@tauri-apps/api/event';
	import type { CommitWorkflowInfo, Nullable, Workflow } from '$lib/types';
	import { stopWorkflow } from '$lib/builds';
	import { appConfig, repoConfig } from '$lib/stores';

	export let selectedCommit = '';
	export let showWorkflowLogsModal = false;
	export let selectedWorkflow: Nullable<Workflow>;
	export let commits: CommitWorkflowInfo[];
	export let showPromoteBuildModal: boolean = false;
	export let promoteBuildCommit: string = '';

	const setSelectedCommit = (commit: string) => {
		selectedCommit = commit;
	};

	const stop = async (workflow: string) => {
		try {
			await stopWorkflow(workflow);
		} catch (e) {
			await emit('error', e);
		}
	};

	const getCommitPhase = (commit: CommitWorkflowInfo): string => {
		// if any workflow is running, return "Running"
		if (commit.workflows.some((workflow) => workflow.status.phase === 'Running')) return 'Running';

		// if any workflow has failed, return "Failed"
		if (commit.workflows.some((workflow) => workflow.status.phase === 'Failed')) return 'Failed';

		// if any workflow is pending, return "Pending"
		if (commit.workflows.some((workflow) => workflow.status.phase === 'Pending')) return 'Pending';

		// if all workflows have succeeded, return "Succeeded"
		if (commit.workflows.every((workflow) => workflow.status.phase === 'Succeeded'))
			return 'Succeeded';

		return 'unknown';
	};

	const getWorkflowName = (workflow: Workflow): string => {
		const annotations = workflow.metadata.annotations || {};
		const displayName = annotations['believer.dev/display-name'];
		if (displayName) {
			return displayName;
		}

		// take everything up until the final -
		const parts = workflow.metadata.name.split('-');
		parts.pop();
		return parts.join('-');
	};

	const getWorkflowTooltip = (workflow: Workflow): string => {
		const annotations = workflow.metadata.annotations || {};
		const desc = annotations['believer.dev/description'];
		if (desc) {
			return desc;
		}
		return `Build steps for ${getWorkflowName(workflow)}`;
	};

	const getWorkflowProgress = (workflow: Workflow): number => {
		if (!workflow.status.progress) return 0;

		return parseInt(workflow.status.progress?.split('/')[0], 10);
	};

	const getWorkflowProgressColor = (
		workflow: Workflow,
		i: number
	):
		| 'gray'
		| 'green'
		| 'none'
		| 'red'
		| 'yellow'
		| 'indigo'
		| 'purple'
		| 'blue'
		| 'dark'
		| 'orange'
		| 'teal'
		| undefined => {
		if (!workflow.status.progress) return 'gray';

		const progress = getWorkflowProgress(workflow);

		if (progress === i) {
			if (workflow.status.phase === 'Failed') return 'red';
			if (workflow.status.phase === 'Running') return 'blue';
			if (workflow.status.phase === 'Pending') return 'yellow';
			if (workflow.status.phase === 'Succeeded') return 'green';
		}

		return progress > i ? 'green' : 'gray';
	};

	const getWorkflowPhaseCount = (workflow: Workflow): number => {
		if (!workflow.status.progress) return 0;

		return parseInt(workflow.status.progress?.split('/')[1], 10);
	};

	const getBadgeColor = (phase: string): string => {
		switch (phase) {
			case 'Failed':
				return 'bg-red-700 dark:bg-red-700';
			case 'Running':
				return 'bg-blue-600 dark:bg-blue-600';
			case 'Pending':
				return 'bg-primary-500 dark:bg-primary-500';
			case 'Succeeded':
				return 'bg-lime-600 dark:bg-lime-600';
			default:
				return '';
		}
	};

	const formatBranchName = (branch: string | null): string => {
		if (!branch) return '';
		// Remove refs/heads/ prefix if present
		return branch.replace('refs/heads/', '');
	};

	const truncateBranchName = (branch: string | null): string => {
		const formatted = formatBranchName(branch);
		if (formatted.length <= 10) return formatted;
		return `${formatted.substring(0, 10)}...`;
	};

	const isPrimaryBranch = (branch: string | null): boolean => {
		if (!branch) return false;
		const cleanBranchName = formatBranchName(branch).toLowerCase();

		// Get primary branch from config, with fallback to first target branch, then "main"
		let primaryBranch = $appConfig?.primaryBranch;
		if (!primaryBranch && $repoConfig?.targetBranches?.length > 0) {
			primaryBranch = $repoConfig.targetBranches[0].name;
		}
		if (!primaryBranch) {
			primaryBranch = 'main';
		}

		return cleanBranchName === primaryBranch.toLowerCase();
	};

	onMount(() => {
		void listen('build-deep-link', (e) => {
			const workflowInfo = e.payload;

			const foundCommitWorkflow = commits.find((commit) =>
				commit.commit.startsWith(workflowInfo.commitSha)
			);

			if (foundCommitWorkflow !== undefined) {
				const foundWorkflow = foundCommitWorkflow.workflows.find(
					(workflow) =>
						workflow.kind === 'Workflow' && workflow.metadata.name.startsWith(workflowInfo.name)
				);

				if (foundWorkflow !== undefined) {
					setSelectedCommit(foundCommitWorkflow.commit);
					selectedWorkflow = foundWorkflow;
					showWorkflowLogsModal = true;
				}
			}
		});
	});
</script>

{#each commits as commit}
	<div class="flex items-center justify-between gap-0">
		<div class="flex items-center gap-2 w-40 flex-none">
			<a
				class="text-sm text-center text-primary-400 dark:text-primary-400 hover:underline"
				href={commit.compareUrl}
				target="_blank"
				rel="noopener noreferrer"><code>{commit.commit.substring(0, 8)}</code></a
			>
			{#if commit.branch}
				<div class="flex items-center gap-1">
					{#if isPrimaryBranch(commit.branch)}
						<CodeBranchOutline class="w-3 h-3 text-green-400" />
					{:else}
						<CodeBranchOutline class="w-3 h-3 text-blue-400" />
					{/if}
					<span class="text-xs text-gray-300" title={formatBranchName(commit.branch)}>
						{truncateBranchName(commit.branch)}
					</span>
				</div>
			{/if}
		</div>
		<p
			class="text-xs text-left w-80 text-primary-400 dark:text-primary-400 text-ellipsis whitespace-nowrap overflow-hidden flex-auto"
		>
			{commit.message}
		</p>
		<Tooltip class="w-auto text-xs text-primary-400 bg-secondary-600 dark:bg-space-800"
			>{commit.message}</Tooltip
		>
		<p class="text-xs text-center w-40 text-primary-400 dark:text-primary-400">{commit.pusher}</p>
		<p class="text-xs text-center w-60 text-primary-400 dark:text-primary-400">
			{new Date(commit.creationTimestamp).toLocaleString()}
		</p>
		<div class="w-24">
			<Badge
				class="text-white dark:text-white w-full {getBadgeColor(
					getCommitPhase(commit)
				)} {getCommitPhase(commit) === 'Running' ? 'animate-pulse' : ''}"
				>{getCommitPhase(commit)}</Badge
			>
		</div>
		<Button
			outline
			size="xs"
			class="border-0"
			on:click={() => {
				selectedCommit === commit.commit ? setSelectedCommit('') : setSelectedCommit(commit.commit);
			}}
		>
			{#if selectedCommit === commit.commit}
				<ChevronDownOutline class="w-5 h-5 text-white" />
			{:else}
				<ChevronUpOutline class="w-5 h-5 text-white" />
			{/if}
		</Button>
	</div>
	{#if selectedCommit === commit.commit}
		<div
			class="border border-gray-600 dark:border-gray-500 rounded-lg mt-2 bg-secondary-800 dark:bg-space-950"
		>
			<div class="grid grid-cols-3 gap-2 transition">
				{#each commit.workflows as workflow}
					<Card
						class="`col-span-1 row-span-1 sm:p-2 bg-secondary-700 dark:bg-space-900 flex flex-col justify-between border-gray-300 dark:border-gray-300"
					>
						<div class="flex justify-between items-center gap-2 pb-2">
							<span class="text-primary-400">{getWorkflowName(workflow)}</span>
							<Tooltip
								class="w-auto text-xs text-primary-400 bg-secondary-700 dark:bg-space-900"
								placement="bottom"
								>{getWorkflowTooltip(workflow)}
							</Tooltip>
							<div class="flex gap-1 pb-2">
								{#if workflow.status.phase === 'Running' || workflow.status.phase === 'Pending'}
									<Button
										size="xs"
										class="py-1 px-2 text-xs bg-red-800 dark:bg-red-800 hover:bg-red-600 dark:hover:bg-red-600"
										on:click={async () => {
											await stop(workflow.metadata.name);
										}}
									>
										<CloseOutline class="w-4 h-4" />
									</Button>
									<Tooltip
										class="w-auto text-xs text-primary-400 bg-secondary-700 dark:bg-space-900"
										placement="bottom"
										>Stop build
									</Tooltip>
								{/if}
								<Button
									size="xs"
									class="py-1 px-2 text-xs"
									on:click={() => {
										showWorkflowLogsModal = true;
										selectedWorkflow = workflow;
									}}
								>
									<CodeOutline class="w-4 h-4" />
								</Button>
								<Tooltip
									class="w-auto text-xs text-primary-400 bg-secondary-700 dark:bg-space-900"
									placement="bottom"
									>Show logs
								</Tooltip>
							</div>
						</div>
						<hr />
						{#if workflow.status.startedAt}
							<div class="flex items-center justify-between">
								<span class="text-sm text-primary-400 w-18">started at:</span>
								<span class="text-sm w-40 text-white">{workflow.status.startedAt}</span>
							</div>
						{/if}
						{#if workflow.status.finishedAt}
							<div class="flex items-center justify-between">
								<span class="text-sm text-primary-400 w-18">finished at:</span>
								<span class="text-sm w-40 text-white">{workflow.status.finishedAt}</span>
							</div>
						{/if}
						<div class="flex items-center justify-between">
							<span class="text-sm text-primary-400 w-18">steps:</span>
							<div class="flex flex-wrap items-center gap-0.5 w-40">
								<!-- eslint-disable-next-line @typescript-eslint/no-unused-vars -->
								{#each { length: getWorkflowPhaseCount(workflow) } as _, i}
									<Indicator
										color={getWorkflowProgressColor(workflow, i)}
										class={i === getWorkflowProgress(workflow) &&
										workflow.status.phase === 'Running'
											? 'animate-pulse'
											: ''}
									/>
								{/each}
							</div>
						</div>
					</Card>
				{/each}
			</div>
			{#if getCommitPhase(commit) === 'Succeeded' && $$props.showPromoteBuildModal !== undefined}
				<div class="flex justify-center pt-2">
					<Button
						size="sm"
						class="bg-lime-700 dark:bg-lime-700 hover:bg-lime-600 dark:hover:bg-lime-600 text-white"
						on:click={() => {
							promoteBuildCommit = commit.commit;
							showPromoteBuildModal = true;
						}}
					>
						<ArrowUpOutline class="w-4 h-4 me-2" />
						Promote Build
					</Button>
					<Tooltip
						class="w-auto text-xs text-primary-400 bg-secondary-700 dark:bg-space-900"
						placement="bottom"
						>Create a promote build workflow for this successful build
					</Tooltip>
				</div>
			{/if}
		</div>
	{/if}
{/each}
