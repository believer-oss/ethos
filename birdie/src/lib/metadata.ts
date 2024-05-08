import { invoke } from '@tauri-apps/api/tauri';
import type { DirectoryMetadata } from '$lib/types';

export const getDirectoryMetadata = async (path: string): Promise<DirectoryMetadata> =>
	invoke('get_directory_metadata', { path });

export const updateDirectoryMetadata = async (
	path: string,
	metadata: DirectoryMetadata
): Promise<DirectoryMetadata> => invoke('update_metadata', { path, metadata });
export const updateMetadataClass = async (path: string, directoryClass: string): Promise<void> =>
	invoke('update_metadata_class', { path, directoryClass });
