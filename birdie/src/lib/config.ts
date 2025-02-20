import { invoke } from '@tauri-apps/api/core';
import type { BirdieConfig } from '$lib/types';

export const getAppConfig = async (): Promise<BirdieConfig> => invoke('get_config');

export const updateAppConfig = async (config: BirdieConfig): Promise<string> =>
	invoke('update_config', { config });
