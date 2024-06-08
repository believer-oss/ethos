export interface ModifiedFile {
	path: string;
	displayName: string;
	indexState: string;
	workingState: string;
}

export enum ModifiedFileState {
	Added = 'added',
	Modified = 'modified',
	Deleted = 'deleted',
	Unknown = 'unknown'
}

export interface Commit {
	sha: string;
	author: string;
	message: string;
	timestamp: string;
	local?: boolean;
}

export interface CommitFileInfo {
	action: string;
	file: string;
}
