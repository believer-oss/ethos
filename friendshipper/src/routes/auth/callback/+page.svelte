<script lang="ts">
	import { onMount } from 'svelte';
	import { emit } from '@tauri-apps/api/event';
	import { oktaAuth } from '$lib/stores';

	onMount(async () => {
		// Parse the tokens from the URL
		if (!$oktaAuth) {
			return;
		}

		const { tokens } = await $oktaAuth.token.parseFromUrl();

		if (tokens && tokens.accessToken) {
			$oktaAuth.tokenManager.setTokens(tokens);

			const { accessToken } = tokens.accessToken;

			// Emit token for in-memory variable update (Okta handles storage)
			await emit('access-token-set', accessToken);
		} else {
			await emit('error', 'No tokens found.');
		}

		window.location.href = '/';
	});
</script>

<main>
	<h1>Redirecting...</h1>
</main>
