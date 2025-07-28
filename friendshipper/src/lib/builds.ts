import { invoke } from '@tauri-apps/api/core';
import { invokeWithAuth } from '$lib/http';
import type {
	GetWorkflowsResponse,
	ArtifactListResponse,
	SyncClientRequest,
	JunitOutput,
	ArtifactEntry
} from '$lib/types';

export const getBuild = async (commit: string, project?: string): Promise<ArtifactEntry> =>
	invokeWithAuth('get_build', { commit, project });

export const getBuilds = async (limit?: number, project?: string): Promise<ArtifactListResponse> =>
	invokeWithAuth('get_builds', { limit, project });

export const syncClient = async (req: SyncClientRequest): Promise<boolean> =>
	invokeWithAuth('sync_client', { req });

export const cancelDownload = async (): Promise<void> => invoke('cancel_download');

export const wipeClientData = async (): Promise<void> => invoke('wipe_client_data');

export const resetLongtail = async (): Promise<void> => invoke('reset_longtail');

export const getWorkflows = async (engine: boolean = false): Promise<GetWorkflowsResponse> =>
	invokeWithAuth('get_workflows', { engine });

export const getWorkflowJunitArtifact = async (
	uid: string,
	nodeId: string
): Promise<JunitOutput | null> => invokeWithAuth('get_workflow_junit_artifact', { uid, nodeId });

export const getWorkflowNodeLogs = async (uid: string, nodeId: string): Promise<string> =>
	invokeWithAuth('get_workflow_node_logs', { uid, nodeId });

export const stopWorkflow = async (workflow: string): Promise<string> =>
	invokeWithAuth('stop_workflow', { workflow });
