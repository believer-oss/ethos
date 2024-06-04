<script lang="ts">
	import { Button, Hr, Input, Modal, Spinner } from 'flowbite-svelte';
	import { emit } from '@tauri-apps/api/event';
	import type { Workflow, WorkflowNode } from '$lib/types';
	import { getWorkflowNodeLogs } from '$lib/builds';

	export let showModal: boolean;
	export let workflow: Workflow;
	let loading: boolean = false;
	let lines: string[] = [];
	let searchTerm: string = '';
	let selectedNode: string = '';

	$: filteredLogs = lines
		.filter((line) => {
			if (searchTerm === '') {
				return true;
			}
			return line.toLowerCase().indexOf(searchTerm.toLowerCase()) !== -1;
		})
		.reverse();

	/* eslint-disable @typescript-eslint/no-unused-vars */
	$: filteredNodes = Object.entries(workflow.status?.nodes)
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
			lines = logs.split('\n').reverse();
		}
	};

	const refreshLogs = async (nodeId: string) => {
		loading = true;
		selectedNode = nodeId;

		try {
			await getLogs(workflow.metadata.uid, selectedNode);
		} catch (e) {
			await emit('error', e);
		}

		loading = false;
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
		lines = [];
	};

	const onOpen = async () => {
		const nodes = getNodesWithLogs(workflow);

		const importantNode = nodes.find(
			(node) => node.phase === 'Failed' || node.phase === 'Running' || node.phase === 'Pending'
		);

		if (importantNode) {
			await refreshLogs(importantNode.id);
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
		</div>
		<div class="flex gap-2">
			{#each filteredNodes as node}
				<Button
					disabled={loading || selectedNode === node.id}
					class="text-xs px-2 py-1 rounded-md text-white hover:bg-gray-500 dark:hover:bg-gray-500 disabled:opacity-100 {getNodeColor(
						node
					)} {selectedNode === node.id && 'border border-white'}"
					on:click={() => refreshLogs(node.id)}
				>
					{node.displayName}
				</Button>
			{/each}
		</div>
		<Hr />
		{#if loading}
			<div class="flex justify-center items-center">
				<Spinner class="h-8 w-8" />
			</div>
		{:else if filteredLogs.length > 0}
			<div
				class="p-2 flex flex-col-reverse bg-secondary-800 dark:bg-space-950 overflow-x-auto overflow-y-auto rounded-md border border-secondary-600"
			>
				<code class="text-sm">
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
	</div>
</Modal>
