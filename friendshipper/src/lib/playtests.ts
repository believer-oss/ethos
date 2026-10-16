import { invoke } from '@tauri-apps/api/core';
import type { AssignUserRequest, GroupStatus, Nullable, Playtest, PlaytestSpec } from '$lib/types';

export const getPlaytests = async (): Promise<Playtest[]> => invoke('get_playtests');

export const CLIENT_CAPTURE_ANNOTATION = 'believer.dev/client-capture';
export type CaptureState = 'off' | 'on' | 'args-lost';
export const isCaptureAnnotated = (p: Nullable<Playtest>): boolean =>
	p?.metadata.annotations?.[CLIENT_CAPTURE_ANNOTATION] === 'true';
export const getCaptureState = (p: Nullable<Playtest>): CaptureState => {
	if (!isCaptureAnnotated(p)) return 'off';
	return (p?.spec.gameClientCmdArgs?.length ?? 0) > 0 ? 'on' : 'args-lost';
};
export const splitLaunchArgs = (s: string): string[] => s.trim().split(/\s+/).filter(Boolean);

export enum ModalState {
	Creating,
	Editing
}

export const createPlaytest = async (
	name: string,
	project: string,
	do_not_prune: boolean,
	spec: PlaytestSpec,
	client_capture: boolean
): Promise<void> => {
	const req = {
		name,
		project,
		do_not_prune,
		spec,
		client_capture
	};
	await invoke('create_playtest', { req });
};

export const updatePlaytest = async (
	playtest: string,
	project: string,
	do_not_prune: boolean,
	spec: PlaytestSpec,
	client_capture: boolean
): Promise<void> => {
	const req = {
		project,
		do_not_prune,
		spec,
		client_capture
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
	playtest?.status?.groups.find((group) => group.users?.includes(user)) ?? null;
