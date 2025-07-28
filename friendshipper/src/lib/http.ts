import { invoke } from '@tauri-apps/api/core';
import { emit } from '@tauri-apps/api/event';
import { get } from 'svelte/store';
import { refreshLogin, checkLoginRequired } from '$lib/auth';
import { oktaAuth } from '$lib/stores';
import { restart } from '$lib/system';

interface ApiError extends Error {
	status_code?: number;
}

let isReauthenticating = false;
let pendingRequests: Array<() => Promise<unknown>> = [];

/**
 * Handles the reauthentication process with Okta
 */
async function handleReauthentication(): Promise<void> {
	try {
		// Check if login is required
		const loginRequired = await checkLoginRequired();

		if (loginRequired) {
			const $oktaAuth = get(oktaAuth);

			if ($oktaAuth) {
				// Try to refresh tokens first
				try {
					await $oktaAuth.session.refresh();
					const tokens = await $oktaAuth.tokenManager.getTokens();

					if (tokens.accessToken) {
						await refreshLogin(tokens.accessToken.accessToken);
						localStorage.setItem('oktaAccessToken', tokens.accessToken.accessToken);
						await emit('access-token-set', tokens.accessToken.accessToken);
						return;
					}
				} catch {
					// If refresh fails, fall through to full login
				}

				// If refresh failed, trigger full reauthentication
				await emit('error', new Error('Authentication expired. Please log in again.'));
				await restart();
			} else {
				await emit(
					'error',
					new Error('Authentication service unavailable. Please restart the application.')
				);
				await restart();
			}
		}
	} catch (error) {
		await emit(
			'error',
			new Error(
				`Reauthentication failed: ${error instanceof Error ? error.message : 'Unknown error'}`
			)
		);
		throw error;
	}
}

/**
 * Global error handler for API responses
 * Automatically handles 401 errors by triggering Okta reauthentication
 */
export async function handleApiError(
	error: unknown,
	retryCallback?: () => Promise<unknown>
): Promise<unknown> {
	const apiError = error as ApiError;

	if (apiError.status_code === 401) {
		// If we're already reauthenticating, queue this request
		if (isReauthenticating && retryCallback) {
			return new Promise<unknown>((resolve, reject) => {
				pendingRequests.push(async () => {
					try {
						const result = await retryCallback();
						resolve(result);
					} catch (err) {
						reject(err instanceof Error ? err : new Error(String(err)));
					}
				});
			});
		}

		// Start reauthentication process
		if (!isReauthenticating) {
			isReauthenticating = true;

			try {
				await handleReauthentication();

				// Retry the original request if callback provided
				if (retryCallback) {
					const result = await retryCallback();

					// Process any queued requests
					const queuedRequests = [...pendingRequests];
					pendingRequests = [];

					await Promise.all(queuedRequests.map((request) => request()));

					return result;
				}
			} catch (reauthError) {
				// If reauthentication fails, clear queue and propagate error
				pendingRequests = [];
				throw new Error(
					reauthError instanceof Error ? reauthError.message : 'Reauthentication failed'
				);
			} finally {
				isReauthenticating = false;
			}
		}
	}

	// For non-401 errors or if no retry callback, just throw the original error
	throw error;
}

/**
 * Wrapper for Tauri invoke calls that automatically handles 401 errors
 */
export async function invokeWithAuth<T>(
	command: string,
	args?: Record<string, unknown>
): Promise<T> {
	const makeRequest = () => invoke<T>(command, args);

	try {
		return await makeRequest();
	} catch (error) {
		return await handleApiError(error, makeRequest);
	}
}

/**
 * Enhanced error handler that can be used in catch blocks throughout the app
 */
export async function handleError(
	error: unknown,
	retryCallback?: () => Promise<unknown>
): Promise<unknown> {
	return handleApiError(error, retryCallback);
}
