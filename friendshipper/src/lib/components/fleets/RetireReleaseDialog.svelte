<script lang="ts">
	// Confirms retiring a release, saying what it does: whether channels still serve it, and whether
	// matches would be ended. Each button names its action. `endMatches` is for a release already
	// retiring that waits on matches: only the option to end them.
	import { Button, Modal } from 'flowbite-svelte';
	import { retireDialog } from '$lib/fleetReleases';
	import type { FleetRelease } from '$lib/types';

	export let open: boolean;
	export let release: FleetRelease;
	export let endMatches = false;
	export let onRetire: (force: boolean) => void;

	$: dialog = retireDialog(release, endMatches);

	const choose = (force: boolean) => {
		open = false;
		onRetire(force);
	};
</script>

<!-- The body scrolls, not the frame: flowbite's Modal lets the mouse wheel through only there. -->
<Modal
	defaultClass="bg-secondary-700 dark:bg-space-900 relative flex flex-col mx-auto max-h-full"
	bodyClass="!border-t-0 min-h-0"
	backdropClass="fixed mt-8 inset-0 z-40 bg-gray-900 bg-opacity-50 dark:bg-opacity-80"
	dialogClass="fixed mt-8 top-0 start-0 end-0 h-modal md:inset-0 md:h-full z-50 w-full p-4 pb-12 flex"
	dismissable={false}
	size="md"
	outsideclose
	bind:open
>
	<div class="flex flex-col gap-3 text-sm text-gray-300">
		<p class="text-xl text-primary-400">{dialog.title}</p>
		{#if dialog.bad}
			<p class="rounded border border-red-800 bg-red-900/30 text-red-300 p-2">{dialog.bad}</p>
		{/if}
		{#if dialog.warn}
			<p class="rounded border border-yellow-800 bg-yellow-900/20 text-yellow-300 p-2">
				{dialog.warn}
			</p>
		{/if}
		{#if dialog.body}<p>{dialog.body}</p>{/if}
		{#if dialog.list.length}
			<ul class="list-disc pl-5 flex flex-col gap-1">
				{#each dialog.list as item, i}
					<li>{item}{i === dialog.list.length - 1 ? '.' : ';'}</li>
				{/each}
			</ul>
		{/if}
		{#if dialog.footnote}<p class="text-xs text-gray-400">{dialog.footnote}</p>{/if}
		<div class="flex justify-end gap-2 pt-1">
			<Button
				size="sm"
				color="alternative"
				on:click={() => {
					open = false;
				}}>Keep it</Button
			>
			{#each dialog.buttons as button (button.label)}
				<Button
					size="sm"
					color={button.danger ? 'red' : 'alternative'}
					on:click={() => {
						choose(button.force);
					}}>{button.label}</Button
				>
			{/each}
		</div>
	</div>
</Modal>
