<script lang="ts">
	// A build's short SHA that opens it on the Builds page (/builds?commit=<sha>), which selects and
	// scrolls to it. The full SHA is in the tooltip; right-click copies it.
	import { emit } from '@tauri-apps/api/event';
	import { goto } from '$app/navigation';
	import { shortSha } from '$lib/fleetFormat';

	export let sha: string;
	// Extra text in the tooltip, e.g. the release name.
	export let title = '';

	const copy = async () => {
		try {
			await navigator.clipboard.writeText(sha);
			await emit('success', `Copied ${sha}`);
		} catch (e) {
			await emit('error', e);
		}
	};
</script>

<button
	class="font-mono text-inherit hover:underline hover:text-primary-400"
	title={`${
		title ? `${title}\n` : ''
	}${sha}\nClick to show it on the Builds page; right-click to copy.`}
	on:click|stopPropagation={() => goto(`/builds?commit=${encodeURIComponent(sha)}`)}
	on:contextmenu|preventDefault|stopPropagation={copy}>{shortSha(sha)}</button
>
