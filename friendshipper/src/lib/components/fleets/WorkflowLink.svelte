<script lang="ts">
	// Opens a promotion's Argo workflow the way the builds view does: in WorkflowLogsModal.
	import { emit } from '@tauri-apps/api/event';
	import { getWorkflowNodes } from '$lib/builds';
	import type { Nullable, Workflow } from '$lib/types';
	import WorkflowLogsModal from '$lib/components/workflows/WorkflowLogsModal.svelte';

	export let workflow: Nullable<string>;
	export let label: string = 'Workflow';

	let selected: Nullable<Workflow> = null;
	let showModal = false;
	let loading = false;

	const open = async () => {
		if (!workflow || loading) return;
		loading = true;
		try {
			selected = await getWorkflowNodes(workflow);
			showModal = true;
		} catch (e) {
			await emit('error', e);
		} finally {
			loading = false;
		}
	};
</script>

{#if workflow}
	<button
		class="text-primary-400 hover:underline disabled:opacity-50"
		disabled={loading}
		title={workflow}
		on:click|stopPropagation={open}>{label}</button
	>
	{#if selected}
		<WorkflowLogsModal workflow={selected} bind:showModal />
	{/if}
{/if}
