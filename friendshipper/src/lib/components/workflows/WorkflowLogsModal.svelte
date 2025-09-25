<script lang="ts">
	import { Button, Hr, Input, Modal, Spinner, Tooltip } from 'flowbite-svelte';
	import { emit } from '@tauri-apps/api/event';
	import { FileCopySolid, LinkOutline } from 'flowbite-svelte-icons';
	import type { JunitOutput, Nullable, Workflow, WorkflowNode } from '$lib/types';
	import { getWorkflowJunitArtifact, getWorkflowNodeLogs } from '$lib/builds';
	import JunitDisplay from './junit/JunitDisplay.svelte';

	export let showModal: boolean;
	export let workflow: Workflow;
	let loading: boolean = false;
	let rawLogs: string = '';
	let lines: string[] = [];
	let junitOutput: Nullable<JunitOutput> = null;
	let searchTerm: string = '';
	let selectedNode: string = '';

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
		.filter(([k, v]) => v.outputs && v.outputs.artifacts && v.outputs.artifacts.length > 0)
		.map(([k, v]) => v as WorkflowNode);

	const getNodesWithLogs = (w: Workflow) =>
		Object.entries(w.status?.nodes)
			.filter(([k, v]) => v.outputs && v.outputs.artifacts && v.outputs.artifacts.length > 0)
			.map(([k, v]) => v as WorkflowNode);
	/* eslint-enable @typescript-eslint/no-unused-vars */

	const getLogs = async (uid: string, nodeId: string) => {
		const logs = await getWorkflowNodeLogs(uid, nodeId);

		if (logs) {
			rawLogs = logs;
			lines = logs.split('\n').reverse();
		}
	};

	const refreshLogs = async (node: WorkflowNode) => {
		loading = true;
		selectedNode = node.id;

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

		try {
			await getLogs(workflow.metadata.uid, selectedNode);
		} catch (e) {
			await emit('error', e);
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

	const onClose = () => {
		selectedNode = '';
		junitOutput = null;
		lines = [];
	};

	const onOpen = async () => {
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
		<div class="flex gap-2">
			{#each filteredNodes as node}
				<Button
					disabled={loading || selectedNode === node.id}
					class="text-xs px-2 py-1 rounded-md text-white hover:bg-gray-500 dark:hover:bg-gray-500 disabled:opacity-100 {getNodeColor(
						node
					)} {selectedNode === node.id && 'border border-white'}"
					on:click={() => refreshLogs(node)}
				>
					{node.displayName}
				</Button>
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
