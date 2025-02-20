import { invoke } from '@tauri-apps/api/core';

export const openUrl = async (url: string) => {
	await invoke('open_url', { url });
};
