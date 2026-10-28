<script lang="ts">
	import { Alert, Button, Tooltip } from 'flowbite-svelte';
	import { ExclamationCircleSolid, InfoCircleSolid } from 'flowbite-svelte-icons';
	import { emit } from '@tauri-apps/api/event';
	import { createEventDispatcher } from 'svelte';
	import { repoConfig } from '$lib/stores';
	import {
		dismissRebaseState,
		finishRebase,
		getRebaseState,
		redoRebase,
		resetRebaseToBackup
	} from '$lib/repo';
	import {
		pushFailedText,
		refreshAfterBranchChange,
		sha7,
		shortSha,
		staleMessage
	} from '$lib/branches';
	import type { Nullable, RebaseFinishResponse, RebaseState } from '$lib/types';
	import ConfirmModal from '$lib/components/ConfirmModal.svelte';

	export let rebaseState: RebaseState;

	const dispatch = createEventDispatcher<{
		changed: null;
		finished: RebaseFinishResponse;
		progress: { title: string };
		progressdone: null;
	}>();

	const infoAlertClass =
		'bg-secondary-700 dark:bg-space-900 border border-primary-500 text-white dark:text-white';
	const warningAlertClass =
		'bg-yellow-900 dark:bg-yellow-900 border border-yellow-700 text-yellow-100 dark:text-yellow-100';

	// Disables every action while a request runs.
	let inFlight = false;
	let resetOpen = false;
	let redoOpen = false;

	$: branch = rebaseState.branch;
	$: trunk = $repoConfig?.trunkBranch || 'main';
	$: backupLabel = rebaseState.backupSha ? shortSha(rebaseState.backupSha) : 'unknown';
	$: resetTitle = `${branch} will be reset to ${rebaseState.backupRef} (${backupLabel}), its state before the rebase, when it matched origin. The rebased commits are discarded. Then Sync to pick up any new commits from origin.`;
	$: redoTitle = `Your conflict resolutions will be lost. Friendshipper will back up your rebased result, reset ${branch} to the version on origin (which includes your collaborator's new commits), and start a fresh rebase onto the latest ${trunk}.`;

	// The backend may have moved even when a request failed.
	const refetchState = async () => {
		try {
			rebaseState = await getRebaseState();
		} catch (e) {
			await emit('error', e);
		}
	};

	// Runs a request that moves HEAD, or touches the rebase record. The progress modal stays up
	// until the status and the rebase state have refreshed.
	const run = async <R>(title: Nullable<string>, call: () => Promise<R>): Promise<Nullable<R>> => {
		if (inFlight) return null;
		inFlight = true;
		if (title !== null) dispatch('progress', { title });
		let result: Nullable<R> = null;
		try {
			result = await call();
		} catch (e) {
			await emit('error', e);
		}
		if (title !== null) await refreshAfterBranchChange();
		await refetchState();
		if (title !== null) dispatch('progressdone', null);
		inFlight = false;
		return result;
	};

	const push = async () => {
		const res = await run('Pushing rebased branch', finishRebase);
		if (res?.outcome === 'pushed') {
			dispatch('finished', res);
		} else {
			dispatch('changed', null);
		}
	};

	const reset = async () => {
		await run('Resetting to backup', resetRebaseToBackup);
		dispatch('changed', null);
	};

	const redo = async () => {
		await run('Redoing rebase', redoRebase);
		dispatch('changed', null);
	};

	const dismiss = async () => {
		await run(null, dismissRebaseState);
		dispatch('changed', null);
	};

	// Refetches first, so the confirmation names the current backup.
	const askReset = async () => {
		if (inFlight) return;
		inFlight = true;
		await refetchState();
		inFlight = false;
		if (rebaseState.state === 'completedUnpushed' || rebaseState.state === 'pushFailed') {
			resetOpen = true;
		}
	};

	const askRedo = () => {
		if (!inFlight) redoOpen = true;
	};

	const onResetAnswer = (yes: boolean) => {
		if (yes) void reset();
	};

	const onRedoAnswer = (yes: boolean) => {
		if (yes) void redo();
	};
</script>

{#if rebaseState.state === 'completedUnpushed'}
	<Alert class={infoAlertClass}>
		<InfoCircleSolid slot="icon" class="w-5 h-5 shrink-0 text-primary-400" />
		<div class="flex flex-col gap-2">
			<p>All commits were replayed onto latest {trunk} but not pushed yet.</p>
			{#if !rebaseState.stepIncomplete && rebaseState.droppedCommits.length > 0}
				<div class="text-sm">
					<p>
						Not carried over, because {trunk} already has them or your resolutions emptied them:
					</p>
					<ul class="pl-4">
						{#each rebaseState.droppedCommits as c (c.sha)}
							<li class="truncate" title={c.subject}>
								<span class="font-mono text-primary-400">{sha7(c.sha)}</span>
								{c.subject}
							</li>
						{/each}
					</ul>
				</div>
			{/if}
			<div class="flex flex-row gap-2 items-center flex-wrap">
				<Button size="sm" disabled={inFlight} on:click={push}>Push rebased branch</Button>
				<Button
					id="rebase-reset-remote"
					size="sm"
					color="alternative"
					disabled={inFlight}
					on:click={askReset}
				>
					Reset to remote version
				</Button>
				<Tooltip
					triggeredBy="#rebase-reset-remote"
					class="z-50 w-[22rem] text-xs text-primary-400 bg-secondary-600 dark:bg-space-800 shadow-2xl"
					placement="bottom"
					>Discards the rebase and puts <code>{branch}</code> back the way it was on origin when the
					rebase started. Nothing is pushed.</Tooltip
				>
			</div>
			<p class="text-xs text-gray-300">
				Force-pushes <code>{branch}</code> to origin, replacing the version collaborators have; they
				must Sync.
			</p>
		</div>
	</Alert>
{:else if rebaseState.state === 'pushFailed'}
	<Alert class={warningAlertClass}>
		<ExclamationCircleSolid slot="icon" class="w-5 h-5 shrink-0 text-yellow-300" />
		<div class="flex flex-col gap-2">
			<p>{pushFailedText(rebaseState)}</p>
			<div class="flex flex-row gap-2 items-center flex-wrap">
				<Button size="sm" disabled={inFlight} on:click={askRedo}>Redo rebase on new origin</Button>
				<Button size="sm" color="alternative" disabled={inFlight} on:click={askReset}>
					Reset to backup
				</Button>
			</div>
		</div>
	</Alert>
{:else if rebaseState.state === 'stale'}
	<Alert class={warningAlertClass}>
		<ExclamationCircleSolid slot="icon" class="w-5 h-5 shrink-0 text-yellow-300" />
		<div class="flex flex-col gap-2">
			<p>{staleMessage(rebaseState)}</p>
			<div>
				<Button size="sm" color="alternative" disabled={inFlight} on:click={dismiss}>Dismiss</Button
				>
			</div>
		</div>
	</Alert>
{/if}

<ConfirmModal bind:showModal={resetOpen} title={resetTitle} resultCallback={onResetAnswer} />
<ConfirmModal bind:showModal={redoOpen} title={redoTitle} resultCallback={onRedoAnswer} />
