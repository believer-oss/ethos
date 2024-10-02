import { invoke } from '@tauri-apps/api/tauri';
import type { Commit, CommitFileInfo } from '@ethos/core';
import type {
	CloneRequest,
	LFSFile,
	PushRequest,
	RebaseStatusResponse,
	RepoStatus,
	RevertFilesRequest,
	VerifyLocksResponse
} from '$lib/types';

export const getCommits = async (
	limit?: number,
	remote?: boolean,
	update?: boolean
): Promise<Commit[]> => invoke('get_commits', { limit, remote, update });

// "All" here refers to the combination of local and upstream, not every commit.
// Right now we're just pulling 250.
export const getAllCommits = async (): Promise<Commit[]> => {
	const remoteCommits: Commit[] = await getCommits(250, true, false);
	const localCommits: Commit[] = await getCommits(250, false, false);

	localCommits.forEach((localCommit) => {
		const index = remoteCommits.findIndex((v) => v.sha === localCommit.sha);

		if (index !== -1) {
			remoteCommits[index].local = true;
		} else {
			remoteCommits.push({ local: false, ...localCommit });
		}
	});

	return remoteCommits;
};

export const cloneRepo = async (req: CloneRequest): Promise<void> => {
	req.path = req.path.replace(/\\/g, '/');
	await invoke('clone_repo', { req });
};

export const getRepoStatus = async (skipDllCheck: boolean = false): Promise<RepoStatus> =>
	invoke('get_repo_status', { skipDllCheck });

export const submit = async (req: PushRequest): Promise<void> => invoke('submit', { req });

export const revertFiles = async (req: RevertFilesRequest): Promise<void> =>
	invoke('revert_files', { req });

export const syncLatest = async (): Promise<void> => invoke('sync_latest');
export const verifyLocks = async (): Promise<VerifyLocksResponse> => invoke('verify_locks');
export const lockFiles = async (paths: string[]): Promise<Lock> => invoke('lock_files', { paths });

export const unlockFiles = async (paths: string[], force: boolean): Promise<void> =>
	invoke('unlock_files', { paths, force });

export const getRebaseStatus = async (): Promise<RebaseStatusResponse> =>
	invoke('get_rebase_status');

export const fixRebase = async (): Promise<void> => invoke('fix_rebase');

export const rebase = async (): Promise<void> => invoke('rebase');

// Birdie
export const getAllFiles = async (): Promise<string[]> => invoke('get_all_files');
export const getFiles = async (root?: string): Promise<LFSFile[]> => invoke('get_files', { root });
export const getFile = async (path: string): Promise<LFSFile> => invoke('get_file', { path });
export const getFileHistory = async (file: string): Promise<Commit[]> =>
	invoke('get_file_history', { file });

export const showCommitFiles = async (
	commit: string,
	stash: boolean = false
): Promise<CommitFileInfo[]> => invoke('show_commit_files', { commit, stash });

export const downloadLFSFiles = async (files: string[], includeWip: boolean): Promise<void> =>
	invoke('download_lfs_files', { files, includeWip });

export const getFetchInclude = async (): Promise<string[]> => invoke('get_fetch_include');

export const delFetchInclude = async (files: string[]): Promise<void> =>
	invoke('del_fetch_include', { files });
