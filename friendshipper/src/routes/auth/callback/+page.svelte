<script lang="ts">
	import { onMount } from 'svelte';
	import { emit } from '@tauri-apps/api/event';
	import { jwtDecode } from 'jwt-decode';
	import { oktaAuth } from '$lib/stores';
	import { refreshLogin } from '$lib/auth';
	import { logError } from '$lib/utils';

	onMount(async () => {
		// Check if we're running in a browser (not in Tauri webview)
		// eslint-disable-next-line no-underscore-dangle
		const isInBrowser = !window.__TAURI__;

		// OAuth callback page loaded

		if (isInBrowser) {
			// We're in a browser, try to redirect back to the Tauri app with the callback data
			try {
				const urlParams = new URLSearchParams(window.location.search);
				const authCode = urlParams.get('code');
				const state = urlParams.get('state');

				if (authCode && state) {
					// Try to open the Tauri app with the callback URL
					const tauriCallbackUrl = `friendshipper://auth/callback?code=${authCode}&state=${state}`;

					// Try to redirect to the custom scheme
					window.location.href = tauriCallbackUrl;

					// Also try to close this browser tab after a short delay
					setTimeout(() => {
						try {
							window.close();
						} catch (_e) {
							// Could not close browser tab automatically
						}
					}, 1000);

					// Show user-friendly message
					document.body.innerHTML = `
						<div style="font-family: system-ui; padding: 40px; text-align: center;">
							<h2>Login Successful!</h2>
							<p>Redirecting back to Friendshipper...</p>
							<p><small>If the app doesn't open automatically, you can close this tab and return to Friendshipper.</small></p>
						</div>
					`;
					return;
				}
				// No authorization code found in browser callback
				document.body.innerHTML = `
						<div style="font-family: system-ui; padding: 40px; text-align: center;">
							<h2>Login Error</h2>
							<p>No authorization code received. Please try logging in again.</p>
							<p><a href="javascript:window.close()">Close this tab</a></p>
						</div>
					`;
				return;
			} catch (_error) {
				// Browser callback error
				document.body.innerHTML = `
					<div style="font-family: system-ui; padding: 40px; text-align: center;">
						<h2>Login Error</h2>
						<p>An error occurred during login. Please try again.</p>
						<p><a href="javascript:window.close()">Close this tab</a></p>
					</div>
				`;
				return;
			}
		}

		// We're in the Tauri webview - with deeplink flow, callbacks should be handled by the deeplink handler
		// Check if we have OAuth parameters that would indicate this was meant for deeplink processing
		const urlParams = new URLSearchParams(window.location.search);
		const authCode = urlParams.get('code');
		const returnedState = urlParams.get('state');

		if (authCode && returnedState) {
			// This appears to be an OAuth callback, but since we're using deeplinks now,
			// this should be handled by the deeplink handler in the main layout.
			// Skip processing here to avoid state parameter conflicts.
			window.location.href = '/';
			return;
		}

		// Fallback: handle any remaining OAuth processing for legacy flows
		if (!$oktaAuth) {
			await logError('OAuth callback: oktaAuth not available', null);
			return;
		}

		try {
			if (authCode) {
				// Found authorization code, proceeding with PKCE flow

				// Verify state parameter
				const storedState = sessionStorage.getItem('oauth_state');
				if (returnedState !== storedState) {
					await logError('OAuth callback: State parameter mismatch', null);
					await emit('error', 'OAuth state verification failed.');
					window.location.href = '/';
					return;
				}

				// Get stored PKCE verifier
				const codeVerifier = sessionStorage.getItem('oauth_code_verifier');
				if (!codeVerifier) {
					await logError('OAuth callback: No code verifier found', null);
					await emit('error', 'OAuth PKCE verifier missing.');
					window.location.href = '/';
					return;
				}

				// Clean up session storage
				sessionStorage.removeItem('oauth_code_verifier');
				sessionStorage.removeItem('oauth_state');

				// Exchange authorization code for tokens
				const tokenUrl = `${$oktaAuth.options.issuer}/oauth2/v1/token`;
				const tokenResponse = await fetch(tokenUrl, {
					method: 'POST',
					headers: {
						'Content-Type': 'application/x-www-form-urlencoded',
						Accept: 'application/json'
					},
					body: new URLSearchParams({
						grant_type: 'authorization_code',
						client_id: $oktaAuth.options.clientId,
						code: authCode,
						redirect_uri: 'friendshipper://auth/callback',
						code_verifier: codeVerifier
					})
				});

				if (!tokenResponse.ok) {
					const errorText = await tokenResponse.text();
					await logError(
						`OAuth callback: Token exchange failed: ${tokenResponse.status}`,
						errorText
					);
					await emit('error', 'Failed to exchange authorization code for tokens.');
					window.location.href = '/';
					return;
				}

				const tokenData = await tokenResponse.json();
				// Token exchange successful

				// Create tokens in Okta format
				const tokens = {
					accessToken: {
						accessToken: tokenData.access_token,
						claims: jwtDecode(tokenData.access_token),
						expiresAt: Math.floor(Date.now() / 1000) + (tokenData.expires_in || 3600),
						tokenType: 'Bearer',
						scopes: ['openid', 'profile', 'email']
					}
				};

				if (tokenData.id_token) {
					tokens.idToken = {
						idToken: tokenData.id_token,
						claims: jwtDecode(tokenData.id_token),
						expiresAt: Math.floor(Date.now() / 1000) + (tokenData.expires_in || 3600)
					};
				}

				// Store tokens in Okta token manager
				$oktaAuth.tokenManager.setTokens(tokens);

				// Notify backend
				await refreshLogin(tokenData.access_token);

				await emit('access-token-set', tokenData.access_token);

				// Clean up URL and redirect
				window.history.replaceState({}, document.title, window.location.pathname);
				window.location.href = '/';
				return;
			}

			// Check if we have tokens in the URL fragment (implicit flow - fallback)
			if (
				window.location.hash &&
				(window.location.hash.includes('access_token') || window.location.hash.includes('id_token'))
			) {
				// Found tokens in URL fragment (implicit flow)

				// Parse tokens from URL fragment
				const fragment = window.location.hash.substring(1); // Remove the '#'
				const params = new URLSearchParams(fragment);

				const accessToken = params.get('access_token');
				const idToken = params.get('id_token');
				const expiresIn = params.get('expires_in');

				if (accessToken) {
					// Successfully extracted access token from fragment

					// Create tokens in Okta format
					const tokens = {
						accessToken: {
							accessToken,
							claims: jwtDecode(accessToken),
							expiresAt:
								Math.floor(Date.now() / 1000) + (expiresIn ? parseInt(expiresIn, 10) : 3600),
							tokenType: 'Bearer',
							scopes: ['openid', 'profile', 'email']
						}
					};

					if (idToken) {
						tokens.idToken = {
							idToken,
							claims: jwtDecode(idToken),
							expiresAt:
								Math.floor(Date.now() / 1000) + (expiresIn ? parseInt(expiresIn, 10) : 3600)
						};
					}

					// Store tokens in Okta token manager
					$oktaAuth.tokenManager.setTokens(tokens);

					// Notify backend
					await refreshLogin(accessToken);

					await emit('access-token-set', accessToken);

					// Clean up URL
					window.history.replaceState({}, document.title, window.location.pathname);
					window.location.href = '/';
					return;
				}
			}

			// Fallback: Use Okta's built-in URL parsing
			const { tokens } = await $oktaAuth.token.parseFromUrl();

			if (tokens && tokens.accessToken) {
				// Tokens parsed successfully via parseFromUrl
				$oktaAuth.tokenManager.setTokens(tokens);

				const { accessToken } = tokens.accessToken;

				// Notify backend and emit token for in-memory variable update
				await refreshLogin(accessToken);

				await emit('access-token-set', accessToken);
			} else {
				await logError('OAuth callback: No tokens found', null);
				await emit('error', 'No tokens found.');
			}
		} catch (error) {
			await logError('OAuth callback: Error processing tokens', error);
			await emit('error', 'Failed to process OAuth tokens.');
		}

		window.location.href = '/';
	});
</script>

<main>
	<h1>Redirecting...</h1>
</main>
