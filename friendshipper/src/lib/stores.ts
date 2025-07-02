import { derived, type Readable, writable } from 'svelte/store';
import type { ChangeSet, Commit, ModifiedFile } from '@ethos/core';
import OktaAuth from '@okta/okta-auth-js';
import type {
	AppConfig,
	ArtifactEntry,
	ArtifactListResponse,
	CommitMessage,
	CommitWorkflowInfo,
	DynamicConfig,
	Nullable,
	Playtest,
	ProjectConfig,
	RepoConfig,
	RepoStatus
} from '$lib/types';
import { getPlaytestGroupForUser } from '$lib/playtests';

export const updateDismissed = writable(false);
export const dynamicConfig = writable(<DynamicConfig>{});

export const projectConfigs = writable(<ProjectConfig[]>[]);

export const playtests = writable(<Playtest[]>[]);

export const builds = writable(<ArtifactListResponse>{});
export const appConfig = writable(<AppConfig>{});
export const repoConfig = writable(<Nullable<RepoConfig>>null);
export const commits = writable(<Commit[]>[]);
export const commitMessage = writable(<string | CommitMessage>'');
export const oktaAuth = writable(<Nullable<OktaAuth>>null);
export const selectedFiles = writable(<ModifiedFile[]>[]);
export const repoStatus = writable(<Nullable<RepoStatus>>null);
export const workflows = writable(<CommitWorkflowInfo[]>[]);
export const engineWorkflows = writable(<CommitWorkflowInfo[]>[]);
export const onboardingInProgress = writable(false);
export const changeSets = writable(<ChangeSet[]>[]);
export const startTime = writable(Date.now());
export const backgroundSyncInProgress = writable(false);
export const currentSyncedVersion = writable('');
export const showPreferences = writable(false);

export const nextPlaytest = derived([playtests, appConfig], ([$playtests, $appConfig]) => {
	if ($playtests.length > 0) {
		const nextAssigned = $playtests.find(
			(p) => getPlaytestGroupForUser(p, $appConfig.userDisplayName) != null
		);

		return nextAssigned || $playtests[0];
	}

	return null;
});

export const activeProjectConfig: Readable<Nullable<ProjectConfig>> = derived(
	[projectConfigs, appConfig],
	([$projectConfigs, $appConfig]) => {
		for (const projectConfig of $projectConfigs) {
			if (projectConfig.name === $appConfig.selectedArtifactProject) {
				return projectConfig;
			}
		}

		if ($projectConfigs.length > 0) {
			return $projectConfigs[0];
		}

		return null;
	}
);

export const allProjects: Readable<Nullable<string[]>> = derived(
	[projectConfigs],
	([$projectConfigs]) => $projectConfigs.map((projectConfig) => projectConfig.name)
);

export const latestLocalCommit: Readable<Nullable<Commit>> = derived(commits, ($commits) => {
	for (const commit of $commits) {
		if (commit.local) {
			return commit;
		}
	}

	return null;
});

export const commitMap = derived(commits, ($commits) => {
	const map = new Map<string, Commit>();

	for (const commit of $commits) {
		map.set(commit.sha, commit);
	}

	return map;
});

export const workflowMap = derived(workflows, ($workflows) => {
	const map = new Map<string, CommitWorkflowInfo>();

	for (const workflow of $workflows) {
		map.set(workflow.commit, workflow);
	}

	return map;
});

export const builtCommits = derived(
	builds,
	($builds) =>
		$builds.entries.map((v) => ({
			value: v,
			name: v.commit
		})),
	[]
);

export const allModifiedFiles = derived(repoStatus, ($repoStatus): ModifiedFile[] => {
	const untracked = $repoStatus?.untrackedFiles ?? [];
	const modified = $repoStatus?.modifiedFiles ?? [];

	const all: ModifiedFile[] = [...untracked, ...modified];

	all.sort((a, b) => {
		const aName = a.displayName === '' ? a.path : a.displayName;
		const bName = b.displayName === '' ? b.path : b.displayName;
		return aName < bName ? -1 : 1;
	});

	return all;
});

export const selectedCommit = writable(<Nullable<ArtifactEntry>>{});
