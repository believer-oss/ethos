<script lang="ts">
	import { Button, Input, Label, Modal, Select } from 'flowbite-svelte';
	import { ExclamationCircleOutline } from 'flowbite-svelte-icons';
	import { emit } from '@tauri-apps/api/event';
	import type { ArtifactEntry, Nullable, Playtest, PlaytestSpec } from '$lib/types';
	import { createPlaytest, deletePlaytest, ModalState, updatePlaytest } from '$lib/playtests';
	import { appConfig, activeProjectConfig, allProjects, workflowMap } from '$lib/stores';
	import { getBuilds } from '$lib/builds';

	export let versions: ArtifactEntry[];
	export let showModal: boolean;
	export let mode: ModalState;
	export let playtest: Playtest | null;
	export let onSubmit: () => void;

	let prevProject: string | null = null;
	let showConfirmation: boolean = false;

	let commits: { name: string; value: string }[] = [];
	let maps: { value: string; name: string }[] = [];
	let submitting = false;
	let deleting = false;

	const getPlaytestProject = (item: Nullable<Playtest>): string => {
		if (item === null) return '';
		if (item.metadata.annotations === null) return '';

		return item.metadata.annotations['believer.dev/project'] ?? '';
	};

	$: project = getPlaytestProject(playtest);

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

		commits = projVersions.map((v) => ({
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
		name: p.split('-')[1]
	}));

	const inputClass = 'bg-secondary-700 dark:bg-space-900 text-white';

	const handleSubmit = async (e: SubmitEvent) => {
		submitting = true;
		const formData = new FormData(e.target as HTMLFormElement);
		const data: Record<string, string> = {};
		for (const field of formData) {
			const [key, value] = field;
			data[key] = value as string;
		}

		if (mode === ModalState.Editing && playtest != null) {
			const spec: PlaytestSpec = {
				displayName: playtest.spec.displayName,
				version: data.version,
				map: data.map,
				minGroups: parseInt(data.minGroups, 10),
				playersPerGroup: parseInt(data.maxPlayersPerGroup, 10),
				startTime: new Date(`${data.startDate} ${data.startTime}`).toISOString(),
				groups: playtest.spec.groups,
				feedbackURL: data.feedbackURL
			};

			await updatePlaytest(playtest?.metadata.name, project, spec);
		} else if (mode === ModalState.Creating) {
			const spec: PlaytestSpec = {
				displayName: data.name,
				version: data.version,
				map: data.map,
				minGroups: parseInt(data.minGroups, 10),
				playersPerGroup: parseInt(data.maxPlayersPerGroup, 10),
				startTime: new Date(`${data.startDate} ${data.startTime}`).toISOString(),
				groups: [],
				feedbackURL: data.feedbackURL
			};

			const name = data.name.toLowerCase().replace(/[_\s/]/g, '-');

			try {
				await createPlaytest(name, data.project, spec);
			} catch (createError) {
				await emit('error', createError);
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

	const getPlaytestDate = (item: Nullable<Playtest>): string => {
		const date = item != null ? new Date(item.spec.startTime) : new Date();
		return `${date.getFullYear()}-${(date.getMonth() + 1).toLocaleString('en-US', {
			minimumIntegerDigits: 2
		})}-${date.getDate().toLocaleString('en-US', { minimumIntegerDigits: 2 })}`;
	};

	const getPlaytestTime = (item: Nullable<Playtest>): string => {
		const date = item != null ? new Date(item.spec.startTime) : new Date();
		return `${date.getHours()}:${date
			.getMinutes()
			.toLocaleString('en-US', { minimumIntegerDigits: 2 })}:00`;
	};
</script>

<Modal
	size="xs"
	defaultClass="bg-secondary-700 dark:bg-space-900 overflow-y-auto"
	bodyClass="!border-t-0"
	backdropClass="fixed mt-8 inset-0 z-40 bg-gray-900 bg-opacity-50 dark:bg-opacity-80"
	dialogClass="fixed mt-8 top-0 start-0 end-0 h-modal md:inset-0 md:h-full z-50 w-full p-4 pb-12 flex"
	bind:open={showModal}
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
			/>
		</Label>
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
				<Select
					size="sm"
					name="version"
					class={inputClass}
					value={playtest ? playtest.spec.version : ''}
					required
				>
					{#each commits as commit}
						<option value={commit.value}
							>{commit.name.substring(0, 8)} {$workflowMap.get(commit.name)?.message || ''}</option
						>
					{/each}
				</Select>
			</Label>
			<Label class="space-y-2 text-xs text-white w-1/2">
				<span>Map</span>
				<Select
					size="sm"
					name="map"
					class={inputClass}
					value={playtest ? playtest.spec.map : ''}
					required
				>
					{#each maps as map}
						<option value={map.value}>{map.name}</option>
					{/each}
				</Select>
			</Label>
		</div>
		<div class="flex flex-row gap-2">
			<Label class="space-y-2 text-xs text-white w-full">
				<span>Number of groups</span>
				<Input
					type="number"
					class={inputClass}
					size="sm"
					name="minGroups"
					min="1"
					max="10"
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
					value={playtest ? playtest.spec.playersPerGroup : 1}
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
