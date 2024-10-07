import { invoke } from '@tauri-apps/api/tauri';
import type { LogEvent, UnrealVersionSelectorStatus } from '$lib/types';

export const restart = async (): Promise<void> => invoke('restart');

export const configureGitUser = async (name: string, email: string): Promise<void> =>
	invoke('configure_git_user', {
		name,
		email
	});

export const installGit = async (): Promise<void> => invoke('install_git');
export const getLogPath = async (): Promise<string> => invoke('get_log_path');

export const getLogs = async (): Promise<LogEvent[]> => invoke('get_logs');

export const openSystemLogsFolder = async (): Promise<void> => invoke('open_system_logs_folder');

export const openTerminalToPath = async (path: string): Promise<void> =>
	invoke('open_terminal_to_path', {
		path
	});

export const getUnrealVersionSelectorStatus = async (): Promise<UnrealVersionSelectorStatus> =>
	invoke('get_unrealversionselector_status');
