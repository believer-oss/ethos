import { emit } from '@tauri-apps/api/event';
import { derived, type Readable } from 'svelte/store';
import { commits, repoConfig, repoStatus } from '$lib/stores';
import { getAllCommits, getRepoStatus, switchBranch } from '$lib/repo';
import type { CommitTagCategory, Nullable, RepoConfig } from '$lib/types';

export const FEATURE_BRANCH_PREFIX = 'f11r-branch-';
const MAX_SLUG_LEN = 48;

export const isFeatureBranch = (name: string): boolean => name.startsWith(FEATURE_BRANCH_PREFIX);

export const isQuickSubmitBranch = (name: string): boolean =>
	name.startsWith('f11r') && !isFeatureBranch(name);

export const isProtectedBranch = (name: string, config: Nullable<RepoConfig>): boolean =>
	!!config &&
	((config.trunkBranch !== '' && name === config.trunkBranch) ||
		config.targetBranches.some((b) => b.name === name));

// True when branch management must disable everything except create and select.
export const branchActionsDisabled: Readable<boolean> = derived(
	[repoStatus, repoConfig],
	([$s, $c]) =>
		!!$s &&
		($s.branch === '' ? false : isProtectedBranch($s.branch, $c) || isQuickSubmitBranch($s.branch))
);

// Mirrors the Rust `normalize_and_validate_slug`.
export const normalizeSlug = (input: string): string =>
	input
		.trim()
		.toLowerCase()
		.replace(/[\s_]/g, '-')
		.replace(/-+/g, '-')
		.replace(/^-+|-+$/g, '');

// Returns an error message, or null when the (normalized) slug is valid.
export const validateSlug = (slug: string): string | null => {
	if (slug === '') {
		return 'Branch name is required.';
	}
	if (!/^[a-z0-9-]+$/.test(slug)) {
		return "Branch name can only contain letters, numbers, spaces, '-' and '_'.";
	}
	if (slug.length > MAX_SLUG_LEN) {
		return 'Branch name must be 48 characters or fewer.';
	}
	return null;
};

// Mirrors the backend Quick Submit guard; unknown state (no branch or config) is allowed.
export const canQuickSubmitOn = (
	branch: string | undefined,
	config: Nullable<RepoConfig>
): boolean =>
	!branch || !config || isProtectedBranch(branch, config) || isQuickSubmitBranch(branch);

// Chosen tag per category in category order; empty choices and option-less categories are dropped.
export const selectedTags = (categories: CommitTagCategory[], selected: string[]): string[] =>
	categories
		.map((c, i) => (c.options.length > 0 ? selected[i] ?? '' : ''))
		.filter((tag) => tag !== '');

export const composeCommitMessage = (tags: string[], message: string): string =>
	tags.length === 0 ? message : `${tags.join(' ')} ${message}`;

// Status first and on its own, so a failed commit fetch never leaves a stale branch shown.
export const refreshAfterBranchChange = async (): Promise<void> => {
	try {
		repoStatus.set(await getRepoStatus());
	} catch (e) {
		await emit('error', e);
	}
	try {
		commits.set(await getAllCommits());
	} catch (e) {
		await emit('error', e);
	}
};

export const performSwitch = async (branch: string): Promise<boolean> => {
	try {
		const res = await switchBranch(branch);
		await refreshAfterBranchChange();
		if (res.warning) {
			await emit('error', res.warning);
		} else {
			await emit('success', `Switched to ${res.branch}`);
		}
		return true;
	} catch (e) {
		await emit('error', e);
		await refreshAfterBranchChange();
		return false;
	}
};
