import { invoke } from '@tauri-apps/api/core';

export const openUrlForPath = async (path: string) => invoke('open_url_for_path', { path });
