import { invoke } from '@tauri-apps/api/core';

export const checkLoginRequired = async (): Promise<boolean> => invoke('check_login_required');

export const refreshLogin = async (token?: string): Promise<void> => {
	await invoke('refresh_login', { token });
};

export const logout = async (): Promise<void> => {
	await invoke('logout');
};

export const exitApp = async (): Promise<void> => {
	await invoke('exit_app');
};

export const createOAuthPopup = async (url: string): Promise<void> => {
	await invoke('create_oauth_popup', { url });
};

export interface GithubAuthStatus {
	connected: boolean;
	username: string;
	expiresAt: string | null;
}

export const getGithubAuthStatus = async (): Promise<GithubAuthStatus> =>
	invoke('get_github_auth_status');

// Opens the user's browser to authorize the Friendshipper GitHub App. The
// backend finishes the flow via its localhost callback route.
export const connectGithub = async (token: string): Promise<void> => {
	await invoke('connect_github', { token });
};

// Asks the backend to refresh the GitHub access token if it's near expiry.
// Resolves true if a refresh happened.
export const refreshGithubToken = async (token: string): Promise<boolean> =>
	invoke('refresh_github_token', { token });
