import { invoke } from '@tauri-apps/api/tauri';

export const openUrl = async (url: string) => {
	await invoke('open_url', { url });
};
