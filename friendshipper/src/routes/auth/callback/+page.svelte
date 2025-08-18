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

			// Store tokens in localStorage to maintain consistency
			localStorage.setItem('oktaAccessToken', accessToken);
			await emit('access-token-set', accessToken);

			if (tokens.refreshToken) {
				const { refreshToken } = tokens.refreshToken;
				localStorage.setItem('oktaRefreshToken', refreshToken);
			}
		} else {
			await emit('error', 'No tokens found.');
		}

		window.location.href = '/';
	});
</script>

<main>
	<h1>Redirecting...</h1>
</main>
