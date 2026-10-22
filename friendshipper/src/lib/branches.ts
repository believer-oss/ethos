import { emit } from '@tauri-apps/api/event';
import { derived, type Readable } from 'svelte/store';
import { commits, repoConfig, repoStatus } from '$lib/stores';
import { getAllCommits, getRepoStatus, switchBranch } from '$lib/repo';
import type { CommitTagCategory, Nullable, RepoConfig, RepoStatus } from '$lib/types';

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

export type SubmitMode = 'quick' | 'branch' | 'unavailable';

// Always false: RepoStatus does not report guided rebase state yet.
export const isGuidedRebaseActive = (_status: RepoStatus): boolean => false;

// Mirrors the Rust `is_branch_mode`: branch mode is any branch that is neither protected nor Quick Submit.
export const deriveSubmitMode = (
	status: Nullable<RepoStatus>,
	cfg: Nullable<RepoConfig>
): { mode: SubmitMode; reason: string } => {
	if (!status || !cfg) {
		return { mode: 'unavailable', reason: '' };
	}
	if (status.detachedHead || status.branch === '' || isGuidedRebaseActive(status)) {
		return {
			mode: 'unavailable',
			reason:
				'Submitting is unavailable while the repository is in a detached HEAD state or a rebase is in progress. Open Branch Management to resolve it.'
		};
	}
	if (isProtectedBranch(status.branch, cfg) || isQuickSubmitBranch(status.branch)) {
		return { mode: 'quick', reason: '' };
	}
	return { mode: 'branch', reason: '' };
};

// Advisory gate for branch submit; the backend check against origin is authoritative.
export const branchSubmitBlockReason = (status: RepoStatus): string | null => {
	if (status.remoteBranch === '') {
		return 'This branch has no upstream on origin. Create feature branches with New branch in Branch Management.';
	}
	if (status.remoteBranch !== `origin/${status.branch}`) {
		return `This branch tracks ${status.remoteBranch}, not origin/${status.branch}. Switch to it from the branch selector.`;
	}
	if (status.commitsBehind > 0) {
		return 'Your branch is behind origin — Sync first';
	}
	return null;
};

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
