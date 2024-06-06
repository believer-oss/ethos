import type { ModifiedFile } from '@ethos/core';

export type Nullable<T> = T | null;

// Config types
export interface DiscordChannelInfo {
	name: string;
	url: string;
}

export interface DynamicConfig {
	playtestDiscordChannels: DiscordChannelInfo[];
}

export interface AppConfig {
	repoPath: string;
	repoUrl: string;
	userDisplayName: string;
	pullDlls: boolean;
	openUprojectAfterSync: boolean;
	githubPAT: string;
	engineType: string;
	enginePrebuiltPath: string;
	engineSourcePath: string;
	recordPlay: boolean;
	initialized: boolean;
}

export interface RepoConfig {
	uprojectPath: string;
	trunkBranch: string;
	gitHooksPath: string;
}

// Kubernetes API types
export interface Metadata {
	creationTimestamp: Nullable<string>;
	name: string;
	namespace: Nullable<string>;
	labels: Nullable<Map<string, string>>;
	uid: string;
}

export interface LocalObjectReference {
	name: string;
}

// Builds types
export interface ArtifactEntry {
	key: string;
	displayName: string;
	lastModified: number;
	commit: string;
}

export interface ArtifactListResponse {
	methodPrefix: string;
	entries: ArtifactEntry[];
}

export interface LaunchOptions {
	ip: string;
	port: number;
}

export interface SyncClientRequest {
	artifactEntry: ArtifactEntry;
	methodPrefix: string;
	launchOptions?: LaunchOptions;
}

// GameServer types

export interface GameServerResult {
	name: string;
	displayName: string;
	ip: string;
	port: number;
	version: string;
}

export interface LaunchRequest {
	commit: string;
	checkForExisting: boolean;
	displayName: string;
	map?: string;
}

// Playtest types
export interface Group {
	name: string;
	users: Nullable<string[]>;
}

export interface PlaytestSpec {
	version: string;
	map: string;
	displayName: string;
	minGroups: number;
	playersPerGroup: number;
	startTime: string;
	feedbackURL: string;
	groups?: Nullable<Group[]>;
}

export interface GroupStatus extends Group {
	serverRef: LocalObjectReference;
}

export interface PlaytestStatus {
	groups: GroupStatus[];
}

export interface Playtest {
	metadata: Metadata;
	spec: PlaytestSpec;
	status: Nullable<PlaytestStatus>;
}

export interface AssignUserRequest {
	playtest: string;
	user: string;
	group?: Nullable<string>;
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

export interface MergeQueueEntry {
	estimatedTimeToMerge: Nullable<number>;
	state: 'QUEUED' | 'AWAITING_CHECKS' | 'LOCKED' | 'MERGEABLE' | 'UNMERGEABLE';
}

export interface PullRequestAuthor {
	login: string;
}

export interface GitHubPullRequest {
	number: number;
	createdAt: string;
	mergedAt: Nullable<string>;
	merged: boolean;
	mergeable: 'CONFLICTING' | 'MERGEABLE' | 'UNKNOWN';
	mergeQueueEntry: Nullable<MergeQueueEntry>;
	author: PullRequestAuthor;
	permalink: string;
	title: string;
	state: 'CLOSED' | 'MERGED' | 'OPEN';
	headRefName: string;
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
}

export interface LockOwnerInfo {
	name: string;
}

export interface Lock {
	id: string;
	path: string;
	locked_at: string;
	owner: Nullable<LockOwnerInfo>;
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

// events

export interface QuickLaunchEvent {
	artifactEntry: ArtifactEntry;
	server: GameServerResult;
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
