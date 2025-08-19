import { invoke } from '@tauri-apps/api/core';
import { emit } from '@tauri-apps/api/event';

export const openUrl = async (url: string) => {
	await invoke('open_url', { url });
};

export const handleError = async (e: unknown) => {
	await emit('error', e);
};

// Enhanced logging that goes to both frontend and backend logs
export const logError = async (message: string, error?: unknown) => {
	const fullMessage = error ? `${message}: ${String(error)}` : message;

	// Emit to frontend (for notifications)
	await emit('error', fullMessage);

	// Log to backend Rust logs
	try {
		await invoke('log_error', { message: fullMessage });
	} catch (_) {
		// Ignore if logging command fails
	}
};

export const logSuccess = async (message: string) => {
	// Emit to frontend (for notifications)
	await emit('success', message);

	// Log to backend Rust logs
	try {
		await invoke('log_info', { message });
	} catch (_) {
		// Ignore if logging command fails
	}
};
