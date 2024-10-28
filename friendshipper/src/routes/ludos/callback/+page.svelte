<script lang="ts">
	import { onMount } from 'svelte';
	import { emit } from '@tauri-apps/api/event';
	import { oktaAuth } from '$lib/stores';

	onMount(async () => {
		// Parse the tokens from the URL
		if (!$oktaAuth) {
			return;
		}

		const tokens = await $oktaAuth.token.parseFromUrl();
		$oktaAuth.tokenManager.setTokens(tokens?.tokens);

		const accessToken = tokens?.tokens.accessToken?.accessToken || '';
		const refreshToken = tokens?.tokens.refreshToken?.refreshToken || '';

		await emit('access-token-set', accessToken);
		localStorage.setItem('oktaRefreshToken', refreshToken);
		window.location.href = '/';
	});
</script>

<main>
	<h1>Redirecting...</h1>
</main>
