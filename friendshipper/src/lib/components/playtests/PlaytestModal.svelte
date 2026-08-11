<script lang="ts">
	import { Button, Checkbox, Input, Label, Modal, Select, Tooltip, Helper } from 'flowbite-svelte';
	import { ExclamationCircleOutline, UndoOutline } from 'flowbite-svelte-icons';
	import { emit } from '@tauri-apps/api/event';
	import type {
		ArtifactEntry,
		CommitWorkflowInfo,
		Nullable,
		Playtest,
		PlaytestSpec,
		PlaytestProfile
	} from '$lib/types';
	import { createPlaytest, deletePlaytest, ModalState, updatePlaytest } from '$lib/playtests';
	import {
		appConfig,
		repoConfig,
		activeProjectConfig,
		allProjects,
		workflowMap,
		builds
	} from '$lib/stores';
	import { getBuild, getBuilds, getWorkflows } from '$lib/builds';
	import { getServerArgsDisplayString } from '$lib/gameServers';
	import { resolveTrunkBranch, isTrunkBuild } from '$lib/utils';
	import BuildPicker from './BuildPicker.svelte';

	export let versions: ArtifactEntry[];
	export let showModal: boolean;
	export let mode: ModalState;
	export let playtest: Playtest | null;
	export let onSubmit: () => void;

	let prevProject: string | null = null;
	let showConfirmation: boolean = false;

	// Full entries rather than {name,value} pairs — BuildPicker needs `lastModified` and `key` too.
	let pickerVersions: ArtifactEntry[] = [];
	// Component-local: the global `workflows` store has readers on other routes and no restore path.
	let projWorkflowMap: Map<string, CommitWorkflowInfo> = new Map();
	// Re-entrancy guard for getProjectValues — see the reactive block below.
	let getProjectValuesRunId = 0;
	let maps: { value: string; name: string }[] = [];
	let profiles: { value: PlaytestProfile; name: string }[] = [];
	let submitting = false;
	let deleting = false;
	let project: string = '';

	// Controlled selection: replaces the `<Select>`'s one-way `value` and the FormData read.
	let selectedVersion: string = '';
	// The sha the playtest is saved with (Editing only) — drives BuildPicker's "Current" row.
	let originalSha: string | null = null;

	let playtestError: string | null = null;
	let nameError: boolean = false;

	enum CommitSelectMode {
		Default,
		Custom
	}

	let commitSelectMode: CommitSelectMode = CommitSelectMode.Default;

	// Takes the map explicitly: the global store is written once at app start, for the default
	// project only, so reading it here filtered nothing for other projects and went stale for this one.
	const getCommitPhase = (commit: string, wfMap: Map<string, CommitWorkflowInfo>): string => {
		const commitWorkflow = wfMap.get(commit);
		if (!commitWorkflow) return 'unknown';

		// if any workflow is running, return "Running"
		if (commitWorkflow.workflows.some((workflow) => workflow.status.phase === 'Running'))
			return 'Running';

		// if any workflow has failed, return "Failed"
		if (commitWorkflow.workflows.some((workflow) => workflow.status.phase === 'Failed'))
			return 'Failed';

		// if any workflow is pending, return "Pending"
		if (commitWorkflow.workflows.some((workflow) => workflow.status.phase === 'Pending'))
			return 'Pending';

		// if all workflows have succeeded, return "Succeeded"
		if (commitWorkflow.workflows.every((workflow) => workflow.status.phase === 'Succeeded'))
			return 'Succeeded';

		return 'unknown';
	};

	// `_item` is unused — it only keeps `playtest` in the reactive block's dependency set.
	const getProjectValues = async (
		_item: Nullable<Playtest>,
		entries: ArtifactEntry[],
		proj: Nullable<string>,
		runId: number
	) => {
		let projVersions = Array<ArtifactEntry>();
		let workflowsResult: CommitWorkflowInfo[] = [];

		if (proj) {
			// Settled, not all: a workflows failure should cost branch attribution, not the build list.
			const [buildsRes, workflowsRes] = await Promise.allSettled([
				getBuilds(250, proj),
				getWorkflows(false, proj)
			]);

			if (buildsRes.status === 'fulfilled') {
				projVersions = buildsRes.value.entries;
			} else {
				await emit('error', buildsRes.reason);
			}

			if (workflowsRes.status === 'fulfilled') {
				workflowsResult = workflowsRes.value.commits;
			} else {
				await emit('error', workflowsRes.reason);
			}
		} else {
			projVersions = entries;
			workflowsResult = $workflowMap.size > 0 ? Array.from($workflowMap.values()) : [];
		}

		// Every await is above this line and every write below it, so a superseded run touches nothing.
		if (runId !== getProjectValuesRunId) return;

		if (proj) {
			// This is purposefully not being set in the global state. We want to update the maps for this Modal only.
			if (prevProject === null) {
				prevProject = $appConfig.selectedArtifactProject;
			}
			$appConfig.selectedArtifactProject = proj;
		}

		maps = $activeProjectConfig?.maps.map((m) => ({ value: m, name: m })) ?? [];

		profiles = $repoConfig?.playtestProfiles.map((p) => ({
			name: p.name,
			value: p
		}));

		projWorkflowMap = new Map(workflowsResult.map((w) => [w.commit, w]));

		// Drop failed builds, and sha-less ones: `commit` is `Option<String>` in Rust but `string` in
		// types.ts, and a malformed S3 key would throw inside BuildPicker's keyed `{#each}`.
		pickerVersions = projVersions.filter(
			(v) => !!v.commit && getCommitPhase(v.commit, projWorkflowMap) !== 'Failed'
		);

		// Only defaults in Creating mode; Editing seeds `selectedVersion` in handleOpen first.
		// `pickerVersions` is newest-first across *every* branch, so `[0]` is often a feature-branch
		// build — take the newest trunk build instead, falling back only if there is none.
		if (selectedVersion === '') {
			const trunk = resolveTrunkBranch($appConfig?.primaryBranch, $repoConfig?.targetBranches);
			const newestTrunkBuild = pickerVersions.find((v) =>
				isTrunkBuild(projWorkflowMap.get(v.commit)?.branch, trunk)
			);
			selectedVersion = newestTrunkBuild?.commit ?? pickerVersions[0]?.commit ?? '';
		}
	};

	// Via a helper so the reactive block does not take a dependency on the counter it writes.
	const nextGetProjectValuesRunId = (): number => {
		getProjectValuesRunId += 1;
		return getProjectValuesRunId;
	};

	// Carries two network calls, so the run id is captured here and rechecked past the awaits:
	// only the last-fired run writes state. In-flight requests are not cancelled, just discarded.
	$: void (async () => {
		const runId = nextGetProjectValuesRunId();
		try {
			await getProjectValues(playtest, versions, project, runId);
		} catch (e) {
			await emit('error', e);
		}
	})();

	const projects = $allProjects?.map((p) => ({
		value: p,
		name: p.substring(p.indexOf('-') + 1)
	}));

	const getPlaytestProject = (item: Nullable<Playtest>): string => {
		if (item === null) return projects?.[0]?.value ?? '';

		// `== null`: kube omits the annotations key entirely, so it arrives `undefined`, not `null`.
		if (item.metadata.annotations == null) return '';

		return item.metadata.annotations['believer.dev/project'] ?? '';
	};

	const inputClass = 'bg-secondary-700 dark:bg-space-900 text-white';

	const validatePlaytestName = (name: string): boolean => {
		if (name === '') return true;
		const regexp = /^[a-zA-Z0-9\s_-]+$/;
		return regexp.test(name);
	};

	const handleNameValidation = (e: Event) => {
		const input = (e.target as HTMLInputElement).value;
		nameError = !validatePlaytestName(input);
	};

	const handleSubmit = async (e: SubmitEvent) => {
		submitting = true;
		playtestError = '';

		// Captured before the first await: Escape stays live during submit, and `handleClose` blanking
		// `project` mid-flight would write an empty `believer.dev/project` annotation.
		const submitMode = mode;
		const submitPlaytest = playtest;
		const submitProject = project;
		const submitVersion = selectedVersion;
		const knownVersions = pickerVersions;

		const formData = new FormData(e.target as HTMLFormElement);
		const data: Record<string, string> = {};
		for (const field of formData) {
			const [key, value] = field;
			data[key] = value as string;
		}

		if (!validatePlaytestName(data.name)) {
			playtestError =
				'Invalid playtest name. Only letters, numbers, spaces, underscores, and dashes are allowed.';
			submitting = false;
			return;
		}

		if (!submitVersion) {
			playtestError = 'A build version is required.';
			submitting = false;
			return;
		}

		// Keyed off the value, not the mode: a hand-typed sha survives the Undo back to Default, and
		// nothing validates it server-side. Anything absent from the list gets the pre-flight.
		const needsBuildPreflight = !knownVersions.some((v) => v.commit === submitVersion);

		let gameServerCmdArgs: string[] = [];
		if (data.profile !== undefined) {
			const selectedProfileName = data.profile;
			const selectedProfile = profiles.find((p) => p.name === selectedProfileName);
			if (selectedProfile) {
				gameServerCmdArgs = selectedProfile.value.args.split(' ');
			}
		}

		if (submitMode === ModalState.Editing && submitPlaytest != null) {
			const doNotPrune = !('autoCleanup' in data);
			const spec: PlaytestSpec = {
				displayName: submitPlaytest.spec.displayName,
				version: submitVersion,
				map: data.map,
				minGroups: parseInt(data.minGroups, 10),
				playersPerGroup: parseInt(data.maxPlayersPerGroup, 10),
				startTime: new Date(`${data.startDate} ${data.startTime}`).toISOString(),
				groups: submitPlaytest.spec.groups,
				feedbackURL: data.feedbackURL,
				includeReadinessProbe: submitPlaytest.spec.includeReadinessProbe ?? false,
				gameServerCmdArgs,
				disableGameServers: submitPlaytest.spec.disableGameServers ?? false
			};

			try {
				if (needsBuildPreflight) {
					// `|| undefined`: '' would arrive as `Some("")` and defeat the backend's fallback.
					// (`data.project` is undefined in Editing mode — the Project <Select> is disabled.)
					await getBuild(submitVersion, submitProject || undefined);
				}

				await updatePlaytest(submitPlaytest.metadata.name, submitProject, doNotPrune, spec);
			} catch (updateError) {
				playtestError = (updateError as Error).message;
				submitting = false;
				return;
			}
		} else if (submitMode === ModalState.Creating) {
			const doNotPrune = !('autoCleanup' in data);
			const includeReadinessProbe = 'includeReadinessProbe' in data;
			const disableGameServers = 'disableGameServers' in data;
			const spec: PlaytestSpec = {
				displayName: data.name,
				version: submitVersion,
				map: data.map,
				minGroups: parseInt(data.minGroups, 10),
				playersPerGroup: parseInt(data.maxPlayersPerGroup, 10),
				startTime: new Date(`${data.startDate} ${data.startTime}`).toISOString(),
				groups: [],
				feedbackURL: data.feedbackURL,
				includeReadinessProbe,
				gameServerCmdArgs,
				disableGameServers
			};

			const name = data.name.toLowerCase().replace(/[_\s/]/g, '-');

			try {
				if (needsBuildPreflight) {
					// `data.project` is the authority in Creating mode - the Project <Select> is enabled and
					// only one-way bound, so `project` does not track the user's choice.
					await getBuild(submitVersion, data.project || undefined);
				}
				await createPlaytest(name, data.project, doNotPrune, spec);
			} catch (createError) {
				playtestError = (createError as Error).message;
				submitting = false;
				return;
			}
		}

		submitting = false;
		showModal = false;

		onSubmit();
	};

	const handleDelete = async () => {
		deleting = true;
		if (playtest != null) {
			await deletePlaytest(playtest.metadata.name);
		}

		deleting = false;
		showModal = false;
		showConfirmation = false;

		await emit('success', 'Playtest deleted successfully!');

		onSubmit();
	};

	const handleOpen = () => {
		// if we're editing and the commit is in the workflow list, set mode to default
		if (mode === ModalState.Editing && playtest != null) {
			const commit = $builds.entries.find((c) => c.commit === playtest.spec.version);
			if (commit) {
				commitSelectMode = CommitSelectMode.Default;
			} else {
				commitSelectMode = CommitSelectMode.Custom;
			}
		} else {
			commitSelectMode = CommitSelectMode.Default;
		}

		// Order matters: assigning `project` fires the reactive block, and `selectedVersion` must
		// already be set in Editing mode so the auto-default no-ops.
		selectedVersion = playtest?.spec.version ?? '';
		originalSha = playtest?.spec.version ?? null;
		project = getPlaytestProject(playtest);
	};

	// Runs on every close path — submit, delete, close-X and Escape. The null check matters: with
	// nothing captured there is nothing to restore, and writing '' would re-point activeProjectConfig.
	const handleClose = () => {
		if (prevProject !== null) {
			$appConfig.selectedArtifactProject = prevProject;
			prevProject = null;
		}

		// Keeps the restore above from being undone: the component stays mounted, so a refreshed
		// `versions` re-fires the reactive block on a closed modal, which would re-point the global
		// value again. Also bumps the run id, discarding any fetch still in flight.
		project = '';
	};

	const getPlaytestDate = (item: Nullable<Playtest>): string => {
		const date = item != null ? new Date(item.spec.startTime) : new Date();
		return `${date.getFullYear()}-${(date.getMonth() + 1).toLocaleString('en-US', {
			minimumIntegerDigits: 2
		})}-${date.getDate().toLocaleString('en-US', { minimumIntegerDigits: 2 })}`;
	};

	const getPlaytestTime = (item: Nullable<Playtest>): string => {
		const date = item != null ? new Date(item.spec.startTime) : new Date();
		const hours = date.getHours().toLocaleString('en-US', { minimumIntegerDigits: 2 });
		const minutes = date.getMinutes().toLocaleString('en-US', { minimumIntegerDigits: 2 });
		return `${hours}:${minutes}:00`;
	};
</script>

<Modal
	size="md"
	defaultClass="bg-secondary-700 dark:bg-space-900 overflow-y-auto"
	bodyClass="!border-t-0"
	backdropClass="fixed mt-8 inset-0 z-40 bg-gray-900 bg-opacity-50 dark:bg-opacity-80"
	dialogClass="fixed mt-8 top-0 start-0 end-0 h-modal md:inset-0 md:h-full z-50 w-full p-4 pb-12 flex"
	bind:open={showModal}
	on:open={handleOpen}
	on:close={handleClose}
>
	<form class="flex flex-col space-y-4" action="#" on:submit|preventDefault={handleSubmit}>
		<h4 class="flex items-center gap-3 text-lg font-semibold text-primary-400">
			{mode === ModalState.Creating ? 'Create Playtest' : 'Edit Playtest'}
			{#if mode === ModalState.Editing}
				<Button
					class="p-2"
					size="sm"
					color="red"
					on:click={() => {
						showConfirmation = true;
						showModal = false;
					}}
				>
					Delete
				</Button>
			{/if}
		</h4>
		<Label class="space-y-2 text-xs text-white">
			<span>Name</span>
			<Input
				disabled={mode === ModalState.Editing}
				class={inputClass}
				type="text"
				size="sm"
				name="name"
				placeholder={playtest ? playtest.metadata.name : 'Playtest name'}
				value={playtest ? playtest.spec.displayName : ''}
				maxLength="18"
				required
				on:input={handleNameValidation}
				color={nameError ? 'red' : 'base'}
			/>
		</Label>
		{#if nameError}
			<Helper class="mt-2" color="red">
				<span class="font-medium">Error!</span>
				Playtest names can only include letters, numbers, spaces, underscores, and dashes.
			</Helper>
		{/if}
		<Label class="space-y-2 text-xs text-white">
			<span>Project</span>
			<Select
				disabled={mode === ModalState.Editing}
				value={project}
				size="sm"
				name="project"
				class={inputClass}
				items={projects}
				required
			/>
		</Label>
		<!-- Not a flowbite <Label>: it renders a <label> with no `for`, which implicitly targets
		     BuildPicker's summary <button>, so clicks on the open panel's dead space collapsed it.
		     The classes below are what <Label class="space-y-2 text-xs text-white"> resolves to. -->
		<div class="space-y-2 text-xs font-medium text-white rtl:text-right dark:text-gray-300">
			<span>Version</span>
			<!-- items-start: without it, align-items:stretch grows the toggle button to the open
			     panel's height. -->
			<div class="flex w-full flex-row items-start gap-2">
				{#if commitSelectMode === CommitSelectMode.Default}
					<BuildPicker
						versions={pickerVersions}
						bind:selectedSha={selectedVersion}
						workflowMap={projWorkflowMap}
						trunkBranch={resolveTrunkBranch($appConfig?.primaryBranch, $repoConfig?.targetBranches)}
						currentUserDisplayName={$appConfig?.userDisplayName ?? ''}
						{originalSha}
						onManualEntry={() => {
							commitSelectMode = CommitSelectMode.Custom;
						}}
					/>
				{:else}
					<Input type="text" class={inputClass} size="sm" bind:value={selectedVersion} required />
					<Button
						type="button"
						size="xs"
						class="shrink-0"
						on:click={() => {
							commitSelectMode = CommitSelectMode.Default;
						}}
					>
						<UndoOutline />
					</Button>
					<Tooltip
						placement="bottom"
						class="w-auto text-xs text-primary-400 bg-secondary-600 dark:bg-space-800"
					>
						Use commit from recent commits list
					</Tooltip>
				{/if}
			</div>
		</div>
		{#if commitSelectMode === CommitSelectMode.Custom}
			<span class="text-xs bg-red-700 text-white p-2 rounded-md">
				Warning: The map list for manually entered commits may not be up to date.
			</span>
		{/if}
		<Label class="space-y-2 text-xs text-white">
			<span>Map</span>
			<Select
				size="sm"
				name="map"
				class={inputClass}
				value={playtest ? playtest.spec.map : maps[0]?.value ?? ''}
				required
			>
				{#each maps as map}
					<option value={map.value}>{map.name}</option>
				{/each}
			</Select>
		</Label>
		<div class="flex flex-row gap-2">
			<Label class="space-y-2 text-xs text-white w-full">
				<span>Number of groups</span>
				<Input
					type="number"
					class={inputClass}
					size="sm"
					name="minGroups"
					min="1"
					max="25"
					value={playtest ? playtest.spec.minGroups : 1}
					required
				/>
			</Label>
			<Label class="space-y-2 text-xs text-white w-full">
				<span>Players per group</span>
				<Input
					type="number"
					class={inputClass}
					size="sm"
					name="maxPlayersPerGroup"
					min="1"
					max="12"
					value={playtest ? playtest.spec.playersPerGroup : 4}
					required
				/>
			</Label>
		</div>
		<Label class="space-y-2 text-xs text-white">
			<span>Start time</span>
			<div class="flex flex-row gap-2">
				<Input
					type="date"
					class={inputClass}
					size="sm"
					name="startDate"
					value={getPlaytestDate(playtest)}
					required
				/>
				<Input
					type="time"
					class={inputClass}
					size="sm"
					name="startTime"
					value={getPlaytestTime(playtest)}
					required
				/>
			</div>
		</Label>
		<Label class="space-y-2 text-xs text-white">
			<span>Feedback Form URL</span>
			<Input
				class={inputClass}
				type="text"
				size="sm"
				name="feedbackURL"
				placeholder={playtest ? playtest.spec.feedbackURL : 'Playtest Feedback URL'}
				value={playtest ? playtest.spec.feedbackURL : ''}
			/>
		</Label>
		{#if profiles !== null && profiles !== undefined && profiles.length > 0}
			<div>
				<Label class="flex flex-col text-xs text-white gap-2">
					<span>Profile</span>
					<Select
						size="sm"
						name="profile"
						class={inputClass}
						required
						value={playtest ? playtest.spec.gameServerCmdArgs : profiles[0].name}
						disabled={mode === ModalState.Editing}
					>
						{#each profiles as profile}
							<option value={profile.name}>
								<span>{profile.name}</span>
								<span>{getServerArgsDisplayString(profile.value.args)}</span>
							</option>
						{/each}
					</Select>
				</Label>
			</div>
		{/if}
		<div class="flex flex-row gap-2">
			<Label class="flex flex-row text-xs text-white">
				<Checkbox
					name="autoCleanup"
					checked={playtest && playtest.metadata.annotations
						? !playtest.metadata.annotations['believer.dev/do-not-prune']
						: true}
				/>
				<span>Auto Cleanup</span>
				<Tooltip>If toggled, this playtest will automatically delete in 24 hours.</Tooltip>
			</Label>
			<Label class="flex flex-row text-xs text-white">
				<Checkbox
					name="includeReadinessProbe"
					disabled={mode === ModalState.Editing}
					checked={(playtest && playtest.spec.includeReadinessProbe) ?? false}
				/>
				<span>Wait for server readiness</span>
				<Tooltip>
					If toggled, the playtest will wait for the server to be ready before starting. Version of
					the deployed gameserver must support an HTTP readiness check.
				</Tooltip>
			</Label>
		</div>
		<div class="flex flex-row gap-2">
			<Label class="flex flex-row text-xs text-white">
				<Checkbox
					name="disableGameServers"
					disabled={mode === ModalState.Editing}
					checked={(playtest && playtest.spec.disableGameServers) ?? true}
				/>
				<span>Disable Game Servers</span>
				<Tooltip>
					If toggled, no game servers will be created. Only sync and launch client functionality
					will be available (similar to launch without server mode).
				</Tooltip>
			</Label>
		</div>
		{#if playtestError}
			<span class="text-xs bg-red-700 text-white p-2 rounded-md">
				{playtestError}
			</span>
		{/if}
		<Button type="submit" class="w-full" disabled={submitting}>Submit</Button>
	</form>
</Modal>

<Modal
	defaultClass="bg-secondary-500 dark:bg-space-900 overflow-y-auto"
	bodyClass="!border-t-0"
	backdropClass="fixed mt-8 inset-0 z-40 bg-gray-900 bg-opacity-50 dark:bg-opacity-80"
	dialogClass="fixed mt-8 top-0 start-0 end-0 h-modal md:inset-0 md:h-full z-50 w-full p-4 pb-12 flex"
	bind:open={showConfirmation}
	size="xs"
	autoclose
	dismissable={false}
>
	<div class="text-center">
		<ExclamationCircleOutline class="mx-auto mb-4 text-white w-12 h-12 dark:text-gray-200" />
		<h3 class="mb-5 text-lg font-normal text-white">
			Are you sure you want to delete this playtest?
		</h3>
		<Button class="me-2" disabled={deleting} on:click={() => handleDelete()}>Yes, I'm sure</Button>
		<Button
			color="alternative"
			on:click={() => {
				showConfirmation = false;
				showModal = true;
			}}>No, cancel</Button
		>
	</div>
</Modal>
