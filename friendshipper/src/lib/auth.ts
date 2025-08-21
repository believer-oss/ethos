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
