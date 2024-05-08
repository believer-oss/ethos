<script lang="ts">
	import { fly } from 'svelte/transition';
	import { CheckCircleSolid } from 'flowbite-svelte-icons';
	import { Toast } from 'flowbite-svelte';

	export let show = false;
	export let message: string;
	export let onClose: () => void = () => {};

	$: if (show) {
		setTimeout(() => {
			show = false;
		}, 3000);
	}
</script>

<div class="w-full flex justify-center fixed top-0 pointer-events-none">
	<Toast
		transition={fly}
		params={{ y: -200 }}
		class="my-4 p-2 left-1/2 dark:text-white text-white bg-lime-700 dark:bg-lime-700"
		defaultIconClass="bg-lime-700 dark:bg-lime-700 dark:text-white text-white"
		on:close={onClose}
		bind:open={show}
	>
		<CheckCircleSolid slot="icon" class="w-5 h-5 text-white" />
		{message}
	</Toast>
</div>
