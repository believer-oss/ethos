export enum ModifiedFileState {
	Added = 'Added',
	Modified = 'Modified',
	Deleted = 'Deleted',
	Unmerged = 'Unmerged',
	Unknown = 'Unknown'
}

export enum SubmitStatus {
	Ok = 'Ok',
	CheckoutRequired = 'CheckoutRequired',
	CheckedOutByOtherUser = 'CheckedOutByOtherUser',
	Unmerged = 'Unmerged',
	Conflicted = 'Conflicted'
}

export enum SortKey {
	FileName = 'FileName',
	LockStatus = 'LockStatus',
	FileState = 'FileState'
}

export interface ModifiedFile {
	path: string;
	displayName: string;
	state: ModifiedFileState;
	isStaged: boolean;
	lockedBy: string;
	submitStatus: SubmitStatus;
	url?: string;
}

export interface ChangeSet {
	name: string;
	files: ModifiedFile[];
	open: boolean;
	checked: boolean;
	indeterminate: boolean;
}

export interface Commit {
	sha: string;
	author: string;
	message: string;
	timestamp: string;
	local?: boolean;
	status?: string;
}

export interface CommitFileInfo {
	action: string;
	file: string;
	displayName: string;
}
