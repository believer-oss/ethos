import { invoke } from '@tauri-apps/api/tauri';
import type { DynamicConfig, AppConfig, RepoConfig } from '$lib/types';

export const getDynamicConfig = async (): Promise<DynamicConfig> => invoke('get_dynamic_config');

export const getAppConfig = async (): Promise<AppConfig> => invoke('get_app_config');

export const updateAppConfig = async (config: AppConfig): Promise<string> =>
	invoke('update_app_config', { config });

export const getRepoConfig = async (): Promise<RepoConfig> => invoke('get_repo_config');
