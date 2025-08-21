<script>
	// This page exists only to satisfy SvelteKit routing
	// OAuth callback processing is handled in +layout.svelte
</script>

<div class="flex items-center justify-center h-screen">
	<div class="text-center">
		<p class="text-lg text-gray-300">Processing authentication...</p>
		<div class="mt-4">
			<div class="animate-spin rounded-full h-8 w-8 border-b-2 border-blue-500 mx-auto" />
		</div>
	</div>
</div>
