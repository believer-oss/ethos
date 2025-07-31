<script lang="ts">
	import { Button, Checkbox, Input, Label, Modal, Select, Tooltip, Helper } from 'flowbite-svelte';
	import { EditOutline, ExclamationCircleOutline, UndoOutline } from 'flowbite-svelte-icons';
	import { emit } from '@tauri-apps/api/event';
	import type {
		ArtifactEntry,
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
	import { getBuild, getBuilds } from '$lib/builds';
	import { getServerArgsDisplayString } from '$lib/gameServers';

	export let versions: ArtifactEntry[];
	export let showModal: boolean;
	export let mode: ModalState;
	export let playtest: Playtest | null;
	export let onSubmit: () => void;

	let prevProject: string | null = null;
	let showConfirmation: boolean = false;

	let commits: { name: string; value: string }[] = [];
	let maps: { value: string; name: string }[] = [];
	let profiles: { value: PlaytestProfile; name: string }[] = [];
	let submitting = false;
	let deleting = false;
	let project: string = '';

	let playtestError: string | null = null;
	let nameError: boolean = false;

	enum CommitSelectMode {
		Default,
		Custom
	}

	let commitSelectMode: CommitSelectMode = CommitSelectMode.Default;

	const getCommitPhase = (commit: string): string => {
		const commitWorkflow = $workflowMap.get(commit);
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

	const getBranchInfo = (commit: string): { branch: string | null; isMain: boolean } => {
		const commitWorkflow = $workflowMap.get(commit);
		if (!commitWorkflow || !commitWorkflow.branch) {
			return { branch: null, isMain: false };
		}

		const cleanBranchName = commitWorkflow.branch.replace('refs/heads/', '');
		const isMain = cleanBranchName.toLowerCase() === 'main';

		return { branch: cleanBranchName, isMain };
	};

	const getProjectValues = async (
		item: Nullable<Playtest>,
		entries: ArtifactEntry[],
		proj: Nullable<string>
	) => {
		let projVersions = Array<ArtifactEntry>();

		if (proj) {
			try {
				projVersions = await getBuilds(250, proj).then((res) => res.entries);
			} catch (getBuildsError) {
				await emit('error', getBuildsError);
			}

			// This is purposefully not being set in the global state. We want to update the maps for this Modal only.
			if (prevProject === null) {
				prevProject = $appConfig.selectedArtifactProject;
			}
			$appConfig.selectedArtifactProject = proj;
		} else {
			projVersions = entries;
		}

		maps = $activeProjectConfig?.maps.map((m) => ({ value: m, name: m })) ?? [];

		profiles = $repoConfig?.playtestProfiles.map((p) => ({
			name: p.name,
			value: p
		}));

		// Filter out failed builds from the available commits
		const filteredVersions = projVersions.filter((v) => {
			const phase = getCommitPhase(v.commit);
			return phase !== 'Failed';
		});

		commits = filteredVersions.map((v) => ({
			value: v.commit,
			name: v.commit
		}));

		// If we have a version selected already, and it's older than the entire commit list,
		// let's add it to the list to avoid confusion.
		if (item != null && !commits.find((c) => c.value === item?.spec.version)) {
			commits.push({
				value: item.spec.version,
				name: item.spec.version
			});
		}
	};

	$: (async () => {
		await getProjectValues(playtest, versions, project);
	})().catch((e) => {
		void emit('error', e);
	});

	const projects = $allProjects?.map((p) => ({
		value: p,
		name: p.substring(p.indexOf('-') + 1)
	}));

	const getPlaytestProject = (item: Nullable<Playtest>): string => {
		if (item === null) return projects?.[0]?.value ?? '';

		if (item.metadata.annotations === null) return '';

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

		let gameServerCmdArgs: string[] = [];
		if (data.profile !== undefined) {
			const selectedProfileName = data.profile;
			const selectedProfile = profiles.find((p) => p.name === selectedProfileName);
			if (selectedProfile) {
				gameServerCmdArgs = selectedProfile.value.args.split(' ');
			}
		}

		if (mode === ModalState.Editing && playtest != null) {
			const doNotPrune = !('autoCleanup' in data);
			const spec: PlaytestSpec = {
				displayName: playtest.spec.displayName,
				version: data.version,
				map: data.map,
				minGroups: parseInt(data.minGroups, 10),
				playersPerGroup: parseInt(data.maxPlayersPerGroup, 10),
				startTime: new Date(`${data.startDate} ${data.startTime}`).toISOString(),
				groups: playtest.spec.groups,
				feedbackURL: data.feedbackURL,
				includeReadinessProbe: playtest?.spec.includeReadinessProbe ?? false,
				gameServerCmdArgs
			};

			try {
				if (commitSelectMode === CommitSelectMode.Custom) {
					await getBuild(data.version, data.project);
				}

				await updatePlaytest(playtest?.metadata.name, project, doNotPrune, spec);
			} catch (updateError) {
				playtestError = (updateError as Error).message;
				submitting = false;
				return;
			}
		} else if (mode === ModalState.Creating) {
			const doNotPrune = !('autoCleanup' in data);
			const includeReadinessProbe = 'includeReadinessProbe' in data;
			const spec: PlaytestSpec = {
				displayName: data.name,
				version: data.version,
				map: data.map,
				minGroups: parseInt(data.minGroups, 10),
				playersPerGroup: parseInt(data.maxPlayersPerGroup, 10),
				startTime: new Date(`${data.startDate} ${data.startTime}`).toISOString(),
				groups: [],
				feedbackURL: data.feedbackURL,
				includeReadinessProbe,
				gameServerCmdArgs
			};

			const name = data.name.toLowerCase().replace(/[_\s/]/g, '-');

			try {
				if (commitSelectMode === CommitSelectMode.Custom) {
					await getBuild(data.version, data.project);
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

		// Put the real project back in the global state.
		$appConfig.selectedArtifactProject = prevProject ?? '';

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

		project = getPlaytestProject(playtest);
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
		<div class="flex flex-row gap-2">
			<Label class="space-y-2 text-xs text-white w-1/2">
				<span>Version</span>
				<div class="flex flex-row gap-2 w-full">
					{#if commitSelectMode === CommitSelectMode.Default}
						<Select
							size="sm"
							name="version"
							class={inputClass}
							value={playtest ? playtest.spec.version : commits[0]?.value ?? ''}
							required
						>
							{#each commits as commit}
								{@const branchInfo = getBranchInfo(commit.value)}
								<option
									value={commit.value}
									class={branchInfo.isMain ? 'text-green-400' : 'text-blue-400'}
								>
									{commit.name.substring(0, 8)}
									{branchInfo.branch && !branchInfo.isMain ? ` (${branchInfo.branch})` : ''}
									{$workflowMap.get(commit.name)?.message || ''}
								</option>
							{/each}
						</Select>
						<Button
							size="xs"
							on:click={() => {
								commitSelectMode = CommitSelectMode.Custom;
							}}
						>
							<EditOutline />
						</Button>
						<Tooltip
							placement="bottom"
							class="w-auto text-xs text-primary-400 bg-secondary-600 dark:bg-space-800"
						>
							Enter commit manually
						</Tooltip>
					{:else}
						<Input
							type="text"
							class={inputClass}
							size="sm"
							name="version"
							value={playtest ? playtest.spec.version : commits[0]?.value ?? ''}
							required
						/>
						<Button
							size="xs"
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
			</Label>
			<Label class="space-y-2 text-xs text-white w-1/2">
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
		</div>
		{#if commitSelectMode === CommitSelectMode.Custom}
			<span class="text-xs bg-red-700 text-white p-2 rounded-md">
				Warning: The map list for manually entered commits may not be up to date.
			</span>
		{/if}
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
