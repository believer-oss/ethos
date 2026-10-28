import { emit } from '@tauri-apps/api/event';
import { derived, writable, type Readable } from 'svelte/store';
import { appConfig, commits, repoConfig, repoStatus } from '$lib/stores';
import { getAllCommits, getRepoStatus, switchBranch } from '$lib/repo';
import type {
	BranchAnalysis,
	BranchCleanupCandidate,
	BranchCleanupResult,
	CommitTagCategory,
	ConflictFile,
	Nullable,
	RebaseCommitInfo,
	RebaseState,
	RebaseStateKind,
	RepoConfig,
	RepoStatus
} from '$lib/types';

export const FEATURE_BRANCH_PREFIX = 'f11r-branch-';
const MAX_SLUG_LEN = 48;

export const isFeatureBranch = (name: string): boolean => name.startsWith(FEATURE_BRANCH_PREFIX);

export const isQuickSubmitBranch = (name: string): boolean =>
	name.startsWith('f11r') && !isFeatureBranch(name);

export const isProtectedBranch = (name: string, config: Nullable<RepoConfig>): boolean =>
	!!config &&
	((config.trunkBranch !== '' && name === config.trunkBranch) ||
		config.targetBranches.some((b) => b.name === name));

export const branchesEnabled: Readable<boolean> = derived(appConfig, ($c) => $c.enableBranches);

// An unfinished guided rebase keeps Branch Management reachable whatever the settings.
export const branchManagementVisible: Readable<boolean> = derived(
	[appConfig, repoStatus],
	([$c, $s]) => ($c.enableBranches && $c.enableBranchManagement) || $s?.guidedRebaseActive === true
);

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

// The name of the user's Quick Submit branches for `target`, up to the timestamp. Mirrors the
// Rust `quick_submit_branch_prefix`.
export const quickSubmitBranchPrefix = (target: string, displayName: string): string =>
	`f11r-${target}-${displayName.replace(/ /g, '-')}-`;

// The target a Quick Submit branch (`f11r-<target>-<name>-<ts>`) was made for. Targets and display
// names can both contain '-', so the longest matching candidate wins. Mirrors the Rust
// `quick_submit_branch_target`.
export const quickSubmitBranchTarget = (branch: string, candidates: string[]): string | null => {
	if (!branch.startsWith('f11r-')) {
		return null;
	}
	const rest = branch.slice('f11r-'.length);
	return candidates
		.filter((t) => t !== '' && rest.startsWith(`${t}-`))
		.reduce<string | null>((best, t) => (best === null || t.length > best.length ? t : best), null);
};

// Mirrors the Rust `quick_submit_target_mismatch`: on another protected branch, or on a Quick
// Submit branch made for another target. The user's own branch for the target always matches;
// unparseable names (e.g. a target removed from config) are allowed.
export const quickSubmitTargetMismatch = (
	branch: string,
	target: string,
	cfg: Nullable<RepoConfig>,
	displayName: string
): boolean => {
	if (!cfg || target === '' || branch === target) {
		return false;
	}
	if (isQuickSubmitBranch(branch)) {
		if (branch.startsWith(quickSubmitBranchPrefix(target, displayName))) {
			return false;
		}
		const found = quickSubmitBranchTarget(branch, [
			...cfg.targetBranches.map((b) => b.name),
			target
		]);
		return found !== null && found !== target;
	}
	return isProtectedBranch(branch, cfg);
};

export interface TargetColor {
	chip: string;
	text: string;
}

// One color per configured target, by its index in targetBranches.
const TARGET_PALETTE: TargetColor[] = [
	{ chip: 'bg-primary-600 text-white', text: 'text-primary-400' },
	{ chip: 'bg-cyan-700 text-white', text: 'text-cyan-400' },
	{ chip: 'bg-purple-700 text-white', text: 'text-purple-400' }
];
const TARGET_FALLBACK: TargetColor = { chip: 'bg-gray-600 text-white', text: 'text-gray-300' };

export const targetBranchColor = (target: string, cfg: Nullable<RepoConfig>): TargetColor =>
	TARGET_PALETTE[cfg?.targetBranches.findIndex((b) => b.name === target) ?? -1] ?? TARGET_FALLBACK;

// Null when the target is not in the repo config.
export const targetUsesMergeQueue = (target: string, cfg: Nullable<RepoConfig>): boolean | null =>
	cfg?.targetBranches.find((b) => b.name === target)?.usesMergeQueue ?? null;

export type SubmitMode = 'quick' | 'branch' | 'unavailable';

// RepoStatus.guidedRebaseActive is set by the backend for inProgress, completedUnpushed and pushFailed.
export const isGuidedRebaseActive = (status: RepoStatus): boolean => status.guidedRebaseActive;

// Mirrors the Rust `is_branch_mode`: branch mode is any branch that is neither protected nor Quick Submit.
// `targetMismatch` blocks Quick Submit, as the backend's `ensure_quick_submit_allowed` does.
export const deriveSubmitMode = (
	status: Nullable<RepoStatus>,
	cfg: Nullable<RepoConfig>,
	target: string,
	displayName: string
): { mode: SubmitMode; reason: string; targetMismatch: boolean } => {
	if (!status || !cfg) {
		return { mode: 'unavailable', reason: '', targetMismatch: false };
	}
	if (status.detachedHead || status.branch === '' || isGuidedRebaseActive(status)) {
		return {
			mode: 'unavailable',
			reason:
				'Submitting is unavailable while the repository is in a detached HEAD state or a rebase is in progress. Open Branch Management to resolve it.',
			targetMismatch: false
		};
	}
	if (isProtectedBranch(status.branch, cfg) || isQuickSubmitBranch(status.branch)) {
		return {
			mode: 'quick',
			reason: '',
			targetMismatch: quickSubmitTargetMismatch(status.branch, target, cfg, displayName)
		};
	}
	return { mode: 'branch', reason: '', targetMismatch: false };
};

export interface BranchLifecycle {
	branch: string;
	merged: boolean;
	gone: boolean;
}

// Set by Branch Management after each analysis; null until then or after an analysis error.
export const branchLifecycle = writable<Nullable<BranchLifecycle>>(null);

// Advisory gate for branch submit; the backend check against origin is authoritative.
export const branchSubmitBlockReason = (
	status: RepoStatus,
	lifecycle: Nullable<BranchLifecycle> = null
): string | null => {
	if (
		lifecycle !== null &&
		lifecycle.branch === status.branch &&
		(lifecycle.merged || lifecycle.gone)
	) {
		return 'This branch was merged or deleted. Use Branch Management to return to your target branch.';
	}
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

// `<slug> to <trunk>`; branches without the feature prefix keep their full name.
export const defaultPrTitle = (branch: string, trunk: string): string =>
	`${isFeatureBranch(branch) ? branch.slice(FEATURE_BRANCH_PREFIX.length) : branch} to ${trunk}`;

export const PR_TITLE_MAX = 256;

export const EDITOR_OPEN = 'Unreal Editor is open. Close it to use Branch Management.';

// Mirrors the Rust `validate_pr_title`: trimmed, non-empty, at most 256 code points.
export const prTitleError = (title: string): string | null => {
	const trimmed = title.trim();
	if (trimmed === '') {
		return 'Pull request title is required.';
	}
	if ([...trimmed].length > PR_TITLE_MAX) {
		return 'Pull request title must be 256 characters or fewer.';
	}
	return null;
};

export type PrAction = 'create' | 'enqueue' | 'none';

// Which pull request button the analysis allows: none for a queued or merged PR.
export const prAction = (a: BranchAnalysis): PrAction => {
	if (a.pr === null) {
		return 'create';
	}
	return a.pr.state === 'OPEN' && a.pr.mergeQueueEntry === null ? 'enqueue' : 'none';
};

// Why local and origin differ, or null when they are in sync.
export const localSyncBlocker = (a: BranchAnalysis): string | null => {
	switch (a.localSync) {
		case 'inSync':
			return null;
		case 'ahead':
			return `${a.localAheadOfOrigin} local commit(s) not pushed`;
		case 'behind':
			return `${a.localBehindOrigin} commit(s) on origin not in your local branch — Sync`;
		case 'diverged':
			return 'Local and origin have diverged — Sync';
		default:
			return 'Local branch state is unknown — Refresh';
	}
};

// Reasons Create PR / Add to merge queue is disabled, in the backend's gate order; the backend
// re-checks each one. Being behind the trunk is not a blocker.
export const createPrBlockers = (a: BranchAnalysis): string[] => {
	if (!a.gitSupported) {
		return ['Update git to use Branch Management'];
	}
	const blockers: string[] = [];
	if (a.pr?.state === 'MERGED' || a.upstream === 'gone') {
		blockers.push('This branch was merged or deleted');
	}
	if (a.upstream === 'notPushed') {
		blockers.push('Push this branch to origin first');
	}
	const sync = localSyncBlocker(a);
	if (sync !== null) {
		blockers.push(sync);
	}
	if (a.commitsAhead === 0) {
		blockers.push(`No commits ahead of ${a.trunkBranch}`);
	}
	if (a.predictedConflicts.length > 0) {
		blockers.push(`Predicted conflicts with ${a.trunkBranch} — rebase first`);
	}
	if (a.prError !== null) {
		blockers.push(`Could not check GitHub: ${a.prError}`);
	}
	if (!a.usesMergeQueue) {
		blockers.push(`The merge queue is not enabled for ${a.trunkBranch} in this repo's config`);
	}
	return blockers;
};

export const shortSha = (sha: string): string => sha.substring(0, 8);

export const sha7 = (sha: string): string => sha.substring(0, 7);

// Mirrors the Rust `MIN_GUIDED_REBASE_GIT`; Branch Management itself needs only 2.38.
const MIN_GUIDED_REBASE_GIT: [number, number] = [2, 43];

// Mirrors the Rust `git_version_gate`: an unparseable version is unsupported.
export const guidedRebaseGitSupported = (version: string): boolean => {
	const m = /^(\d+)\.(\d+)/.exec(version.trim());
	if (!m) {
		return false;
	}
	const [major, minor] = [Number(m[1]), Number(m[2])];
	return (
		major > MIN_GUIDED_REBASE_GIT[0] ||
		(major === MIN_GUIDED_REBASE_GIT[0] && minor >= MIN_GUIDED_REBASE_GIT[1])
	);
};

// First reason the Rebase button is disabled, in the backend's gate order; the backend re-checks
// each one.
export const rebaseBlockReason = (
	status: Nullable<RepoStatus>,
	analysis: Nullable<BranchAnalysis>,
	rebase: Nullable<RebaseState>,
	busy: boolean
): string | null => {
	if (!status || !analysis) {
		return 'Loading branch status…';
	}
	if (!analysis.gitSupported) {
		return 'Update git to use Branch Management';
	}
	if (!guidedRebaseGitSupported(analysis.gitVersion)) {
		return `Update git to use Branch Management (found ${analysis.gitVersion}, need 2.43 or newer).`;
	}
	if (analysis.pr?.state === 'MERGED' || analysis.upstream === 'gone') {
		return 'This branch was merged or deleted.';
	}
	// Null means the state could not be read, which is not the same as idle.
	if (rebase === null) {
		return "Couldn't read the rebase state. Retry.";
	}
	if (rebase.state !== 'idle') {
		return 'Finish or dismiss the current rebase first.';
	}
	if (status.hasLocalChanges || status.untrackedFiles.length > 0) {
		return 'Commit, snapshot, or revert your changes before rebasing.';
	}
	if (status.commitsAhead > 0 || status.commitsBehind > 0) {
		return 'Your branch has unpushed or unsynced commits — submit or Sync first.';
	}
	if (analysis.commitsBehind === 0) {
		return `${analysis.branch} already contains the latest ${analysis.trunkBranch}.`;
	}
	if (busy) {
		return 'Wait for the current operation to finish.';
	}
	return null;
};

// A resolved file with no recorded choice was resolved outside Friendshipper, so it counts as
// a manual merge.
const isManualResolution = (f: ConflictFile): boolean =>
	f.resolved && (f.resolution === 'manual' || f.resolution === null);

// Whether Undo discards edits the user made, so it needs a confirmation first.
export const undoNeedsConfirm = (f: ConflictFile): boolean => isManualResolution(f);

// True when the chosen side is the one that deleted the file.
const choseDeletion = (f: ConflictFile): boolean =>
	f.deletedOn !== null && f.resolution === f.deletedOn;

export const resolutionLabel = (f: ConflictFile): string => {
	if (isManualResolution(f)) {
		return 'Manually merged';
	}
	if (choseDeletion(f)) {
		return 'Deleted';
	}
	return f.resolution === 'main' ? "Kept main's version" : "Kept branch's version";
};

// ' on <date>', or empty when the date is unknown.
const onDate = (c: RebaseCommitInfo): string => {
	if (c.date === '') return '';
	const parsed = new Date(c.date);
	const text = Number.isNaN(parsed.getTime())
		? c.date
		: parsed.toLocaleString(undefined, { dateStyle: 'medium', timeStyle: 'short' });
	return ` on ${text}`;
};

// The choice for the Continue confirmation; Continue can still refuse, e.g. for edits left
// unstaged.
export const confirmLine = (f: ConflictFile): string => {
	if (isManualResolution(f)) {
		return 'Manually merged';
	}
	if (choseDeletion(f)) {
		return 'Deleted';
	}
	if (f.resolution === 'main') {
		return f.mainSide
			? `Main's version — by ${f.mainSide.author}${onDate(f.mainSide)} in ${sha7(f.mainSide.sha)}`
			: "Main's version";
	}
	return `Branch's version — by ${f.branchSide.author}${onDate(f.branchSide)} in ${sha7(
		f.branchSide.sha
	)}`;
};

export const mainSideText = (f: ConflictFile): string => {
	if (f.deletedOn === 'main') {
		return f.mainSide
			? `deleted on main by ${f.mainSide.author}${onDate(f.mainSide)}`
			: 'deleted on main';
	}
	return f.mainSide
		? `last changed by ${f.mainSide.author}${onDate(f.mainSide)} in ${sha7(f.mainSide.sha)} "${
				f.mainSide.subject
		  }"`
		: 'last changed on main';
};

export const branchSideText = (f: ConflictFile): string =>
	f.deletedOn === 'branch'
		? `deleted on branch${onDate(f.branchSide)}`
		: `from ${sha7(f.branchSide.sha)} by ${f.branchSide.author}${onDate(f.branchSide)}`;

// Mirrors the backend's stale messages.
export const staleMessage = (state: RebaseState): string => {
	switch (state.staleReason) {
		case 'abortedOutside':
			return `The rebase of ${state.branch} was aborted outside Friendshipper. Your branch is back where it started.`;
		case 'pushedOutside':
			return `The rebased ${state.branch} is already on origin. Sync and download editor binaries if needed.`;
		case 'branchChanged':
			return `You switched away from ${state.branch} outside Friendshipper while it was being rebased. Its backup is kept at ${state.backupRef}.`;
		case 'otherRebase':
			return 'A different rebase was started outside Friendshipper. Finish it in your git client or in Diagnostics.';
		case 'unreadable':
			return "Friendshipper's rebase record is unreadable. Your backup refs are kept under refs/f11r-backup/.";
		default:
			return `${state.branch} was changed outside Friendshipper during the rebase. Its backup is kept at ${state.backupRef}.`;
	}
};

// Advisory; Continue re-checks everything, and the confirmation step does not promise success.
export const canContinue = (state: RebaseState): boolean =>
	state.state === 'inProgress' &&
	!state.stepIncomplete &&
	state.conflicts.length > 0 &&
	state.conflicts.every((c) => c.resolved && c.kind !== 'unsupported');

export const pushFailedText = (state: RebaseState): string =>
	state.pushFailedRemoteSha
		? `Someone pushed to ${state.branch} (${shortSha(
				state.pushFailedRemoteSha
		  )}) while you were rebasing. Sync and submit are blocked until you choose:`
		: `${state.branch} was changed or deleted on origin while you were rebasing. Sync and submit are blocked until you choose:`;

export const rebaseViewFor = (state: Nullable<RebaseState>): 'resolver' | 'recovery' | 'none' => {
	const kind: Nullable<RebaseStateKind> = state?.state ?? null;
	if (kind === 'inProgress') {
		return 'resolver';
	}
	return kind === 'completedUnpushed' || kind === 'pushFailed' || kind === 'stale'
		? 'recovery'
		: 'none';
};

export const localBranchCount = (n: number): string => `${n} local branch${n === 1 ? '' : 'es'}`;

export type CleanupTone = 'safe' | 'warning' | 'unknown';

export interface CleanupNote {
	tone: CleanupTone;
	text: string;
}

// Safety note for a branch-cleanup candidate; anything but 'safe' needs the SHA acknowledgement.
export const cleanupNote = (c: BranchCleanupCandidate, trunk: string): CleanupNote => {
	const unmerged = `${c.unmergedCommitCount} commit${
		c.unmergedCommitCount === 1 ? '' : 's'
	} not on ${trunk}`;
	if (c.mergedIntoTrunk) return { tone: 'safe', text: `Merged into ${trunk}` };
	if (c.patchEquivalent) return { tone: 'safe', text: `Merged into ${trunk} (same changes)` };
	if (c.mergedViaPr && c.safe)
		return {
			tone: 'safe',
			text: `Merged via PR #${c.mergedViaPr.number} into ${c.mergedViaPr.baseRefName}`
		};
	if (c.mergedViaPr) {
		return {
			tone: 'warning',
			text: `Merged via PR #${c.mergedViaPr.number} into ${c.mergedViaPr.baseRefName}, not a target branch; ${unmerged}`
		};
	}
	if (c.prUnknown) return { tone: 'unknown', text: `Unknown (GitHub unavailable); ${unmerged}` };
	return { tone: 'warning', text: unmerged };
};

// True when the branch is safe only because of a merged pull request, which can't be rechecked offline.
export const cleanupSafeViaPr = (c: BranchCleanupCandidate): boolean =>
	c.safe && !c.mergedIntoTrunk && !c.patchEquivalent;

// Toast texts for a cleanup response: deleted branches with their SHAs, then failures.
export const cleanupResultMessages = (
	results: BranchCleanupResult[]
): { success: string | null; failure: string | null } => {
	const deleted = results.filter((r) => r.deleted);
	const failed = results.filter((r) => !r.deleted);
	return {
		success:
			deleted.length > 0
				? `Deleted ${localBranchCount(deleted.length)}: ${deleted
						.map((r) => `${r.name} (${r.sha})`)
						.join(', ')}`
				: null,
		failure:
			failed.length > 0
				? `Not deleted: ${failed.map((r) => `${r.name}: ${r.error ?? 'unknown error'}`).join('; ')}`
				: null
	};
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
