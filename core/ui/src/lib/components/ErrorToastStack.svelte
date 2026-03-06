<script lang="ts">
	import { fly } from 'svelte/transition';
	import { ExclamationCircleSolid, CloseOutline } from 'flowbite-svelte-icons';

	export interface ErrorItem {
		id: number;
		message: string;
	}

	export let errors: ErrorItem[] = [];
	export let onDismiss: (id: number) => void = () => {};
	export let onDismissAll: () => void = () => {};

	const MAX_VISIBLE = 5;

	$: visibleErrors = errors.slice(0, MAX_VISIBLE);
	$: overflowCount = Math.max(0, errors.length - MAX_VISIBLE);
</script>

{#if errors.length > 0}
	<div class="w-full flex flex-col items-center pointer-events-none gap-2">
		{#each visibleErrors as error (error.id)}
			<div
				class="pointer-events-auto flex items-center gap-3 w-full max-w-xs p-4 rounded-lg shadow text-white bg-red-700"
				role="alert"
				transition:fly={{ y: -50, duration: 200 }}
			>
				<div
					class="inline-flex items-center justify-center shrink-0 w-8 h-8 rounded-lg bg-red-700 text-white"
				>
					<ExclamationCircleSolid class="w-5 h-5" />
				</div>
				<div class="text-sm font-normal flex-1">{error.message}</div>
				<button
					type="button"
					class="ml-auto -mx-1.5 -my-1.5 rounded-lg p-1.5 inline-flex items-center justify-center h-8 w-8 text-white hover:text-gray-200 hover:bg-red-800 focus:ring-2 focus:ring-red-300"
					aria-label="Close"
					on:click={() => {
						onDismiss(error.id);
					}}
				>
					<CloseOutline class="w-3 h-3" />
				</button>
			</div>
		{/each}
		{#if overflowCount > 0}
			<div class="pointer-events-auto text-sm text-red-300 bg-red-900 rounded px-3 py-1">
				+{overflowCount} more error{overflowCount > 1 ? 's' : ''}
			</div>
		{/if}
		{#if errors.length >= 2}
			<button
				class="pointer-events-auto text-sm text-white bg-red-800 hover:bg-red-900 rounded px-3 py-1 mt-1 flex items-center gap-1"
				on:click={onDismissAll}
			>
				<CloseOutline class="w-3 h-3" />
				Dismiss All
			</button>
		{/if}
	</div>
{/if}
