import type { ModifiedFile } from '@ethos/core';

export type Nullable<T> = T | null;

// Config types
export interface DiscordChannelInfo {
	name: string;
	url: string;
}

export interface DynamicConfig {
	maps: string[];
	playtestDiscordChannels: DiscordChannelInfo[];
	playtestRegions: string[];
}

export interface ProjectConfig {
	name: string;
	maps: string[];
}

export interface AWSConfig {
	accountId: string;
	ssoStartUrl: string;
	roleName: string;
	artifactBucketName: string;
}

export interface AppConfig {
	repoPath: string;
	repoUrl: string;
	userDisplayName: string;
	gameClientDownloadSymbols: boolean;
	pullDlls: boolean;
	editorDownloadSymbols: boolean;
	openUprojectAfterSync: boolean;
	githubPAT: string;
	engineType: string;
	enginePrebuiltPath: string;
	engineSourcePath: string;
	engineDownloadSymbols: boolean;
	engineRepoUrl: string;
	recordPlay: boolean;
	awsConfig: AWSConfig;
	selectedArtifactProject: string;
	playtestRegion: string;
	initialized: boolean;
	mobileURLScheme: string;
}

export interface RepoConfig {
	uprojectPath: string;
	trunkBranch: string;
	gitHooksPath: string;
	commitGuidelinesUrl?: string;
	useConventionalCommits: boolean;
	conventionalCommitsAllowedTypes: string[];
}

// Kubernetes API types
export interface Metadata {
	creationTimestamp: Nullable<string>;
	name: string;
	namespace: Nullable<string>;
	annotations: Nullable<Record<string, string>>;
	labels: Nullable<Record<string, string>>;
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
	netimguiPort: number;
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
	netimguiPort: number;
	version: string;
	creationTimestamp: string;
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
	serverRef?: LocalObjectReference;
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
	lockUser: string;
	locksOurs: Lock[];
	locksTheirs: Lock[];
}

export interface CommitAuthor {
	name: string;
}

export interface Commit {
	author: CommitAuthor;
	message: string;
}

export interface CommitMessage {
	type: string;
	scope: string;
	message: string;
}

export interface MergeQueueEntry {
	estimatedTimeToMerge: Nullable<number>;
	headCommit: Commit;
	enqueuedAt: string;
	state: 'QUEUED' | 'AWAITING_CHECKS' | 'LOCKED' | 'MERGEABLE' | 'UNMERGEABLE';
}

export interface PullRequestAuthor {
	login: string;
}

interface MergeQueueEntryConnection {
	nodes: MergeQueueEntry[];
}

export interface MergeQueue {
	url: string;
	entries: MergeQueueEntryConnection;
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

export interface RebaseStatusResponse {
	rebaseMergeExists: boolean;
	headNameExists: boolean;
}

export interface Snapshot {
	commit: string;
	message: string;
	timestamp: string;
}

// System types
export interface LogEvent {
	timestamp: string;
	level: 'DEBUG' | 'INFO' | 'WARN' | 'ERROR';
	fields: Record<string, string | number>;
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

// workflow types
export interface WorkflowOutputs {
	artifacts: Nullable<WorkflowArtifact[]>;
}

export interface WorkflowArtifact {
	name: string;
}

export interface WorkflowNode {
	id: string;
	displayName: string;
	phase: string;
	outputs: Nullable<WorkflowOutputs>;
}

export interface WorkflowStatus {
	phase: Nullable<string>;
	startedAt: Nullable<string>;
	finishedAt: Nullable<string>;
	estimatedDuration: Nullable<number>;
	progress: Nullable<string>;
	nodes: Map<string, WorkflowNode>;
}

export interface Workflow {
	metadata: Metadata;
	status: WorkflowStatus;
}

export interface CommitWorkflowInfo {
	creationTimestamp: string;
	message: Nullable<string>;
	compareUrl: Nullable<string>;
	commit: string;
	pusher: string;
	workflows: Workflow[];
}

export interface GetWorkflowsResponse {
	commits: CommitWorkflowInfo[];
}

export interface UnrealVersionSelectorStatus {
	valid_version_selector: boolean;
	version_selector_msg: string;
	uproject_file_assoc: boolean;
	uproject_file_assoc_msg: string[];
}
