import { invoke } from '@tauri-apps/api/tauri';
import type { Commit, CommitFileInfo } from '@ethos/core';
import type {
	CloneRequest,
	GitHubPullRequest,
	MergeQueue,
	PushRequest,
	RebaseStatusResponse,
	RepoStatus,
	RevertFilesRequest,
	Snapshot
} from '$lib/types';

export const getCommits = async (
	limit?: number,
	remote?: boolean,
	update?: boolean
): Promise<Commit[]> => invoke('get_commits', { limit, remote, update });

// "All" here refers to the combination of local and upstream, not every commit.
// Right now we're just pulling 250.
export const getAllCommits = async (): Promise<Commit[]> => {
	let localCommits: Commit[];
	try {
		localCommits = await getCommits(250, false, false);
	} catch (_) {
		localCommits = [];
	}

	let remoteCommits: Commit[];
	try {
		remoteCommits = await getCommits(250, true, false);
	} catch (_) {
		remoteCommits = [];
	}

	for (const commit of localCommits) {
		commit.local = true;
	}

	for (const remoteCommit of remoteCommits) {
		const index = localCommits.findIndex((v) => v.sha === remoteCommit.sha);
		remoteCommit.local = index !== -1;
	}

	return remoteCommits.length >= localCommits.length ? remoteCommits : localCommits;
};

export const cloneRepo = async (req: CloneRequest): Promise<void> => {
	req.path = req.path.replace(/\\/g, '/');
	await invoke('clone_repo', { req });
};

export enum SkipDllCheck {
	False = 0,
	True = 1
}

export enum AllowOfflineCommunication {
	False = 0,
	True = 1
}

export const getRepoStatus = async (
	shouldSkipDllCheck: SkipDllCheck = SkipDllCheck.False,
	shouldAllowOfflineCommunication: AllowOfflineCommunication = AllowOfflineCommunication.False
): Promise<RepoStatus> => {
	const skipDllCheck: boolean = shouldSkipDllCheck === SkipDllCheck.True;
	const allowOfflineCommunication: boolean =
		shouldAllowOfflineCommunication === AllowOfflineCommunication.True;
	return invoke('get_repo_status', { skipDllCheck, allowOfflineCommunication });
};

export const submit = async (req: PushRequest): Promise<void> => invoke('submit', { req });

export const quickSubmit = async (req: PushRequest): Promise<void> =>
	invoke('quick_submit', { req });

export const listSnapshots = async (): Promise<Snapshot[]> => invoke('list_snapshots');

export const restoreSnapshot = async (commit: string): Promise<void> =>
	invoke('restore_snapshot', { commit });

export const saveSnapshot = async (message: string, files: string[]): Promise<void> =>
	invoke('save_snapshot', { message, files });

export const deleteSnapshot = async (commit: string): Promise<void> =>
	invoke('delete_snapshot', { commit });
export const revertFiles = async (req: RevertFilesRequest): Promise<void> =>
	invoke('revert_files', { req });
export const getPullRequests = async (limit: number): Promise<GitHubPullRequest[]> =>
	invoke('get_pull_requests', { limit });

export const syncLatest = async (): Promise<void> => invoke('sync_latest');

export const openProject = async (): Promise<void> => invoke('open_project');

export const generateSln = async (): Promise<void> => invoke('generate_sln');

export const openSln = async (): Promise<void> => invoke('open_sln');

export const forceDownloadDlls = async (): Promise<void> => invoke('force_download_dlls');

export const forceDownloadEngine = async (): Promise<void> => invoke('force_download_engine');

export const reinstallGitHooks = async (): Promise<void> => invoke('reinstall_git_hooks');

export const syncEngineCommitWithUproject = async (): Promise<string> =>
	invoke('sync_engine_commit_with_uproject');

export const syncUprojectWithEngineCommit = async (): Promise<string> =>
	invoke('sync_uproject_commit_with_engine');

export const acquireLocks = async (paths: string[], force: boolean): Promise<void> =>
	invoke('acquire_locks', { paths, force });

export const releaseLocks = async (paths: string[], force: boolean): Promise<void> =>
	invoke('release_locks', { paths, force });

export const getRebaseStatus = async (): Promise<RebaseStatusResponse> =>
	invoke('get_rebase_status');

export const fixRebase = async (): Promise<void> => invoke('fix_rebase');

export const rebase = async (): Promise<void> => invoke('rebase');

export const resetRepo = async (): Promise<void> => invoke('reset_repo');

export const showCommitFiles = async (
	commit: string,
	stash: boolean = false
): Promise<CommitFileInfo[]> => invoke('show_commit_files', { commit, stash });

export const getCommitFileTextClass = (action: string) => {
	if (action === 'M') {
		return 'text-yellow-300';
	}
	if (action === 'D') {
		return 'text-red-700';
	}
	if (action === 'A') {
		return 'text-lime-500';
	}
	return '';
};

export const getMergeQueue = async (): Promise<MergeQueue> => invoke('get_merge_queue');

export const checkoutCommit = async (commit: string): Promise<void> =>
	invoke('checkout_commit', { commit });
