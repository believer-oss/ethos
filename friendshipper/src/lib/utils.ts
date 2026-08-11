import { invoke } from '@tauri-apps/api/core';
import { emit } from '@tauri-apps/api/event';
import type { TargetBranchConfig } from '$lib/types';

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

export const logInfo = async (message: string) => {
	// Log to backend Rust logs only (no frontend notification)
	try {
		await invoke('log_info', { message });
	} catch (_) {
		// Ignore if logging command fails
	}
};

export const resolveTrunkBranch = (
	primaryBranch: string | undefined | null,
	targetBranches: TargetBranchConfig[] | undefined | null
): string => {
	if (primaryBranch) return primaryBranch;
	if (targetBranches && targetBranches.length > 0) return targetBranches[0].name;
	return 'main';
};

export const cleanBranchName = (branch: string): string => branch.replace(/^refs\/heads\//, '');

// Argo writes the branch label with `refs/heads/` stripped and every `/` rewritten as `_` (see
// docs/build-artifacts.md:75), so `release/2025.1` arrives as `release_2025.1`. Normalize both
// sides before comparing. Display of branch names is left untouched.
const normalizeBranchForTrunkMatch = (branch: string): string =>
	cleanBranchName(branch).replace(/\//g, '_').toLowerCase();

export const isTrunkBuild = (branch: string | null | undefined, trunkBranch: string): boolean => {
	if (!branch || !trunkBranch) return false;
	return normalizeBranchForTrunkMatch(branch) === normalizeBranchForTrunkMatch(trunkBranch);
};

export const formatRelativeAge = (
	lastModifiedSeconds: number,
	nowMs: number = Date.now()
): string => {
	const diffSeconds = nowMs / 1000 - lastModifiedSeconds;

	if (diffSeconds < 60) return 'just now';
	if (diffSeconds < 3600) return `${Math.floor(diffSeconds / 60)}m`;
	if (diffSeconds < 86400) return `${Math.floor(diffSeconds / 3600)}h`;
	if (diffSeconds < 30 * 86400) return `${Math.floor(diffSeconds / 86400)}d`;

	return new Date(lastModifiedSeconds * 1000).toLocaleDateString('en-US', {
		month: 'short',
		day: 'numeric'
	});
};
