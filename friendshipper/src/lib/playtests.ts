import { invoke } from '@tauri-apps/api/tauri';
import type { AssignUserRequest, GroupStatus, Nullable, Playtest, PlaytestSpec } from '$lib/types';

export const getPlaytests = async (): Promise<Playtest[]> => invoke('get_playtests');

export enum ModalState {
	Creating,
	Editing
}

export const createPlaytest = async (
	name: string,
	project: string,
	spec: PlaytestSpec
): Promise<void> => {
	const req = {
		name,
		project,
		spec
	};
	await invoke('create_playtest', { req });
};

export const updatePlaytest = async (
	playtest: string,
	project: string,
	spec: PlaytestSpec
): Promise<void> => {
	const req = {
		project,
		spec
	};
	await invoke('update_playtest', { playtest, req });
};

export const deletePlaytest = async (playtest: string): Promise<void> => {
	await invoke('delete_playtest', { playtest });
};

export const assignUserToGroup = async (req: AssignUserRequest): Promise<void> => {
	await invoke('assign_user_to_group', { req });
};
export const unassignUserFromPlaytest = async (playtest: string, user: string): Promise<void> => {
	const req = {
		playtest,
		user
	};
	await invoke('unassign_user_from_playtest', { req });
};

export const getPlaytestGroupForUser = (
	playtest: Nullable<Playtest>,
	user: string
): Nullable<GroupStatus> =>
	playtest?.status?.groups.find((group) => group?.users?.includes(user)) ?? null;
