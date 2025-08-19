import { OktaAuth } from '@okta/okta-auth-js';
import { jwtDecode } from 'jwt-decode';
import type { AccessToken } from '@okta/okta-auth-js';

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
			syncStorage: true
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

	// Listen for token removal
	oktaAuth.tokenManager.on('removed', (key: string) => {
		if (key === 'accessToken') {
			onTokenExpired();
		}
	});
};
