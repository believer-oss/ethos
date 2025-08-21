<script lang="ts">
	import { onMount } from 'svelte';
	import { emit } from '@tauri-apps/api/event';
	import { oktaAuth, appConfig } from '$lib/stores';
	import { logError, logInfo } from '$lib/utils';
	import { exchangeCodeForTokens } from '$lib/okta';

	onMount(async () => {
		try {
			await logInfo('[CALLBACK] ========== CALLBACK PAGE LOADED ==========');
			await logInfo(`[CALLBACK] Current URL: ${window.location.href}`);
			await logInfo(`[CALLBACK] Search params: ${window.location.search}`);
			await logInfo('[CALLBACK] Processing OAuth callback...');

			if (!$oktaAuth) {
				await logError('[CALLBACK] Okta Auth not available');
				await emit('error', 'Authentication service not available');
				return;
			}

			// Extract authorization code from URL for external browser flow
			const urlParams = new URLSearchParams(window.location.search);
			const authCode = urlParams.get('code');
			// state parameter is not used but available if needed for validation
			// const _stateParam = urlParams.get('state');

			if (authCode) {
				await logInfo('[CALLBACK] Found authorization code, exchanging for tokens...');

				const codeVerifier = sessionStorage.getItem('okta-code-verifier');
				if (!codeVerifier) {
					await logError('[CALLBACK] No code verifier found in session storage');
					await emit('error', 'Authentication state lost - please try logging in again');
					return;
				}

				// Exchange authorization code for tokens using our custom function
				const tokenResponse = await exchangeCodeForTokens(
					$appConfig.oktaConfig.issuer,
					$appConfig.oktaConfig.clientId,
					authCode,
					codeVerifier,
					'friendshipper://auth/callback'
				);

				await logInfo('[CALLBACK] Successfully exchanged code for tokens');

				// Convert to Okta SDK token format and store
				const oktaTokens = {
					accessToken: {
						accessToken: tokenResponse.access_token,
						tokenType: 'Bearer',
						expiresAt: Date.now() / 1000 + (tokenResponse.expires_in || 3600),
						scopes: ['openid', 'email', 'profile', 'offline_access']
					},
					...(tokenResponse.id_token && {
						idToken: {
							idToken: tokenResponse.id_token,
							expiresAt: Date.now() / 1000 + (tokenResponse.expires_in || 3600)
						}
					}),
					...(tokenResponse.refresh_token && {
						refreshToken: {
							refreshToken: tokenResponse.refresh_token
						}
					})
				};

				$oktaAuth.tokenManager.setTokens(oktaTokens);
				await emit('access-token-set', tokenResponse.access_token);

				// Clean up code verifier
				sessionStorage.removeItem('okta-code-verifier');

				await logInfo('[CALLBACK] Redirecting to home page');
			} else {
				// Fallback: try to parse tokens from URL (in case of implicit flow)
				await logInfo('[CALLBACK] No authorization code found, trying to parse tokens from URL...');
				const { tokens } = await $oktaAuth.token.parseFromUrl();

				if (tokens && tokens.accessToken) {
					await logInfo('[CALLBACK] Successfully parsed tokens from URL');
					$oktaAuth.tokenManager.setTokens(tokens);
					const { accessToken } = tokens.accessToken;
					await emit('access-token-set', accessToken);
				} else {
					await logError('[CALLBACK] No tokens found in URL');
					await emit('error', 'No authentication data received');
				}
			}
		} catch (error) {
			await logError('[CALLBACK] Error processing OAuth callback', error);
			await emit(
				'error',
				`Authentication failed: ${error instanceof Error ? error.message : 'Unknown error'}`
			);
		}

		// Always redirect to home after processing
		window.location.href = '/';
	});
</script>

<main>
	<h1>Redirecting...</h1>
</main>
