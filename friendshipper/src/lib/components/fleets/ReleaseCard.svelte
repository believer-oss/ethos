<script lang="ts">
	// One FleetRelease on the shard page: a header (phase, build, the channels serving it, since when
	// and who), one sentence saying what's going on and what to do, a chip per cluster and fleet map,
	// and the actions that fit its phase, the next one as a button and the rest in a ⋯ menu.
	import { Button, Dropdown, DropdownItem, Spinner } from 'flowbite-svelte';
	import { emit } from '@tauri-apps/api/event';
	import {
		pinFleetRelease,
		refreshFleets,
		retireFleetRelease,
		setFleetReleaseOverrides,
		warmFleetRelease
	} from '$lib/fleets';
	import { formatAge, shortSha } from '$lib/fleetFormat';
	import {
		isRetiring,
		releaseAllocated,
		releaseChips,
		releaseDetails,
		releasePhase,
		releasePhaseClass,
		releasePhaseLabel,
		releaseStatus,
		releaseWho,
		type ChipTone,
		type StatusTone
	} from '$lib/fleetReleases';
	import {
		overridesToRows,
		policyText,
		rowsToOverrides,
		saveGeneration,
		type OverrideRow
	} from '$lib/fleetSizing';
	import type {
		FleetRelease,
		FleetReleasePhase,
		FleetsConfig,
		Nullable,
		Promotion
	} from '$lib/types';
	import ReleaseGate from './ReleaseGate.svelte';
	import OverridesEditor from './OverridesEditor.svelte';
	import RetireReleaseDialog from './RetireReleaseDialog.svelte';
	import ShaLink from './ShaLink.svelte';

	export let release: FleetRelease;
	export let fleets: Nullable<FleetsConfig>;
	export let fleetMapIds: string[];
	export let promotions: Promotion[];
	export let now: number;
	export let reload: () => Promise<void>;
	// With promotionViaCoordinator off: no actions.
	export let readOnly = false;

	$: name = release.metadata.name ?? '';
	$: sha = shortSha(release.spec.version);
	$: phase = releasePhase(release);
	$: retiring = isRetiring(release);
	$: allocated = releaseAllocated(release);
	$: status = releaseStatus(release, fleets, promotions, now);
	$: chips = releaseChips(release, now);
	$: details = releaseDetails(release);
	$: clusters = (fleets?.clusters ?? []).map((c) => c.name);
	$: limit = fleets?.limits?.maxReplicasPerFleet ?? null;

	let busy = '';
	const act = async (what: string, action: () => Promise<unknown>, done: string) => {
		busy = what;
		try {
			await action();
			await emit('success', done);
			await reload();
			await refreshFleets();
		} catch (e) {
			await emit('error', e);
		} finally {
			busy = '';
		}
	};

	const togglePin = () =>
		act(
			'pin',
			() => pinFleetRelease(name, !release.spec.pinned),
			`${release.spec.pinned ? 'Unpinned' : 'Pinned'} ${sha}`
		);

	const warmAgain = () =>
		act('warm', () => warmFleetRelease(release.spec.shard, release.spec.version), `Warming ${sha}`);

	let showRetire = false;
	let endMatches = false;
	const askRetire = (end: boolean) => {
		endMatches = end;
		showRetire = true;
	};
	const retire = (force: boolean) =>
		act(
			'retire',
			() => retireFleetRelease(name, force),
			force ? `Ending the matches on ${sha} and retiring it` : `Retiring ${sha}`
		);

	// The shard page expands its Fleet settings on this event; the scroll waits for that render.
	const openSettings = () => {
		window.dispatchEvent(new CustomEvent('fleet-settings-open'));
		setTimeout(() => {
			document.getElementById('fleet-settings')?.scrollIntoView({ behavior: 'smooth' });
		}, 0);
	};

	// Overrides for this release only.
	let editing = false;
	let rows: OverrideRow[] = [];
	let overrideProblems = 0;
	let overridesError = '';
	// The generation the editor opened at: the save tests it, so a release changed since (by a poll
	// bringing someone else's edit) refuses the save instead of losing their overrides.
	let editingAt: Nullable<number> = null;
	$: overridesStale = editing && saveGeneration(editingAt, release.metadata.generation) === null;
	const startEditing = () => {
		rows = overridesToRows(release.spec.overrides);
		editingAt = release.metadata.generation ?? null;
		overridesError = '';
		editing = true;
	};
	// A refused save keeps the editor open with the error, so the edits aren't lost.
	const saveOverrides = async () => {
		const version = saveGeneration(editingAt, release.metadata.generation);
		if (version === null) return;
		busy = 'overrides';
		overridesError = '';
		try {
			await setFleetReleaseOverrides(name, version, rowsToOverrides(rows));
			editing = false;
			await emit('success', `Saved the overrides of ${sha}`);
			await reload();
			await refreshFleets();
		} catch (e) {
			overridesError = (e as { message?: string })?.message ?? String(e);
		} finally {
			busy = '';
		}
	};

	interface Action {
		label: string;
		run: () => void;
	}
	// The ⋯ menu for each phase; the next action is a button of its own in the markup.
	const menuFor = (
		current: Nullable<FleetReleasePhase>,
		isRetiringNow: boolean,
		pinned: boolean
	): Action[] => {
		const pin = (label: string): Action => ({
			label: pinned ? 'Unpin' : label,
			run: () => {
				void togglePin();
			}
		});
		const retireItem = (label: string): Action => ({
			label,
			run: () => {
				askRetire(false);
			}
		});
		if (isRetiringNow || current === 'Retired') return [];
		if (current === 'Live') return [pin('Pin'), retireItem('Retire')];
		if (current === 'Draining') return [pin('Pin (keeps it warm)'), retireItem('Retire now')];
		if (current === 'Failed') return [retireItem('Retire')];
		return [pin('Pin'), { label: 'Overrides', run: startEditing }, retireItem('Retire')];
	};
	$: menu = menuFor(phase, retiring, release.spec.pinned);
	let menuOpen = false;

	// The icon before the status sentence, outside its pre-line text so it stays on the first line.
	const STATUS_ICON: Record<StatusTone, string> = { ok: '', info: '', warn: '⚠', bad: '✕' };
	// "since 12 m", or "since just now".
	const sinceText = (at: string, nowMs: number) => {
		const age = formatAge(at, nowMs);
		return age === 'now' ? 'just now' : age;
	};

	const TONE_CLASS: Record<StatusTone, string> = {
		ok: 'text-gray-200',
		info: 'text-gray-300',
		warn: 'text-yellow-300',
		bad: 'text-red-300'
	};
	const BORDER_CLASS: Record<StatusTone, string> = {
		ok: 'border-secondary-600 dark:border-space-800',
		info: 'border-secondary-600 dark:border-space-800',
		warn: 'border-yellow-700',
		bad: 'border-red-800'
	};
	const DOT_CLASS: Record<ChipTone, string> = {
		ok: 'bg-green-400',
		stale: 'bg-gray-500',
		failed: 'bg-red-500',
		pending: 'bg-yellow-400',
		retiring: 'bg-blue-400',
		refused: 'bg-red-400',
		off: 'bg-gray-500'
	};
</script>

<div
	id="release-{name}"
	class="rounded-md border {BORDER_CLASS[
		status.tone
	]} p-3 text-sm text-gray-300 flex flex-col gap-2"
>
	<div class="flex items-center gap-2 flex-wrap">
		<span
			class="rounded bg-gray-800 px-1.5 py-0.5 text-xs font-semibold {releasePhaseClass(release)}"
			>{releasePhaseLabel(release)}</span
		>
		<ShaLink sha={release.spec.version} title={name} />
		{#each release.status?.servedBy ?? [] as channel}
			<span
				class="text-xs rounded px-1 {phase === 'Retired'
					? 'bg-yellow-900/40 text-yellow-300'
					: 'bg-green-900/40 text-green-300'}">{channel}</span
			>
		{/each}
		{#if release.spec.pinned}<span class="text-xs text-primary-400">pinned</span>{/if}
		<span class="text-gray-400 text-xs">
			{#if release.status?.phaseSince}since {sinceText(release.status.phaseSince, now)} ·{/if}
			{releaseWho(release, promotions)}
		</span>
		{#if !readOnly}
			<span class="ml-auto flex items-center gap-2">
				{#if retiring && allocated > 0 && !release.spec.force}
					<ReleaseGate let:disabled>
						<Button
							size="xs"
							color="red"
							disabled={disabled || !!busy}
							on:click={() => {
								askRetire(true);
							}}
						>
							{#if busy === 'retire'}<Spinner size="3" class="mr-1" />{/if}End {allocated}
							match{allocated === 1 ? '' : 'es'} now…
						</Button>
					</ReleaseGate>
				{:else if phase === 'Retired'}
					<ReleaseGate let:disabled>
						<Button
							size="xs"
							color="green"
							disabled={disabled || !!busy || !!release.metadata.deletionTimestamp}
							on:click={warmAgain}
						>
							{#if busy === 'warm'}<Spinner size="3" class="mr-1" />{/if}Warm again
						</Button>
					</ReleaseGate>
				{:else if phase === 'Live'}
					<ReleaseGate let:disabled>
						<Button
							size="xs"
							color="alternative"
							disabled={disabled || !!busy}
							on:click={startEditing}>Overrides</Button
						>
					</ReleaseGate>
				{:else if phase === 'Failed' && status.settings}
					<Button size="xs" color="alternative" on:click={openSettings}>Fleet settings</Button>
				{/if}
				{#if menu.length}
					<ReleaseGate let:disabled>
						<Button
							id="release-menu-{name}"
							size="xs"
							color="alternative"
							title="More actions"
							disabled={disabled || !!busy}
						>
							{#if busy === 'pin' || busy === 'retire'}<Spinner size="3" />{:else}⋯{/if}
						</Button>
					</ReleaseGate>
					<Dropdown triggeredBy="#release-menu-{name}" placement="bottom-end" bind:open={menuOpen}>
						{#each menu as item (item.label)}
							<DropdownItem
								class="text-xs"
								on:click={() => {
									menuOpen = false;
									item.run();
								}}>{item.label}</DropdownItem
							>
						{/each}
					</Dropdown>
				{/if}
			</span>
		{/if}
	</div>

	<div class="flex gap-1.5 {TONE_CLASS[status.tone]}">
		{#if STATUS_ICON[status.tone]}<span aria-hidden="true">{STATUS_ICON[status.tone]}</span>{/if}
		<span class="whitespace-pre-line">{status.text}</span>
	</div>
	{#if status.hint}<div class="text-xs text-gray-400">{status.hint}</div>{/if}

	{#if chips.length}
		<div class="flex flex-wrap gap-1.5">
			{#each chips as chip (chip.key)}
				<span
					class="inline-flex items-center gap-1.5 rounded bg-gray-800 px-2 py-0.5 text-xs {chip.tone ===
						'stale' || chip.tone === 'off'
						? 'text-gray-500'
						: 'text-gray-300'}"
				>
					<span class="inline-block w-2 h-2 rounded-full {DOT_CLASS[chip.tone]}" />{chip.text}
				</span>
			{/each}
		</div>
	{/if}

	{#if release.spec.overrides?.length && !editing}
		<div class="text-xs text-gray-400">
			Overrides: {release.spec.overrides
				.map((o) => `${o.mapId} in ${o.cluster}: ${policyText(o.sizing)}`)
				.join('; ')}
		</div>
	{/if}
	{#if details.length}
		<details class="text-xs text-gray-400">
			<summary class="cursor-pointer w-fit">Details</summary>
			{#each details as line}<div class="font-mono">{line}</div>{/each}
		</details>
	{/if}
	{#if editing}
		<div class="flex flex-col gap-2 border-t border-secondary-600 dark:border-space-800 pt-2">
			<span class="text-xs text-gray-400"
				>Overrides for this release only, over the shard's sizing. Copied to the next release on
				promotion.</span
			>
			<OverridesEditor
				bind:rows
				bind:problemCount={overrideProblems}
				{clusters}
				maps={fleetMapIds}
				{limit}
				disabled={!!busy}
			/>
			{#if overridesStale}
				<div class="text-yellow-400 text-sm bg-yellow-900/20 p-2 rounded">
					This release changed since you opened its overrides, so this save would be refused. Cancel
					and open them again to see the changes.
				</div>
			{/if}
			{#if overridesError}
				<div class="text-red-400 text-sm bg-red-900/20 p-2 rounded">
					Not saved: {overridesError}
				</div>
			{/if}
			<div class="flex gap-2">
				<Button
					size="xs"
					disabled={!!busy || overrideProblems > 0 || overridesStale}
					on:click={saveOverrides}
				>
					{#if busy === 'overrides'}<Spinner size="3" class="mr-1" />{/if}Save overrides
				</Button>
				<Button
					size="xs"
					color="alternative"
					disabled={!!busy}
					on:click={() => {
						editing = false;
					}}>Cancel</Button
				>
			</div>
		</div>
	{/if}
</div>

<RetireReleaseDialog bind:open={showRetire} {release} {endMatches} onRetire={retire} />
