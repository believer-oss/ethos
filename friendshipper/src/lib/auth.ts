import { invoke } from '@tauri-apps/api/tauri';

export const checkLoginRequired = async (): Promise<boolean> => invoke('check_login_required');

export const refreshLogin = async (): Promise<void> => {
	await invoke('refresh_login');
};
