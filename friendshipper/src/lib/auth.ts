import { invoke } from '@tauri-apps/api/core';
import { invokeWithAuth } from '$lib/http';

export const checkLoginRequired = async (): Promise<boolean> => invoke('check_login_required');

export const refreshLogin = async (token?: string): Promise<void> => {
	await invokeWithAuth('refresh_login', { token });
};

export const logout = async (): Promise<void> => {
	await invokeWithAuth('logout');
};
