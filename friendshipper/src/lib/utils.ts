import { invoke } from '@tauri-apps/api/core';
import { emit } from '@tauri-apps/api/event';
import { handleError as handleApiError } from '$lib/http';

export const openUrl = async (url: string) => {
	await invoke('open_url', { url });
};

export const handleError = async (
	e: unknown,
	retryCallback?: () => Promise<unknown>
): Promise<unknown> => {
	try {
		if (retryCallback) {
			return await handleApiError(e, retryCallback);
		}
		return await handleApiError(e);
	} catch (error) {
		await emit('error', error);
		throw error;
	}
};
