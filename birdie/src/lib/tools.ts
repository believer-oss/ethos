import { invoke } from '@tauri-apps/api/tauri';

export const syncTools = async (): Promise<boolean> => invoke('sync_tools');
