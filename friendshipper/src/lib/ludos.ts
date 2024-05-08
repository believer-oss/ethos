import { invoke } from '@tauri-apps/api/tauri';

import type {
	LudosGetResponse,
	LudosPutResponse,
	LudosListResponse,
	LudosDeleteResponse
} from '$lib/types';

export const ludosGet = async (key: string): Promise<LudosGetResponse> =>
	invoke('ludos_get', { key });

export const ludosPut = async (key: string, jsonData: string): Promise<LudosPutResponse> =>
	invoke('ludos_put', { key, jsonData });

export const ludosList = async (filter: string): Promise<LudosListResponse> =>
	invoke('ludos_list', { filter });

export const ludosDelete = async (keys: string[]): Promise<LudosDeleteResponse> =>
	invoke('ludos_delete', { keys });
