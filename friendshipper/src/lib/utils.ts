import { invoke } from '@tauri-apps/api/core';
import { emit } from '@tauri-apps/api/event';

export const openUrl = async (url: string) => {
	await invoke('open_url', { url });
};

export const handleError = async (e: unknown) => {
	await emit('error', e);
};
