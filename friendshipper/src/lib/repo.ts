import type { ChangeSet, Commit, CommitFileInfo } from '@ethos/core';
import { invoke } from '@tauri-apps/api/core';
import type {
	CloneRequest,
	CommitInfo,
	FileHistoryResponse,
	GitHubPullRequest,
	MergeQueue,
	ObjectCountResponse,
	PushRequest,
	RebaseStatusResponse,
	RepoDirectoryListing,
	RepoStatus,
	RevertFilesRequest,
	Snapshot
} from '$lib/types';

export const getCommits = async (
	limit?: number,
	remote?: boolean,
	update?: boolean
): Promise<Commit[]> => invoke('get_commits', { limit, remote, update });

export const getBranchComparison = async (limit?: number): Promise<Commit[]> =>
	invoke('get_branch_comparison', { limit });

// "All" here refers to the combination of local and upstream, not every commit.
// Right now we're just pulling 500.
export const getAllCommits = async (): Promise<Commit[]> => {
	let localCommits: Commit[];
	try {
		localCommits = await getCommits(500, false, false);
	} catch (_) {
		localCommits = [];
	}

	let remoteCommits: Commit[];
	try {
		remoteCommits = await getCommits(500, true, false);
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

export const saveChangeSet = async (changeSets: ChangeSet[]): Promise<void> =>
	invoke('save_changeset', { changeSets });

export const loadChangeSet = async (): Promise<ChangeSet[]> => invoke('load_changeset');

export const revertFiles = async (req: RevertFilesRequest): Promise<void> =>
	invoke('revert_files', { req });

export const getPullRequests = async (limit: number): Promise<GitHubPullRequest[]> =>
	invoke('get_pull_requests', { limit });

export interface SyncResponse {
	alreadyUpToDate: boolean;
}

export const syncLatest = async (): Promise<SyncResponse> => invoke('sync_latest');

export const openProject = async (): Promise<void> => invoke('open_project');

export const generateSln = async (): Promise<void> => invoke('generate_sln');

export const openSln = async (): Promise<void> => invoke('open_sln');

export const forceDownloadDlls = async (): Promise<void> => invoke('force_download_dlls');

export const forceDownloadEngine = async (): Promise<void> => invoke('force_download_engine');

export const resetEngine = async (): Promise<void> => invoke('reset_engine');

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

export const getObjectCount = async (): Promise<ObjectCountResponse> => invoke('get_object_count');

export const runGitGc = async (): Promise<void> => invoke('run_git_gc');

export const resetRepo = async (): Promise<void> => invoke('reset_repo');

export const refetchRepo = async (): Promise<void> => invoke('refetch_repo');

export const resetRepoToCommit = async (commit: string): Promise<void> =>
	invoke('reset_repo_to_commit', { commit });

export const showCommitFiles = async (
	commit: string,
	stash: boolean = false
): Promise<CommitFileInfo[]> => invoke('show_commit_files', { commit, stash });

export const getCommitFileTextClass = (action: string) => {
	// git --name-status emits single-letter codes for M/A/D/T/U and letter+similarity for R/C
	// (e.g. R080, C75). Match the leading letter so renames/copies color too.
	const code = action.charAt(0).toUpperCase();
	switch (code) {
		case 'M':
			return 'text-yellow-300';
		case 'D':
			return 'text-red-700';
		case 'A':
			return 'text-lime-500';
		case 'R':
			return 'text-sky-400';
		case 'C':
			return 'text-purple-400';
		case 'T':
			return 'text-orange-400';
		case 'U':
			return 'text-red-500';
		default:
			return '';
	}
};

export const getMergeQueue = async (): Promise<MergeQueue> => invoke('get_merge_queue');

export const checkoutTargetBranch = async (): Promise<void> => invoke('checkout_target_branch');

export const listRepoDirectory = async (path: string): Promise<RepoDirectoryListing> =>
	invoke('list_repo_directory', { path });

export const getFileHistory = async (path: string): Promise<FileHistoryResponse> =>
	invoke('get_file_history', { path });

export const getCommitInfo = async (sha: string): Promise<CommitInfo> =>
	invoke('get_commit_info', { sha });
