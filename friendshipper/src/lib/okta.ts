import { jwtDecode } from 'jwt-decode';

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
