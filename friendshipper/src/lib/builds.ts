import { invoke } from '@tauri-apps/api/core';
import type {
	GetWorkflowsResponse,
	ArtifactListResponse,
	SyncClientRequest,
	JunitOutput,
	ArtifactEntry,
	Workflow
} from '$lib/types';

export const getBuild = async (commit: string, project?: string): Promise<ArtifactEntry> =>
	invoke('get_build', { commit, project });

export const getBuilds = async (limit?: number, project?: string): Promise<ArtifactListResponse> =>
	invoke('get_builds', { limit, project });

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
