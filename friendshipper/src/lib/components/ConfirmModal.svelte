<script lang="ts">
	import { Modal, Button } from 'flowbite-svelte';

	export let showModal: boolean;
	export let title: string = 'Are you sure?';
	export let resultCallback: (result: boolean) => void;

	const handleYes = () => {
		showModal = false;
		resultCallback(true);
	};

	const handleNo = () => {
		showModal = false;
		resultCallback(false);
	};
</script>

<Modal
	defaultClass="bg-secondary-700 dark:bg-space-900 overflow-y-auto"
	bodyClass="!border-t-0"
	backdropClass="fixed mt-8 inset-0 z-40 bg-gray-900 bg-opacity-50 dark:bg-opacity-80"
	dialogClass="fixed mt-8 top-0 start-0 end-0 h-modal md:inset-0 md:h-full z-50 w-full p-4 pb-12 flex"
	dismissable={false}
	size="sm"
	outsideclose
	bind:open={showModal}
	bind:resultCallback
>
	<div class="flex items-center gap-2">
		<!-- <Spinner size="4" /> -->
		<p class="text-xl text-primary-400">{title}</p>
	</div>
	<div class="flex flex-row-reverse gap-2">
		<Button class="bg-lime-700 dark:bg-lime-700" on:click={handleNo}>No</Button>
		<Button class="bg-red-700 dark:bg-red-700" on:click={handleYes}>Yes</Button>
	</div>
</Modal>
