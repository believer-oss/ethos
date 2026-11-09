<script lang="ts">
	import { fly } from 'svelte/transition';
	import { ExclamationCircleSolid, CloseOutline } from 'flowbite-svelte-icons';
	import { Toast } from 'flowbite-svelte';

	export let show = false;
	export let errorMessage: string;
	export let onClose: () => void = () => {};
</script>

<div class="w-full flex justify-center fixed top-0">
	<Toast
		transition={fly}
		params={{ y: -200 }}
		class="my-4 p-2 left-1/2 w-auto max-w-[90vw] text-neutral-200 dark:text-neutral-200 bg-red-700 dark:bg-red-700"
		contentClass="w-full text-sm font-normal break-words"
		defaultIconClass="bg-red-700 dark:bg-red-700 text-neutral-200 dark:text-neutral-200"
		on:close={onClose}
		bind:open={show}
	>
		<ExclamationCircleSolid slot="icon" class="w-5 h-5 text-white" />
		<span class="w-full">{errorMessage}</span>
		<button
			slot="close-button"
			let:close
			type="button"
			class="ms-auto rounded-lg p-1.5 inline-flex items-center justify-center shrink-0 h-10 w-10 text-neutral-200 border border-neutral-200 hover:text-neutral-300 hover:bg-red-800 focus:ring-2 focus:ring-red-300"
			aria-label="Close"
			on:click={close}
		>
			<CloseOutline class="w-5 h-5" />
		</button>
	</Toast>
</div>
