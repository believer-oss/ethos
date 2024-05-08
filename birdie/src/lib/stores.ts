import { derived, type Readable, writable } from 'svelte/store';
import { type Commit, type ModifiedFile } from '@ethos/core';
import type {
	AppConfig,
	Nullable,
	RepoConfig,
	RepoStatus,
	LFSFile,
	VerifyLocksResponse
} from '$lib/types';

export const updateDismissed = writable(false);
export const appConfig = writable(<AppConfig>{});
export const repoConfig = writable(<Nullable<RepoConfig>>null);
export const commits = writable(<Commit[]>[]);
export const repoStatus = writable(<Nullable<RepoStatus>>null);
export const currentRoot = writable('');
export const commitMessage = writable('');
export const selectedFiles = writable(<ModifiedFile[]>[]);

export const locks = writable(<VerifyLocksResponse>{
	ours: [],
	theirs: [],
	nextCursor: null
});

export const currentRootFiles = writable(<LFSFile[]>[]);
export const enableGlobalSearch = writable(true);
export const onboardingInProgress = writable(false);

export const latestLocalCommit: Readable<Nullable<Commit>> = derived(commits, ($commits) => {
	for (const commit of $commits) {
		if (commit.local) {
			return commit;
		}
	}

	return null;
});

export const allModifiedFiles = derived(repoStatus, ($repoStatus) => {
	const untracked = $repoStatus?.untrackedFiles ?? [];
	const modified = $repoStatus?.modifiedFiles ?? [];

	const all: ModifiedFile[] = [...untracked, ...modified];
	all.sort((a, b) => (a.path < b.path ? -1 : 1));

	return all;
});
