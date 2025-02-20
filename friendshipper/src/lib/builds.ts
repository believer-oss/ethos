import { invoke } from '@tauri-apps/api/core';
import type {
	GetWorkflowsResponse,
	ArtifactListResponse,
	SyncClientRequest,
	JunitOutput,
	ArtifactEntry
} from '$lib/types';

export const getBuild = async (commit: string, project?: string): Promise<ArtifactEntry> =>
	invoke('get_build', { commit, project });

export const getBuilds = async (limit?: number, project?: string): Promise<ArtifactListResponse> =>
	invoke('get_builds', { limit, project });

export const syncClient = async (req: SyncClientRequest): Promise<void> =>
	invoke('sync_client', { req });

export const wipeClientData = async (): Promise<void> => invoke('wipe_client_data');

export const resetLongtail = async (): Promise<void> => invoke('reset_longtail');

export const getWorkflows = async (engine: boolean = false): Promise<GetWorkflowsResponse> =>
	invoke('get_workflows', { engine });

export const getWorkflowJunitArtifact = async (
	uid: string,
	nodeId: string
): Promise<JunitOutput | null> => invoke('get_workflow_junit_artifact', { uid, nodeId });

export const getWorkflowNodeLogs = async (uid: string, nodeId: string): Promise<string> =>
	invoke('get_workflow_node_logs', { uid, nodeId });

export const stopWorkflow = async (workflow: string): Promise<string> =>
	invoke('stop_workflow', { workflow });
