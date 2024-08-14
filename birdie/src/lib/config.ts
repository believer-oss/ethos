import { invoke } from '@tauri-apps/api/tauri';
import type { AppConfig } from '$lib/types';

export const getAppConfig = async (): Promise<AppConfig> => invoke('get_app_config');

export const updateAppConfig = async (config: AppConfig): Promise<string> =>
	invoke('update_app_config', { config });
