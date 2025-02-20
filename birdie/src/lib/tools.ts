import { invoke } from '@tauri-apps/api/core';

export const syncTools = async (): Promise<boolean> => invoke('sync_tools');
export const runSetEnv = async (): Promise<boolean> => invoke('run_set_env');
