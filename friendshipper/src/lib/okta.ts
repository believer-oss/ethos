import { OktaAuth } from '@okta/okta-auth-js';
import { jwtDecode } from 'jwt-decode';
import type { AccessToken, Tokens } from '@okta/okta-auth-js';

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

export const renewTokens = async (oktaAuth: OktaAuth): Promise<boolean> => {
	try {
		// Use Okta's built-in token renewal with refresh token
		const tokens: Tokens = await oktaAuth.token.renew(['accessToken', 'idToken']);

		if (tokens.accessToken) {
			// Okta automatically updates tokenManager, just return success
			return true;
		}
		return false;
	} catch (error) {
		// If refresh token is invalid/expired, this will fail
		// eslint-disable-next-line no-console
		console.error('Token renewal failed:', error);
		return false;
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
			// Try to renew the token using refresh token
			renewTokens(oktaAuth)
				.then((renewed) => {
					if (!renewed) {
						// If renewal fails, trigger re-authentication
						onTokenExpired();
					}
				})
				.catch((error) => {
					// eslint-disable-next-line no-console
					console.error('Token renewal error:', error);
					onTokenExpired();
				});
		}
	});

	// Listen for token removal
	oktaAuth.tokenManager.on('removed', (key: string) => {
		if (key === 'accessToken') {
			onTokenExpired();
		}
	});
};
