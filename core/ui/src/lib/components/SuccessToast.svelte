<script lang="ts">
	import { fly } from 'svelte/transition';
	import { CheckCircleSolid } from 'flowbite-svelte-icons';
	import { Toast } from 'flowbite-svelte';

	export let show = false;
	export let message: string;
	export let onClose: () => void = () => {};
	export let fixed = true;

	$: if (show) {
		setTimeout(() => {
			show = false;
			onClose();
		}, 5000);
	}
</script>

<div class="w-full flex justify-center pointer-events-none" class:fixed class:top-0={fixed}>
	<Toast
		transition={fly}
		params={{ y: fixed ? -200 : -50, duration: fixed ? 400 : 200 }}
		class="my-4 p-2 left-1/2 w-auto max-w-[90vw] text-neutral-800 dark:text-neutral-800 bg-lime-700 dark:bg-lime-700"
		contentClass="w-full text-sm font-normal break-words"
		defaultIconClass="bg-lime-700 dark:bg-lime-700 text-neutral-800 dark:text-neutral-800"
		dismissable={false}
		bind:open={show}
	>
		<CheckCircleSolid slot="icon" class="w-5 h-5 text-white" />
		{message}
	</Toast>
</div>
