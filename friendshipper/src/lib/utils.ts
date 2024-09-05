import { emit } from '@tauri-apps/api/event';
import { invoke } from '@tauri-apps/api/tauri';
import type { TauriError } from '$lib/types';
import { restart } from '$lib/system';
import { checkLoginRequired } from '$lib/auth';

export const openUrl = async (url: string) => {
	await invoke('open_url', { url });
};

export const handleError = async (e: unknown) => {
	await emit('error', e);

	const error = e as TauriError;
	// check auth status
	if (error.status_code === 401) {
		const loginRequired = await checkLoginRequired();
		if (loginRequired) {
			await restart();
		}
	}
};
