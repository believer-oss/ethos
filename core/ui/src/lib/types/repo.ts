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

export interface ModifiedFile {
	path: string;
	displayName: string;
	state: ModifiedFileState;
	isStaged: boolean;
	lockedBy: string;
	submitStatus: SubmitStatus;
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
	displayName: string;
}
