import { invoke } from '@tauri-apps/api/core';
import type { GameServerResult, LaunchRequest } from '$lib/types';

export const getServer = async (name: string): Promise<GameServerResult> =>
	invoke('get_server', { name });

export const getServers = async (commit?: string): Promise<GameServerResult[]> =>
	invoke('get_servers', { commit });

export const launchServer = async (req: LaunchRequest): Promise<void> =>
	invoke('launch_server', { req });

export const terminateServer = async (name: string): Promise<void> =>
	invoke('terminate_server', { name });

export const downloadServerLogs = async (name: string): Promise<void> =>
	invoke('download_server_logs', { name });

export const openLogsFolder = async (): Promise<void> => invoke('open_logs_folder');

export const startLogTail = async (name: string): Promise<void> =>
	invoke('start_gameserver_log_tail', { name });

export const stopLogTail = async (): Promise<void> => invoke('stop_gameserver_log_tail');

export const copyProfileDataFromGameserver = async (name: string): Promise<void> =>
	invoke('copy_profile_data_from_gameserver', { name });

export const getServerArgsDisplayString = (args: string): string => {
	if (args === '') {
		return '';
	}
	return `(${args})`;
};
