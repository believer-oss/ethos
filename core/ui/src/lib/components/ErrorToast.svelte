<script lang="ts">
	import { fly } from 'svelte/transition';
	import { ExclamationCircleSolid } from 'flowbite-svelte-icons';
	import { Toast } from 'flowbite-svelte';

	export let show = false;
	export let errorMessage: string;
	export let onClose: () => void = () => {};

	$: if (show) {
		setTimeout(() => {
			show = false;
		}, 3000);
	}
</script>

<div class="w-full flex justify-center fixed top-0">
	<Toast
		transition={fly}
		params={{ y: -200 }}
		class="my-4 p-2 left-1/2 text-white dark:text-white bg-red-700 dark:bg-red-700"
		defaultIconClass="bg-red-700 dark:bg-red-700 text-white dark:text-white"
		on:close={onClose}
		bind:open={show}
	>
		<ExclamationCircleSolid slot="icon" class="w-5 h-5 text-white" />
		<span class="w-full">{errorMessage}</span>
	</Toast>
</div>
