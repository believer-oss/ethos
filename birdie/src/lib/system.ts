import { invoke } from '@tauri-apps/api/tauri';
import type { LogEvent } from '$lib/types';

export const getLatestVersion = async (): Promise<string> => invoke('get_latest_version');

export const runUpdate = async (): Promise<void> => invoke('run_update');

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
