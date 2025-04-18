import { invoke } from '@tauri-apps/api/core';

export const openUrlForPath = async (path: string) => invoke('open_url_for_path', { path });
export const checkEngineReady = async (): Promise<boolean> => invoke('check_engine_ready');
