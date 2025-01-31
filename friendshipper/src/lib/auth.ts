import { invoke } from '@tauri-apps/api/tauri';

export const checkLoginRequired = async (): Promise<boolean> => invoke('check_login_required');

export const refreshAWSLogin = async (token?: string): Promise<void> => {
	await invoke('refresh_aws_login', { token });
};

export const logout = async (): Promise<void> => {
	await invoke('logout');
};

export const authenticate = async (): Promise<void> => {
	await invoke('authenticate');
};

export const refreshAuth = async (token?: string): Promise<void> => {
	await invoke('refresh', { token });
};
