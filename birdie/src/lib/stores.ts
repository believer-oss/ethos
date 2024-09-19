import { derived, type Readable, writable } from 'svelte/store';
import type { Commit, ModifiedFile } from '@ethos/core';
import type {
	BirdieConfig,
	LFSFile,
	Node,
	Nullable,
	RepoStatus,
	VerifyLocksResponse
} from '$lib/types';

export const updateDismissed = writable(false);
export const appConfig = writable(<BirdieConfig>{});
export const commits = writable(<Commit[]>[]);
export const repoStatus = writable(<Nullable<RepoStatus>>null);
export const currentRoot = writable('');
export const fileTree = writable(<Node>{
	parent: null,
	value: null,
	children: []
});
export const commitMessage = writable('');
export const selectedFiles = writable(<ModifiedFile[]>[]);
export const fetchIncludeList = writable(<string[]>[]);

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
