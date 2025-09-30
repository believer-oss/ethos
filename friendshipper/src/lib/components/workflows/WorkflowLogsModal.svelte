<script lang="ts">
	import { Button, Hr, Input, Modal, Spinner, Tooltip } from 'flowbite-svelte';
	import { emit, listen } from '@tauri-apps/api/event';
	import { FileCopySolid, LinkOutline } from 'flowbite-svelte-icons';
	import type { JunitOutput, LogChunk, Nullable, Workflow, WorkflowNode } from '$lib/types';
	import {
		getWorkflowJunitArtifact,
		getWorkflowNodeLogs,
		getWorkflowNodes,
		startWorkflowLogTail,
		stopWorkflowLogTail
	} from '$lib/builds';
	import JunitDisplay from './junit/JunitDisplay.svelte';

	export let showModal: boolean;
	export let workflow: Workflow;
	let loading: boolean = false;
	let rawLogs: string = '';
	let lines: string[] = [];
	let junitOutput: Nullable<JunitOutput> = null;
	let searchTerm: string = '';
	let selectedNode: string = '';
	let isStreaming: boolean = false;
	let workflowLogUnlisten: (() => void) | null = null;
	let refreshInterval: number | null = null;

	enum DisplayType {
		Logs,
		Junit
	}

	let displayType: DisplayType = DisplayType.Logs;

	$: filteredLogs = lines
		.filter((line) => {
			if (searchTerm === '') {
				return true;
			}
			return line.toLowerCase().includes(searchTerm.toLowerCase());
		})
		.reverse();

	/* eslint-disable @typescript-eslint/no-unused-vars */
	$: filteredNodes = Object.entries(workflow.status?.nodes || {})
		.map(([_k, v]) => v)
		.filter((node) => {
			// Show nodes with main-logs artifacts. This filters out steps that only output variables.
			if (
				node.outputs?.artifacts &&
				node.outputs.artifacts.some(
					(artifact) => artifact.name === 'main-logs' || artifact.name.endsWith('main-logs')
				)
			) {
				return true;
			}

			// Show running nodes with type "Pod" (for live logs)
			if (node.phase === 'Running' && node.type === 'Pod') {
				return true;
			}

			return false;
		})
		.sort((a, b) => {
			// Sort by startedAt first, then by displayName
			if (a.startedAt && b.startedAt) {
				const timeCompare = a.startedAt.localeCompare(b.startedAt);
				if (timeCompare !== 0) return timeCompare;
			}
			if (a.startedAt && !b.startedAt) return -1;
			if (!a.startedAt && b.startedAt) return 1;
			return a.displayName.localeCompare(b.displayName);
		});

	const getNodesWithLogs = (w: Workflow): WorkflowNode[] =>
		Object.entries(w.status?.nodes || {})
			.filter(
				([_k, v]: [string, WorkflowNode]) =>
					v.outputs &&
					v.outputs.artifacts &&
					v.outputs.artifacts.some(
						(artifact) => artifact.name === 'main-logs' || artifact.name.endsWith('main-logs')
					)
			)
			.map(([_k, v]: [string, WorkflowNode]) => v);
	/* eslint-enable @typescript-eslint/no-unused-vars */

	const getLogs = async (nodeId: string) => {
		// Use smart log retrieval with workflow name
		const logs = await getWorkflowNodeLogs(workflow.metadata.name, nodeId);

		if (logs) {
			rawLogs = logs;
			lines = logs.split('\n').reverse();
		}
	};

	const setupWorkflowLogListener = async () => {
		if (workflowLogUnlisten) {
			workflowLogUnlisten();
		}

		workflowLogUnlisten = await listen('workflow-log', (event) => {
			const chunk: LogChunk = JSON.parse(event.payload as string);

			// Add log data if present
			if (chunk.data.trim()) {
				lines = [chunk.data, ...lines];
				rawLogs = lines.join('\n');
			}

			// Handle stream completion or errors
			if (chunk.finished || chunk.error) {
				isStreaming = false;
				if (chunk.error) {
					void emit('error', chunk.error);
				}
			}
		});
	};

	const stopLogStreaming = async () => {
		if (isStreaming) {
			try {
				await stopWorkflowLogTail();
				isStreaming = false;
			} catch (e) {
				await emit('error', e);
			}
		}
	};

	const refreshLogs = async (node: WorkflowNode) => {
		loading = true;
		selectedNode = node.id;

		// Stop any existing streaming
		await stopLogStreaming();

		// Clear existing logs
		lines = [];
		rawLogs = '';

		// if the node has an artifact called junit-xml, get it
		const junitArtifact = node.outputs?.artifacts?.find((artifact) =>
			artifact.name.endsWith('junit-xml')
		);
		if (junitArtifact) {
			try {
				const output = await getWorkflowJunitArtifact(workflow.metadata.uid, selectedNode);
				if (!output) {
					// immediately switch to logs view if this junit output not found
					displayType = DisplayType.Logs;
				}
				junitOutput = output;
			} catch (e) {
				await emit('error', e);
			}
		} else {
			junitOutput = null;
			displayType = DisplayType.Logs;
		}

		// Check if this is a running pod that should be streamed
		const isRunningPod = node.phase === 'Running' && node.type === 'Pod';

		if (isRunningPod && workflow.metadata.name) {
			try {
				// Set up event listener for streaming logs
				await setupWorkflowLogListener();

				// Start streaming
				await startWorkflowLogTail(workflow.metadata.name, selectedNode);
				isStreaming = true;
			} catch (e) {
				await emit('error', e);
				// Fall back to static logs if streaming fails
				try {
					await getLogs(selectedNode);
				} catch (fallbackError) {
					await emit('error', fallbackError);
				}
			}
		} else {
			// Get static logs for non-running pods
			try {
				await getLogs(selectedNode);
			} catch (e) {
				await emit('error', e);
			}
		}

		loading = false;
	};

	const toggleDisplayType = () => {
		displayType = displayType === DisplayType.Logs ? DisplayType.Junit : DisplayType.Logs;
	};

	const getNodeColor = (node: WorkflowNode): string => {
		switch (node.phase) {
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

	const stopNodeRefresh = () => {
		if (refreshInterval !== null) {
			clearInterval(refreshInterval);
			refreshInterval = null;
		}
	};

	const refreshWorkflowNodes = async () => {
		try {
			const updated = await getWorkflowNodes(workflow.metadata.name);
			workflow = updated;

			// Stop refreshing if workflow is no longer running
			if (updated.status?.phase !== 'Running') {
				stopNodeRefresh();
			}
		} catch (e) {
			await emit('error', e);
		}
	};

	const startNodeRefresh = () => {
		// Only refresh if workflow is running
		if (workflow.status?.phase !== 'Running') {
			return;
		}

		// Refresh nodes every 3 seconds
		refreshInterval = window.setInterval(() => {
			void refreshWorkflowNodes();
		}, 3000);
	};

	const onClose = async () => {
		// Stop node refresh polling
		stopNodeRefresh();

		// Stop streaming and clean up listeners
		await stopLogStreaming();
		if (workflowLogUnlisten) {
			workflowLogUnlisten();
			workflowLogUnlisten = null;
		}

		selectedNode = '';
		junitOutput = null;
		lines = [];
		rawLogs = '';
	};

	const onOpen = async () => {
		// Start periodic node refresh
		startNodeRefresh();

		const nodes = getNodesWithLogs(workflow);

		const importantNode = nodes.find(
			(node) => node.phase === 'Failed' || node.phase === 'Running' || node.phase === 'Pending'
		);

		if (importantNode) {
			await refreshLogs(importantNode);
		}
	};

	const copyLogToClipboard = async () => {
		try {
			await navigator.clipboard.writeText(rawLogs);
		} catch (e) {
			await emit('error', e);
		}
	};

	const copyLinkToClipboard = async () => {
		try {
			const sha = workflow.metadata.labels?.['believer.dev/commit'];
			const link: string = `friendshipper://builds/game/${sha}/${workflow.metadata.name}`;
			await navigator.clipboard.writeText(link);
		} catch (e) {
			await emit('error', e);
		}
	};
</script>

<Modal
	size="xl"
	class="bg-secondary-700 dark:bg-space-900 relative flex flex-col mx-auto max-h-[100vh] border-0"
	placement="top-center"
	bodyClass="!border-t-0 overflow-y-hidden h-full"
	backdropClass="fixed mt-8 inset-0 z-40 bg-gray-900 bg-opacity-50 dark:bg-opacity-80"
	dialogClass="fixed mt-8 top-0 start-0 end-0 h-modal md:inset-0 md:h-full z-50 w-full p-4 pb-12 flex"
	bind:open={showModal}
	autoclose={false}
	outsideclose
	on:open={onOpen}
	on:close={onClose}
>
	<div class="flex flex-col space-y-2 overflow-y-hidden h-full justify-between">
		<div class="flex flex-row gap-4 items-center pb-2">
			<h2 class="text-lg font-semibold text-primary-400 pb-0">Logs for {workflow.metadata.name}</h2>
			{#if isStreaming}
				<span class="text-sm text-green-400 animate-pulse">🔴 Live streaming</span>
			{/if}
			<Input
				type="text"
				size="sm"
				placeholder="Search"
				class="w-1/4 tracking-wider"
				bind:value={searchTerm}
			/>
			<Button
				disabled={loading || rawLogs === ''}
				on:click={() => copyLogToClipboard()}
				class="px-3 py-2 focus:outline-none"
			>
				<FileCopySolid />
			</Button>
			<Tooltip
				class="w-auto bg-secondary-600 dark:bg-space-800 font-semibold shadow-2xl"
				placement="bottom"
				>Copy entire log to clipboard
			</Tooltip>
			<Button on:click={() => copyLinkToClipboard()} class="px-3 py-2 focus:outline-none">
				<LinkOutline />
			</Button>
			<Tooltip
				class="w-auto bg-secondary-600 dark:bg-space-800 font-semibold shadow-2xl"
				placement="bottom"
				>Copy link to this build log to clipboard
			</Tooltip>
		</div>
		<div class="flex gap-2 flex-wrap">
			{#each filteredNodes as node}
				<div class="relative">
					{#if node.phase === 'Running' && node.type === 'Pod'}
						<div class="breathing-indicator" />
					{/if}
					<Button
						disabled={loading || selectedNode === node.id}
						class="text-xs px-2 py-1 rounded-md text-white hover:bg-gray-500 dark:hover:bg-gray-500 disabled:opacity-100 {getNodeColor(
							node
						)} {selectedNode === node.id && 'border border-white'}"
						on:click={() => refreshLogs(node)}
					>
						{node.displayName}
						{#if node.outputs?.artifacts && node.outputs.artifacts.length > 0}
							<span class="artifact-indicator">📋</span>
						{:else if node.phase === 'Running' && node.type === 'Pod'}
							<span class="running-indicator">⚡</span>
						{/if}
					</Button>
				</div>
			{/each}
		</div>
		<Hr />
		{#if junitOutput}
			<div class="flex justify-center gap-2 h-full">
				{#if displayType === DisplayType.Logs}
					<Button size="xs" class="py-1" on:click={toggleDisplayType}>Show Test Results</Button>
				{:else if displayType === DisplayType.Junit}
					<Button size="xs" class="py-1" on:click={toggleDisplayType}>Show Logs</Button>
				{/if}
			</div>
		{/if}
		{#if displayType === DisplayType.Logs}
			{#if loading}
				<div class="flex justify-center items-center">
					<Spinner class="h-8 w-8" />
				</div>
			{:else if filteredLogs.length > 0}
				<div
					class="p-2 flex flex-col-reverse bg-secondary-800 dark:bg-space-950 overflow-x-auto overflow-y-auto rounded-md border border-secondary-600"
				>
					<code class="text-sm text-gray-300 dark:text-gray-300">
						{#each filteredLogs as line}
							{line}<br />
						{/each}
					</code>
				</div>
			{:else}
				<code class="text-sm">Select a build phase above!</code>
			{/if}
			<div class="flex justify-end items-end">
				<code class="text-xs align-right">{filteredLogs.length} entries</code>
			</div>
		{:else if displayType === DisplayType.Junit && junitOutput}
			<div class="flex flex-col gap-2 h-full overflow-y-none">
				<JunitDisplay {junitOutput} />
			</div>
		{/if}
	</div>
</Modal>

<style>
	.breathing-indicator {
		position: absolute;
		top: -2px;
		right: -2px;
		width: 8px;
		height: 8px;
		background: #10b981;
		border-radius: 50%;
		animation: breathe 2s ease-in-out infinite;
		z-index: 10;
	}

	@keyframes breathe {
		0%,
		100% {
			opacity: 0.4;
			transform: scale(1);
		}
		50% {
			opacity: 1;
			transform: scale(1.3);
		}
	}

	.artifact-indicator {
		margin-left: 4px;
		font-size: 10px;
	}

	.running-indicator {
		margin-left: 4px;
		font-size: 10px;
		animation: pulse 1.5s ease-in-out infinite;
	}

	@keyframes pulse {
		0%,
		100% {
			opacity: 1;
		}
		50% {
			opacity: 0.5;
		}
	}
</style>
