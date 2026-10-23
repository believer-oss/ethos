import type { Commit as CoreCommit, ModifiedFile } from '@ethos/core';

export type Nullable<T> = T | null;

// Error type
export interface TauriError {
	message: string;
	status_code: number;
}

// Okta types

export interface Credentials {
	id_token: string;
}

// Config types
export interface DiscordChannelInfo {
	name: string;
	url: string;
}

export interface GameServerCluster {
	displayName: string;
	clusterName: string;
	region: string;
}

export interface DynamicConfig {
	maps: string[];
	playtestDiscordChannels: DiscordChannelInfo[];
	playtestRegions: string[];
	profileDataPath: string;
	mobileURLScheme: string;
	promotableBuildShards?: PromoteBuildShard[];
	// Bucket holding promoted-build metadata objects. Overrides the server config and
	// the build-time constant, so changing buckets needs no Friendshipper release.
	promotedArtifactBucketName?: string;
	gameServerClusters?: GameServerCluster[];
	// Promote through the fleet coordinator (Promotion objects) instead of creating the
	// promote workflow directly from promotableBuildShards.
	promotionViaCoordinator?: boolean;
	commitTags?: CommitTagCategory[];
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

export interface OktaConfig {
	clientId: string;
	issuer: string;
}

export interface Project {
	repoPath: string;
	repoUrl: string;
}

export type SyncKind = 'client' | 'engine' | 'editorDlls';

export interface IncomingEngineChange {
	/** True only when a difference is visible. Not knowing looks the same as no change. */
	changes: boolean;
	current: string | null;
	incoming: string | null;
}

export type ArtifactState =
	| 'notInstalled'
	| 'outOfDate'
	| 'installed'
	| 'installedNoExpectation'
	/** Installed, but we cannot tell which version it should be. */
	| 'unknown';

export interface ArtifactStatus {
	kind: SyncKind;
	state: ArtifactState;
	/** What the checkout asks for. Null for the client, where you pick the build. */
	expected: string | null;
	/** What is on disk. */
	installed: string | null;
	location: string | null;
	syncedAt: string | null;
	expectationSource: string | null;
}

export interface VerifyResponse {
	status: ArtifactStatus;
	/** False when the version is wrong or missing - nothing was checked or changed. */
	checked: boolean;
	/** Files that did not match the build and were rewritten. Zero is healthy. */
	repaired: number;
	bytesWritten: number;
	message: string;
}

export interface AppConfig {
	projects: Record<string, Project>;
	repoPath: string;
	repoUrl: string;
	userDisplayName: string;
	groupDownloadedBuildsByPlaytest: boolean;
	gameClientDownloadSymbols: boolean;
	pullDlls: boolean;
	editorDownloadSymbols: boolean;
	openUprojectAfterSync: boolean;
	targetBranch: string;
	primaryBranch?: string;
	contentBranch?: string;
	githubPAT: string;
	engineType: string;
	enginePrebuiltPath: string;
	engineSourcePath: string;
	engineDownloadSymbols: boolean;
	engineRepoUrl: string;
	engineAllowMultipleProcesses: boolean;
	recordPlay: boolean;
	serverUrl: string;
	oktaConfig: OktaConfig;
	serverless: boolean;
	selectedArtifactProject: string;
	maxClientCacheSizeGb: number;
	maxEngineCacheSizeGb: number;
	maxEditorCacheSizeGb: number;
	s3TransferAcceleration: boolean;
	playtestRegion: string;
	initialized: boolean;
	lastQuickSubmitType?: string;
	lastQuickSubmitScope?: string;
	disableBackgroundGitOperations: boolean;
	releaseControlsEnabled?: boolean;
	showAllBranches: boolean;
}

export interface BranchInfo {
	name: string;
	sha: string;
	shortSha: string;
	commitDate: string;
	author: string;
	subject: string;
	message: string;
	isFeatureBranch: boolean;
	isProtected: boolean;
	isCurrent: boolean;
}

export interface CreateBranchResponse {
	branch: string;
	switched: boolean;
	message: Nullable<string>;
}

export interface SwitchBranchResponse {
	branch: string;
	warning: Nullable<string>;
}

export interface CommitTagOption {
	tag: string;
	label: string;
	description: string;
}

export interface CommitTagCategory {
	name: string;
	options: CommitTagOption[];
}

export interface PlaytestProfile {
	name: string;
	description: string;
	args: string;
}

export interface TargetBranchConfig {
	name: string;
	usesMergeQueue: boolean;
	blockedFileGlobs: string[];
}

export interface PromoteBuildShard {
	displayName: string;
	shard?: string;
	metadataPath?: string;
	// S3 key of the object holding this shard's deployed SHA. Separate from
	// metadataPath, the Argo workflow parameter.
	metadataObjectKey?: string;
	distribution?: string;
	gameConfig?: string;
	steamBranches?: string[];
	disableShardDeploy?: boolean;
}

export interface RepoConfig {
	uprojectPath: string;
	trunkBranch: string;
	targetBranches: TargetBranchConfig[];
	gitHooksPath: string;
	commitGuidelinesUrl?: string;
	useConventionalCommits: boolean;
	conventionalCommitsAllowedTypes: string[];
	playtestProfiles: PlaytestProfile[];
	buildsEnabled: boolean;
	serversEnabled: boolean;
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

export type ActiveBuildStatus = 'resolved' | 'tbd' | 'error';

export interface ActiveBuild {
	displayName: string;
	steamBranch?: string;
	sha?: string;
	deployedAt?: string;
	status: ActiveBuildStatus;
	error?: string;
}

export enum LaunchMode {
	WithServer = 'withServer',
	WithoutServer = 'withoutServer'
}

export interface LaunchOptions {
	name: string;
	launchMode: LaunchMode;
}

export interface SyncClientRequest {
	artifactEntry: ArtifactEntry;
	methodPrefix: string;
	subPath?: string;
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
	ready: boolean;
}

export interface LaunchRequest {
	commit: string;
	checkForExisting: boolean;
	displayName: string;
	map?: string;
	includeReadinessProbe: boolean;
	cmdArgs: string[];
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
	includeReadinessProbe: boolean;
	gameServerCmdArgs: Nullable<string[]>;
	disableGameServers?: boolean;
}

export interface GroupStatus extends Group {
	serverRef?: LocalObjectReference;
	ready: boolean;
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
	commitHead: string;
	commitsAhead: number;
	commitsBehind: number;
	commitsAheadOfTrunk: number;
	commitsBehindTrunk: number;
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
	willConflictOnRebase: string[];
	willConflictOnSync?: string[];
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

export interface GithubPullRequestCommit {
	oid: string;
	message: string;
}

export interface GithubPullRequestCommitNode {
	commit: GithubPullRequestCommit;
}

export interface GithubPullRequestCommits {
	nodes: GithubPullRequestCommitNode[];
}

export interface GitHubPullRequest {
	number: number;
	createdAt: string;
	mergedAt: Nullable<string>;
	merged: boolean;
	mergeable: 'CONFLICTING' | 'MERGEABLE' | 'UNKNOWN';
	mergeQueueEntry: Nullable<MergeQueueEntry>;
	author: PullRequestAuthor;
	commits: GithubPullRequestCommits;
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
	expectedBranch?: string;
}

export interface RevertFilesRequest {
	files: string[];
	skipEngineCheck: boolean;
	takeSnapshot: boolean;
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

export interface ObjectCountResponse {
	inPackCount: number;
	isHealthy: boolean;
	rawOutput: string;
	looseCount: number;
	lastPacked: string | null;
}

export interface GitHubStatusResponse {
	indicator: 'none' | 'minor' | 'major' | 'critical' | 'maintenance';
	description: string;
	url: string;
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
	Failure,
	/**
	 * Checked, and the answer is genuinely not knowable - as opposed to Loading, which
	 * will resolve on its own. Distinct so the icon can say "look inside" rather than
	 * either claiming a clean bill of health or implying it is still working.
	 */
	Unknown
}

// workflow types
export interface LogChunk {
	data: string;
	finished: boolean;
	error: Nullable<string>;
}

export interface WorkflowOutputs {
	artifacts: Nullable<WorkflowArtifact[]>;
}

export interface WorkflowArtifact {
	name: string;
	s3: Nullable<S3Artifact>;
}

export interface S3Artifact {
	key: string;
}

export interface WorkflowNode {
	id: string;
	displayName: string;
	type?: string;
	phase: string;
	startedAt?: string;
	outputs: Nullable<WorkflowOutputs>;
}

export interface WorkflowStatus {
	phase: Nullable<string>;
	startedAt: Nullable<string>;
	finishedAt: Nullable<string>;
	estimatedDuration: Nullable<number>;
	progress: Nullable<string>;
	nodes?: Record<string, WorkflowNode>;
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
	branch: Nullable<string>;
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

// Junit types
export interface JunitFailure {
	message: string;
	failure_type: string;
	$value: Nullable<string>;
}

export interface JunitTestCase {
	name: string;
	className: string;
	time: Nullable<number>;
	failure: Nullable<JunitFailure[]>;
}

export interface JunitOutput {
	time: Nullable<number>;
	testsuite: JunitTestSuite[];
}

export interface JunitTestSuite {
	name: string;
	time: Nullable<number>;
	testcase: Nullable<JunitTestCase[]>;
	testsuite: Nullable<JunitTestSuite[]>;
}

// Repository file browser types
export type RepoFileKind = 'directory' | 'file';

export type RepoFileState =
	| 'unmodified'
	| 'modified'
	| 'added'
	| 'untracked'
	| 'deleted'
	| 'outOfDate'
	| 'conflicted';

export interface RepoDirectoryEntry {
	name: string;
	path: string;
	kind: RepoFileKind;
	state: RepoFileState;
	size?: number;
}

export interface RepoDirectoryListing {
	path: string;
	entries: RepoDirectoryEntry[];
}

export interface FileHistoryRevision {
	filename: string;
	commitId: string;
	shortCommitId: string;
	commitIdNumber: number;
	revisionNumber: number;
	fileHash: string;
	description: string;
	userName: string;
	action: string;
	date: string;
	fileSize: number;
}

export interface FileHistoryResponse {
	displayName: string;
	revisions: FileHistoryRevision[];
}

export interface RestoreFileToRevisionRequest {
	path: string;
	sha: string;
	takeSnapshot?: boolean;
	skipEngineCheck?: boolean;
}

export interface CommitInfo {
	sha: string;
	shortSha: string;
	authorName: string;
	authorEmail: string;
	authorDate: string;
	committerName: string;
	committerEmail: string;
	committerDate: string;
	parents: string[];
	subject: string;
	message: string;
}

export interface ZipLocalChangesResponse {
	destination: string;
	fileCount: number;
}

export interface ZipPreviewEntry {
	path: string;
	displayName: string;
	state: 'Unknown' | 'Added' | 'Modified' | 'Deleted' | 'Unmerged';
	size: number;
	conflictsWithLocal: boolean;
	existsOnDisk: boolean;
}

export interface ZipPreviewResponse {
	source: string;
	createdBy: string | null;
	createdAt: string | null;
	entries: ZipPreviewEntry[];
}

export interface ImportZippedChangesResponse {
	extracted: number;
	deleted: number;
}

export interface SnapshotPreviewEntry {
	path: string;
	state: 'Unknown' | 'Added' | 'Modified' | 'Deleted' | 'Unmerged';
	conflictsWithLocal: boolean;
	existsOnDisk: boolean;
}

export interface SnapshotPreviewResponse {
	entries: SnapshotPreviewEntry[];
}

export interface TraceEntry {
	date: string;
	serverName: string;
	filename: string;
	key: string;
	size: number;
	lastModified: string | null;
}

export interface RecentTracesResponse {
	traces: TraceEntry[];
	nextCursor: string | null;
}

// Fleet objects (fleet.believer.dev/v1alpha1), as the fleet-api crate serializes them.
// Timestamps are RFC 3339 strings; optional status fields may be null.

export type ChannelKind = 'launcher' | 'steam';
export type GameConfig = 'Development' | 'Test' | 'Shipping';
export type PromotionPhase =
	| 'Pending'
	| 'Running'
	| 'Succeeded'
	| 'Failed'
	| 'Rejected'
	| 'Cancelled';

export interface ChannelRef {
	kind: ChannelKind;
	metadataPath?: string;
	branch?: string;
}

export interface ShardChannel extends ChannelRef {
	gameConfig: GameConfig;
}

export interface FleetCluster {
	name: string;
	zone: string;
}

export interface FleetsConfig {
	clusters: FleetCluster[];
	imageRepository: string;
	limits: { maxReplicasPerFleet?: number };
	// Durations are Go-style strings such as "2m"; unset fields take the coordinator's defaults.
	gate: { minReady?: number; stableFor?: string; timeout?: string };
	retireAfterIdle?: string;
	warmTtl?: string;
}

export interface ShardConfigSpec {
	shardName: string;
	displayName?: string;
	healthcheckUrl: string;
	channels: ShardChannel[];
	promotion: { allowDeployBackend: boolean };
	shardId?: string;
	backendAddress?: string;
	// Absent: the shard can never have Fleets.
	fleets?: Nullable<FleetsConfig>;
}

// An Agones Buffer policy, possibly partial. bufferSize is an integer or a percentage like "20%".
// parked: true scales the Fleet to zero with no autoscaler; the other fields are then ignored.
export interface Sizing {
	bufferSize?: number | string;
	minReplicas?: number;
	maxReplicas?: number;
	parked?: boolean;
}

export interface SizingOverride {
	cluster: string;
	mapId: string;
	sizing: Sizing;
}

export interface LiveSizing {
	default?: Sizing;
	maps?: Record<string, Sizing>;
	overrides?: SizingOverride[];
}

// The shard's Fleet settings, edited in Friendshipper.
export interface ShardSpec {
	fleetsEnabled: boolean;
	maps?: string[];
	warm?: Nullable<Sizing>;
	live?: LiveSizing;
}

export type FleetReleasePhase =
	| 'Warming'
	| 'Warm'
	| 'Live'
	| 'Draining'
	| 'Retiring'
	| 'Retired'
	| 'Failed';

export interface ReleaseSummary {
	name: string;
	version: string;
	phase?: Nullable<FleetReleasePhase>;
	servedBy: string[];
	ready: number;
	allocated: number;
}

export interface BackendParts {
	platform?: Nullable<string>;
	portal?: Nullable<string>;
	content?: Nullable<string>;
	config?: Nullable<string>;
}

export interface BackendStatus {
	healthy: boolean;
	parts: BackendParts;
	// When the backend process last started (a deploy, crash, OOM or reschedule): not a deploy time.
	launchedAt?: Nullable<string>;
	// The last successful observation; a failed one leaves it and sets error.
	observedAt?: Nullable<string>;
	error: string;
}

export interface ChannelStatus {
	key: string;
	kind: ChannelKind;
	metadataPath?: string;
	branch?: string;
	version: string;
	// When this version arrived (or the promotion that set it was recorded). Launcher: the pointer's
	// S3 LastModified. Steam: when the coordinator first saw the build on the branch, lagging by up
	// to a poll, or its first observation for a build already live then.
	since?: Nullable<string>;
	source?: Nullable<'observed' | 'promotion'>;
	// The last successful observation; a failed one leaves it and sets error.
	observedAt?: Nullable<string>;
	error: string;
	promotion?: Nullable<string>;
	buildId?: Nullable<number>;
	gameConfig?: Nullable<GameConfig>;
	// When the build was uploaded to Steam, not when it was set live on the branch.
	buildCreatedAt?: Nullable<string>;
}

export interface PromotionSummary {
	name: string;
	channel: string;
	version: string;
	phase?: Nullable<PromotionPhase>;
	startedAt?: Nullable<string>;
	finishedAt?: Nullable<string>;
}

export interface K8sCondition {
	type: string;
	status: string;
	reason: string;
	message: string;
	lastTransitionTime?: string;
}

export interface ShardStatus {
	observedGeneration?: Nullable<number>;
	backend?: Nullable<BackendStatus>;
	channels: ChannelStatus[];
	promotions: PromotionSummary[];
	releases?: ReleaseSummary[];
	conditions: K8sCondition[];
}

export interface ShardSummary {
	name: string;
	displayName: string;
	config: Nullable<ShardConfigSpec>;
	// Null while there is no Shard object.
	spec: Nullable<ShardSpec>;
	status: Nullable<ShardStatus>;
	// The Shard's generation, sent back with a spec edit so it can't overwrite a newer one. Status
	// writes don't change it.
	generation: Nullable<number>;
	// The live sizing under spec.live.default: the coordinator's default, its max capped at the
	// shard's limit. Null without a fleets section.
	liveDefault?: Nullable<Sizing>;
}

export interface PromotionSpec {
	shard: string;
	channel: ChannelRef;
	version: string;
	gameConfig?: GameConfig;
	deployBackend: boolean;
	sign?: boolean;
	// Don't wait for the warm gate; the release is still created.
	skipFleetGate?: { reason: string };
}

// deploy-backend, warm-fleets, publish-client, flip-channel; or the single `promote` step of a
// promotion run as one workflow.
export interface PromotionStep {
	name: string;
	// Empty for steps the coordinator runs itself (warm-fleets) and for skipped steps.
	workflow: string;
	fleetRelease?: Nullable<string>;
	// Argo's phase, or Pending / Skipped.
	phase: string;
	startedAt?: Nullable<string>;
	finishedAt?: Nullable<string>;
	message: string;
	outputs?: Record<string, string>;
}

export type PromotionChangeKind =
	| 'BackendDeployed'
	| 'FleetReleaseWarming'
	| 'ClientUploaded'
	| 'ChannelFlipped'
	| 'FleetReleaseLive';

export interface PromotionChange {
	kind: PromotionChangeKind;
	detail: string;
	at?: Nullable<string>;
}

export interface BackendPartChange {
	name: string;
	from: string;
	to: string;
}

export interface PromotionStatus {
	phase?: Nullable<PromotionPhase>;
	reason: string;
	message: string;
	previousVersion?: Nullable<string>;
	// Resolved at acceptance: spec.gameConfig, else the channel default.
	gameConfig?: Nullable<GameConfig>;
	plan?: Nullable<{
		backend?: Nullable<{
			deploy: boolean;
			parts: BackendPartChange[];
			// When the `from` values were read: a healthcheck at acceptance, or the Shard's last
			// successful observation if that read failed.
			observedAt?: Nullable<string>;
		}>;
	}>;
	// The running step; empty when nothing runs.
	currentStep?: string;
	fleetRelease?: Nullable<string>;
	steps: PromotionStep[];
	// Each effect outside the cluster, in order, for every outcome.
	changes?: PromotionChange[];
	startedAt?: Nullable<string>;
	finishedAt?: Nullable<string>;
	conditions: K8sCondition[];
}

// A request to cancel, from the Promotion's cancel annotations.
export interface PromotionCancel {
	// Blank or missing annotation: unknown.
	requestedBy: Nullable<string>;
	// As written: RFC 3339 UTC when Friendshipper set it, but any value counts as a request.
	requestedAt: string;
}

export interface Promotion {
	metadata: {
		name?: string;
		creationTimestamp?: string;
		labels?: Record<string, string>;
		annotations?: Record<string, string>;
	};
	spec: PromotionSpec;
	status?: Nullable<PromotionStatus>;
}

export type GatePhase = 'Pending' | 'Passed' | 'Failed';

export interface ReleaseClusterMap {
	mapId: string;
	fleet: string;
	policy: string;
	desired: number;
	ready: number;
	allocated: number;
}

export interface ReleaseCluster {
	name: string;
	gate?: Nullable<GatePhase>;
	maps: ReleaseClusterMap[];
	observedAt?: Nullable<string>;
	error: string;
	// Set once retiring has waited longer than cluster mode's --retire-stuck-after for its matches to
	// end; `since` is when it was flagged. Kept as last seen while the cluster is unreachable.
	retireStuck?: Nullable<{ since: string; message: string }>;
}

// Against the build manifest; anything but OK fails the release.
export type MapCheck = 'OK' | 'NotInBuild' | 'NotCooked';

export interface ReleaseMapCheck {
	mapId: string;
	check: MapCheck;
	levelUrl: string;
}

export interface FleetRelease {
	metadata: {
		name?: string;
		creationTimestamp?: string;
		// Set once the release is being deleted; it goes when the coordinator removes its finalizer.
		deletionTimestamp?: string;
		// Bumped by spec changes only; an overrides edit is sent with it.
		generation?: number;
		labels?: Record<string, string>;
		annotations?: Record<string, string>;
	};
	spec: {
		shard: string;
		version: string;
		phase: 'Warm' | 'Live' | 'Retired';
		pinned: boolean;
		overrides?: SizingOverride[];
		force: boolean;
	};
	status?: Nullable<{
		phase?: Nullable<FleetReleasePhase>;
		// When phase last changed.
		phaseSince?: Nullable<string>;
		servedBy: string[];
		warmUntil?: Nullable<string>;
		lastAllocationAt?: Nullable<string>;
		idleSince?: Nullable<string>;
		// Every map the build has a level for (its manifest's maps.levelAssets keys), sorted.
		buildMapIds?: string[];
		maps: ReleaseMapCheck[];
		clusters: ReleaseCluster[];
		conditions: K8sCondition[];
	}>;
}

export interface ShardDetail extends ShardSummary {
	promotions: Promotion[];
	releases: FleetRelease[];
}

// From the build's manifest, for the promote form (G6) and the map list (G8).
export interface BuildManifest {
	backend: BackendParts;
	// maps.levelAssets keys, sorted.
	mapIds: string[];
}

export interface PromotionPlan {
	// Null for a build with no readable manifest (older builds).
	manifest: Nullable<BuildManifest>;
	// The shard's backend as last observed.
	shardBackend: Nullable<BackendParts>;
}

export interface CreatePromotionRequest {
	spec: PromotionSpec;
	note?: string;
export type BranchUpstream = 'present' | 'gone' | 'notPushed';

export type BranchLocalSync = 'inSync' | 'ahead' | 'behind' | 'diverged' | 'unknown';

export interface FileTouch {
	sha: string;
	author: string;
	date: string;
}

export interface LockableFileRow {
	path: string;
	displayName: string;
	branchCommit: Nullable<FileTouch>;
	changedOnMain: boolean;
	mainCommit: Nullable<FileTouch>;
}

export interface BranchMergeQueueEntry {
	state: string;
	position: number;
}

export interface BranchPullRequest {
	number: number;
	id: string;
	url: string;
	headRefOid: string;
	baseRefName: string;
	headRepository: Nullable<string>;
	state: 'OPEN' | 'MERGED';
	mergeable: string;
	mergeQueueEntry: Nullable<BranchMergeQueueEntry>;
}

export interface BranchAnalysis {
	gitSupported: boolean;
	gitVersion: string;
	branch: string;
	trunkBranch: string;
	usesMergeQueue: boolean;
	upstream: BranchUpstream;
	localSync: BranchLocalSync;
	localAheadOfOrigin: number;
	localBehindOrigin: number;
	commitsAhead: number;
	commitsBehind: number;
	mergeBase: Nullable<string>;
	predictedConflicts: string[];
	lockableFiles: LockableFileRow[];
	lockableFileCount: number;
	commits: CoreCommit[];
	pr: Nullable<BranchPullRequest>;
	prError: Nullable<string>;
}

export interface CreateBranchPullRequestRequest {
	title: string;
}

export interface BranchPullRequestResult {
	number: number;
	url: string;
	created: boolean;
	alreadyQueued: boolean;
}

export interface BranchCleanupPr {
	number: number;
	url: string;
	baseRefName: string;
}

export interface BranchCleanupCommit {
	sha: string;
	subject: string;
}

export interface BranchCleanupCandidate {
	name: string;
	sha: string;
	subject: string;
	author: string;
	date: string;
	mergedIntoTrunk: boolean;
	patchEquivalent: boolean;
	mergedViaPr: Nullable<BranchCleanupPr>;
	prUnknown: boolean;
	unmergedCommitCount: number;
	unmergedCommits: BranchCleanupCommit[];
	safe: boolean;
}

export interface BranchCleanupCandidates {
	trunkBranch: string;
	candidates: BranchCleanupCandidate[];
	prError: Nullable<string>;
}

export interface BranchCleanupTarget {
	name: string;
	sha: string;
	// Safety the user saw; a branch safe by git checks is rechecked before deletion.
	safe: boolean;
	safeViaPr: boolean;
}

export interface BranchCleanupRequest {
	branches: BranchCleanupTarget[];
}

export interface BranchCleanupResult {
	name: string;
	sha: string;
	deleted: boolean;
	error: Nullable<string>;
}
