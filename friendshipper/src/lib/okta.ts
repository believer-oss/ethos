import { OktaAuth } from '@okta/okta-auth-js';
import { jwtDecode } from 'jwt-decode';
import type { AccessToken } from '@okta/okta-auth-js';

// PKCE utilities for external browser OAuth flow
export const generateCodeVerifier = (): string => {
	const array = new Uint8Array(32);
	crypto.getRandomValues(array);
	return btoa(String.fromCharCode.apply(null, Array.from(array)))
		.replace(/\+/g, '-')
		.replace(/\//g, '_')
		.replace(/=/g, '');
};

export const generateCodeChallenge = async (codeVerifier: string): Promise<string> => {
	const encoder = new TextEncoder();
	const data = encoder.encode(codeVerifier);
	const digest = await crypto.subtle.digest('SHA-256', data);
	return btoa(String.fromCharCode.apply(null, Array.from(new Uint8Array(digest))))
		.replace(/\+/g, '-')
		.replace(/\//g, '_')
		.replace(/=/g, '');
};

// Build OAuth authorization URL manually since buildAuthorizeUrl doesn't exist
export const buildOAuthUrl = (
	issuer: string,
	clientId: string,
	redirectUri: string,
	codeChallenge: string,
	scopes: string[] = ['openid', 'email', 'profile', 'offline_access']
): string => {
	const params = new URLSearchParams({
		client_id: clientId,
		response_type: 'code',
		scope: scopes.join(' '),
		redirect_uri: redirectUri,
		code_challenge: codeChallenge,
		code_challenge_method: 'S256',
		state: Math.random().toString(36).substring(2, 15) // Random state for security
	});

	// Ensure the issuer ends with /oauth2 for the authorize endpoint
	const baseUrl = issuer.endsWith('/oauth2') ? issuer : `${issuer}/oauth2`;
	return `${baseUrl}/v1/authorize?${params.toString()}`;
};

// Exchange authorization code for tokens manually
export const exchangeCodeForTokens = async (
	issuer: string,
	clientId: string,
	authCode: string,
	codeVerifier: string,
	redirectUri: string
): Promise<{ access_token: string; id_token?: string; refresh_token?: string }> => {
	// Ensure the issuer ends with /oauth2 for the token endpoint
	const baseUrl = issuer.endsWith('/oauth2') ? issuer : `${issuer}/oauth2`;
	const tokenEndpoint = `${baseUrl}/v1/token`;

	const body = new URLSearchParams({
		grant_type: 'authorization_code',
		client_id: clientId,
		code: authCode,
		redirect_uri: redirectUri,
		code_verifier: codeVerifier
	});

	const response = await fetch(tokenEndpoint, {
		method: 'POST',
		headers: {
			'Content-Type': 'application/x-www-form-urlencoded',
			Accept: 'application/json'
		},
		body: body.toString()
	});

	if (!response.ok) {
		const errorText = await response.text();
		throw new Error(
			`Token exchange failed: ${response.status} ${response.statusText} - ${errorText}`
		);
	}

	const tokens = (await response.json()) as {
		access_token: string;
		id_token?: string;
		refresh_token?: string;
	};
	return tokens;
};

export const createOktaAuth = (issuer: string, clientId: string) => {
	const redirectUri = `${window.location.origin}/auth/callback`;
	const postLogoutRedirectUri = window.location.origin;

	return new OktaAuth({
		issuer,
		clientId,
		redirectUri,
		postLogoutRedirectUri,
		pkce: true,
		tokenManager: {
			autoRenew: true,
			autoRemove: true,
			syncStorage: true,
			expireEarlySeconds: 300 // Renew token 5 minutes before expiration
		}
	});
};

export function isTokenExpired(token: string | null): boolean {
	// Check if the token is null or an empty string
	if (!token) {
		return true;
	}

	try {
		const decodedToken = jwtDecode<{ exp: number }>(token);
		const currentTime = Math.floor(Date.now() / 1000);
		return decodedToken.exp < currentTime;
	} catch (_) {
		// If decoding fails, consider the token as expired
		return true;
	}
}

export const clearExpiredTokens = async (oktaAuth: OktaAuth): Promise<void> => {
	try {
		const tokens = await oktaAuth.tokenManager.getTokens();

		// Check and remove expired access token
		if (tokens.accessToken && oktaAuth.tokenManager.hasExpired(tokens.accessToken)) {
			oktaAuth.tokenManager.remove('accessToken');
			// eslint-disable-next-line no-console
			console.info('Removed expired access token');
		}

		// Check and remove expired ID token
		if (tokens.idToken && oktaAuth.tokenManager.hasExpired(tokens.idToken)) {
			oktaAuth.tokenManager.remove('idToken');
			// eslint-disable-next-line no-console
			console.info('Removed expired ID token');
		}

		// Note: Refresh tokens are opaque, can't check expiration client-side
		// Okta will handle refresh token expiration on renewal attempts
	} catch (error) {
		// eslint-disable-next-line no-console
		console.error('Error clearing expired tokens:', error);
	}
};

export const setupOktaEventListeners = (
	oktaAuth: OktaAuth,
	onTokenRenewed: (token: string) => void,
	onTokenExpired: () => void
) => {
	// Listen for automatic token renewals
	oktaAuth.tokenManager.on('renewed', (key: string, newToken: AccessToken) => {
		if (key === 'accessToken' && newToken.accessToken) {
			onTokenRenewed(newToken.accessToken);
		}
	});

	// Listen for token expiration
	oktaAuth.tokenManager.on('expired', (key: string) => {
		if (key === 'accessToken') {
			// With autoRenew: true, Okta should handle renewal automatically
			// If we get here, auto-renewal failed, so trigger re-authentication
			onTokenExpired();
		}
	});

	// Listen for token removal - but don't trigger automatic re-auth to prevent loops
	oktaAuth.tokenManager.on('removed', (key: string) => {
		if (key === 'accessToken') {
			// Log the removal but don't trigger re-authentication
			// The 'expired' event handler will manage re-auth when needed
			// Token removal is expected during cleanup
		}
	});
};
