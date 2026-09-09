import { invoke } from '@tauri-apps/api/core';
import type {
	GetWorkflowsResponse,
	ArtifactListResponse,
	SyncClientRequest,
	JunitOutput,
	ArtifactEntry,
	ActiveBuild,
	Workflow
} from '$lib/types';

export const getBuild = async (commit: string, project?: string): Promise<ArtifactEntry> =>
	invoke('get_build', { commit, project });

export const getBuilds = async (limit?: number, project?: string): Promise<ArtifactListResponse> =>
	invoke('get_builds', { limit, project });

export const getActiveBuilds = async (): Promise<ActiveBuild[]> => invoke('get_active_builds');

export const syncClient = async (req: SyncClientRequest): Promise<boolean> =>
	invoke('sync_client', { req });

export const cancelDownload = async (): Promise<void> => invoke('cancel_download');

export const wipeClientData = async (): Promise<void> => invoke('wipe_client_data');

export const resetLongtail = async (): Promise<void> => invoke('reset_longtail');

export const getWorkflows = async (
	engine: boolean = false,
	project?: string
): Promise<GetWorkflowsResponse> => invoke('get_workflows', { engine, project });

export const getWorkflowNodes = async (name: string): Promise<Workflow> =>
	invoke('get_workflow_nodes', { name });

export const getWorkflowJunitArtifact = async (
	uid: string,
	nodeId: string
): Promise<JunitOutput | null> => invoke('get_workflow_junit_artifact', { uid, nodeId });

export const getWorkflowNodeLogs = async (workflowName: string, nodeId: string): Promise<string> =>
	invoke('get_workflow_node_logs', { workflowName, nodeId });

export const stopWorkflow = async (workflow: string): Promise<string> =>
	invoke('stop_workflow', { workflow });

export interface CreatePromoteBuildWorkflowRequest {
	commit: string; // required
	// Backend environment. Key stays `shard`: it is the Argo template's
	// parameter name and the wire field shared with friendshipper-server.
	shard?: string; // optional, from repo config
	metadata_path?: string; // optional, from repo config
	pusher?: string; // optional, github username or playtest username
	distribution?: string; // optional, e.g. "steam"
	steam_branch?: string; // optional, Steam branch name
	game_config?: string; // optional, defaults to "development" server-side
}

export const createPromoteBuildWorkflow = async (
	request: CreatePromoteBuildWorkflowRequest
): Promise<Workflow> => invoke('create_promote_build_workflow', { request });

export const startWorkflowLogTail = async (workflowName: string, nodeId: string): Promise<void> =>
	invoke('start_workflow_log_tail', { workflowName, nodeId });

export const stopWorkflowLogTail = async (): Promise<void> => invoke('stop_workflow_log_tail');

// Shortest prefix treated as a match; below this collisions stop being negligible.
const MIN_SHA_MATCH_LENGTH = 7;

/**
 * True when two commit identifiers refer to the same commit.
 *
 * Prefix rather than equality: the workflow list carries full SHAs while the metadata
 * object holds whatever the pipeline wrote, which may be abbreviated.
 */
const isSameCommit = (a: string, b: string): boolean => {
	const left = a.trim().toLowerCase();
	const right = b.trim().toLowerCase();
	if (left.length < MIN_SHA_MATCH_LENGTH || right.length < MIN_SHA_MATCH_LENGTH) return false;
	return left.startsWith(right) || right.startsWith(left);
};

/**
 * Destination names this commit is deployed to, in configured order.
 *
 * Only `resolved` rows count; Steam rows carry no SHA, so a configured Steam branch is
 * never reported as promoted.
 */
export const promotedDestinationsFor = (commit: string, rows: ActiveBuild[]): string[] => {
	if (!commit) return [];
	return rows
		.filter((row) => row.status === 'resolved' && !!row.sha && isSameCommit(commit, row.sha))
		.map((row) => row.displayName);
};
