import type { ModifiedFile } from '@ethos/core';

export type Nullable<T> = T | null;

// Config types
export interface BirdieConfig {
	repoPath: string;
	repoUrl: string;
	toolsPath: string;
	toolsUrl: string;
	userDisplayName: string;
	githubPAT: string;
	initialized: boolean;
}

// Repo types
export interface PullRequestStatus {
	number: number;
	merged_at?: string;
}

export interface RepoStatus {
	detachedHead: boolean;
	lastUpdated: string;
	operationInProgress: boolean;
	lastPushSucceeded: boolean;
	lastSyncSucceeded: boolean;
	outOfDate: boolean;
	branch: string;
	remoteBranch: string;
	repoOwner: string;
	repoName: string;
	commitsAhead: number;
	commitsBehind: number;
	commitHeadOrigin: string;
	originHasNewDlls: boolean;
	pullDlls: boolean;
	dllCommitLocal: string;
	dllArchiveForLocal: string;
	dllCommitRemote: string;
	dllArchiveForRemote: string;
	untrackedFiles: ModifiedFile[];
	modifiedFiles: ModifiedFile[];
	hasStagedChanges: boolean;
	hasLocalChanges: boolean;
	conflictUpstream: boolean;
	conflicts: string[];
	modifiedUpstream: string[];
	lastPullRequest?: PullRequestStatus;
}

export interface CloneRequest {
	url: string;
	path: string;
}

export interface PushRequest {
	commitMessage: string;
	files: string[];
}

export interface RevertFilesRequest {
	files: string[];
	skipEngineCheck: boolean;
}

export interface LockOwnerInfo {
	name: string;
}

export interface Lock {
	id: string;
	path: string;
	locked_at: string;
	owner: Nullable<LockOwnerInfo>;
	display_name: Nullable<string>;
}

export interface VerifyLocksResponse {
	ours: Lock[];
	theirs: Lock[];
	nextCursor: Nullable<string>;
}

export interface RebaseStatusResponse {
	rebaseMergeExists: boolean;
	headNameExists: boolean;
}

// System types
export interface LogEvent {
	timestamp: string;
	level: 'DEBUG' | 'INFO' | 'WARN' | 'ERROR';
	fields: Record<string, string>;
	target: string;
}

// diagnostic status checks

export enum CheckStatus {
	Loading,
	Success,
	Failure
}

// Birdie types
export enum FileType {
	File = 'File',
	Directory = 'Directory'
}

export enum LocalFileLFSState {
	None = 'None',
	Local = 'Local',
	Stub = 'Stub',
	Untracked = 'Untracked'
}

export interface LockCacheEntry {
	lock: Lock;
	ours: boolean;
}

export interface LFSFile {
	path: string;
	name: string;
	size: number;
	fileType: FileType;
	lfsState: LocalFileLFSState;
	locked: boolean;
	lockInfo: Nullable<LockCacheEntry>;
}

export enum DirectoryClass {
	None = 'none',
	Character = 'character'
}

export interface CharacterMetadata {
	codeName: string;
	characterName: string;
	rigs: Record<string, string>;
	animations: Record<string, string>;
	meshes: Record<string, string>;
}

export interface DirectoryMetadata {
	directoryClass: DirectoryClass;
	character: Nullable<CharacterMetadata>;
}

export interface Node {
	value: LFSFile;
	children: Node[];
}
