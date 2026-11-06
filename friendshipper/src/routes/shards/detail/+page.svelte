<script lang="ts">
	// One shard: a header (Fleets on or off, the backend in one line), shard-level problems once,
	// then a "Now" card per channel (its build, who promoted it, Fleets health, any running
	// promotion, Roll back…), the promotions (running ones in full, finished ones a line each),
	// the releases, and the Fleet settings, collapsed to a line. Reached as
	// /shards/detail?name=<shard>: the app is prerendered with no fallback page, so a dynamic
	// /shards/[name] route can't be built.
	import { onDestroy, onMount } from 'svelte';
	import { Button, Card, Spinner } from 'flowbite-svelte';
	import {
		ArrowLeftOutline,
		ExclamationCircleOutline,
		RefreshOutline
	} from 'flowbite-svelte-icons';
	import { emit } from '@tauri-apps/api/event';
	import { page } from '$app/stores';
	import {
		cancelPromotion,
		fleetRevision,
		getShard,
		promotionViaCoordinator,
		refreshFleets,
		watchFleets
	} from '$lib/fleets';
	import {
		backendParts,
		backendWarning,
		cancelEffect,
		channelKey,
		channelLabel,
		channelSinceText,
		isActivePromotion,
		observationWarning,
		promotionSubmittedAt
	} from '$lib/fleetFormat';
	import { isExpandedByDefault, trayDetail } from '$lib/promotionCard';
	import {
		backendSummary,
		channelsNow,
		fleetSettingsLine,
		partTimes,
		settingsStartOpen,
		shardProblems
	} from '$lib/shardPage';
	import type { Nullable, Promotion, ShardDetail } from '$lib/types';
	import { fleetMaps } from '$lib/fleetSizing';
	import { releaseSections, type StatusTone } from '$lib/fleetReleases';
	import ConfirmModal from '$lib/components/ConfirmModal.svelte';
	import PromoteBuildModal from '$lib/components/PromoteBuildModal.svelte';
	import ReleaseGate from '$lib/components/fleets/ReleaseGate.svelte';
	import FleetSettings from '$lib/components/fleets/FleetSettings.svelte';
	import PromotionCard from '$lib/components/fleets/PromotionCard.svelte';
	import ReleaseCard from '$lib/components/fleets/ReleaseCard.svelte';
	import ShaLink from '$lib/components/fleets/ShaLink.svelte';

	let detail: Nullable<ShardDetail> = null;
	let error = '';
	let loading = false;
	let now = Date.now();
	let cancelTarget: Nullable<Promotion> = null;
	let showCancelConfirm = false;
	let cancelling = '';
	// Retired releases are kept a while as history; hidden unless asked for.
	let showRetired = false;
	let showBackend = false;
	// Null until the shard loads: then open only when Fleets are off and not set up.
	let settingsOpen: Nullable<boolean> = null;

	// Roll back…: the promote form, opened on the channel at its previous build.
	let showPromote = false;
	let promoteCommit = '';
	let promoteChannel = '';

	const clock = setInterval(() => {
		now = Date.now();
	}, 30_000);
	const openSettings = () => {
		settingsOpen = true;
	};
	onMount(() => {
		window.addEventListener('fleet-settings-open', openSettings);
	});
	onDestroy(() => {
		clearInterval(clock);
		window.removeEventListener('fleet-settings-open', openSettings);
	});

	// With the switch off, the poller runs only while a Shards page is open.
	onMount(watchFleets);

	// Bumped per fetch, so a slow older response can't overwrite a newer one.
	let request = 0;

	$: name = $page.url.searchParams.get('name') ?? '';

	// The spinner is for manual refreshes; background polls update in place.
	const load = async (shard: string, manual = false) => {
		if (!shard) return;
		request += 1;
		const mine = request;
		loading = manual || !detail;
		try {
			const fetched = await getShard(shard);
			if (mine !== request) return;
			detail = fetched;
			if (settingsOpen === null) settingsOpen = settingsStartOpen(fetched);
			error = '';
		} catch (e) {
			if (mine !== request) return;
			error = (e as { message?: string })?.message ?? String(e);
		} finally {
			if (mine === request) {
				loading = false;
				now = Date.now();
			}
		}
	};

	// Refetch when the name changes and on every poll of the shared poller, which runs fast while a
	// promotion is active.
	const refetch = (shard: string, _revision: number) => {
		void load(shard);
	};
	$: refetch(name, $fleetRevision);

	const newestFirst = (a: Promotion, b: Promotion) =>
		(Date.parse(promotionSubmittedAt(b) ?? '') || 0) -
		(Date.parse(promotionSubmittedAt(a) ?? '') || 0);

	const askCancel = (p: Promotion) => {
		cancelTarget = p;
		showCancelConfirm = true;
	};

	const onCancelConfirmed = async (confirmed: boolean) => {
		const target = cancelTarget?.metadata.name;
		cancelTarget = null;
		if (!confirmed || !target) return;
		cancelling = target;
		try {
			await cancelPromotion(target);
			await emit('success', `Cancel requested for ${target}`);
			await refreshFleets();
		} catch (e) {
			await emit('error', e);
		} finally {
			cancelling = '';
		}
	};

	const rollBack = (channel: string, version: string) => {
		promoteChannel = channel;
		promoteCommit = version;
		showPromote = true;
	};

	const showPromotion = (p: Promotion) => {
		document
			.getElementById(`promotion-${p.metadata.name ?? ''}`)
			?.scrollIntoView({ behavior: 'smooth' });
	};

	const TONE_CLASS: Record<StatusTone, string> = {
		ok: 'text-gray-300',
		info: 'text-gray-400',
		warn: 'text-yellow-300',
		bad: 'text-red-300'
	};
</script>

<div class="flex items-center gap-2 flex-wrap">
	<a href="/shards" class="text-gray-400 hover:text-white"><ArrowLeftOutline class="w-5 h-5" /></a>
	<p class="text-2xl my-2 text-primary-400 dark:text-primary-400">
		{detail?.displayName ?? name}
	</p>
	{#if detail && detail.displayName !== detail.name}
		<code class="text-gray-400">{detail.name}</code>
	{/if}
	{#if detail?.config?.fleets}
		<span
			class="text-xs rounded px-1.5 py-0.5 {detail.spec?.fleetsEnabled
				? 'bg-green-900/40 text-green-300'
				: 'bg-gray-800 text-gray-400'}">Fleets {detail.spec?.fleetsEnabled ? 'on' : 'off'}</span
		>
	{/if}
	{#if detail}
		{@const parts = backendParts(detail.status?.backend?.parts)}
		{@const backendIssue = detail.status
			? backendWarning(detail.status.backend, detail.promotions, detail.name, now)
			: null}
		<button
			class="text-xs text-gray-400 hover:text-white flex items-center gap-1"
			title="Show the backend parts"
			on:click={() => {
				showBackend = !showBackend;
			}}
		>
			{#if detail.status?.backend && !detail.status.backend.healthy}
				<span class="text-red-400">backend unhealthy ·</span>
			{/if}
			{parts.length ? backendSummary(parts) : 'backend not observed yet'}
			{showBackend ? '▾' : '▸'}
		</button>
		{#if backendIssue}
			<span title={backendIssue}><ExclamationCircleOutline class="w-4 h-4 text-yellow-400" /></span>
		{/if}
	{/if}
	<Button disabled={loading} class="!p-1.5" primary on:click={() => load(name, true)}>
		{#if loading}
			<Spinner size="4" />
		{:else}
			<RefreshOutline class="w-4 h-4" />
		{/if}
	</Button>
</div>

<Card
	class="w-full p-4 sm:p-4 max-w-full bg-secondary-700 dark:bg-space-900 h-full border-0 shadow-none overflow-auto flex flex-col gap-6"
>
	{#if error}
		<p class="text-red-400">{error}</p>
	{/if}
	{#if detail}
		{#if showBackend}
			<table class="text-sm text-gray-300 w-fit">
				{#each backendParts(detail.status?.backend?.parts) as part (part.name)}
					<tr>
						<td class="pr-4 text-gray-400">{part.name}</td>
						{#if part.version}
							<td class="pr-4"><code>{part.version.sha}</code></td>
							<td class="pr-4">{part.version.branch}</td>
							<td class="pr-4 text-gray-400">{part.version.release}</td>
							<td
								class="text-gray-400"
								title="deploy times are known only where a promotion recorded the deploy"
							>
								{partTimes(part, detail.promotions, now)}
							</td>
						{:else}
							<td colspan="4"><code>{part.raw}</code></td>
						{/if}
					</tr>
				{/each}
			</table>
		{/if}

		{@const problems = shardProblems(detail, now)}
		{#if problems.length}
			<div class="flex flex-col gap-1 text-sm">
				{#each problems as problem}
					<div class="{TONE_CLASS[problem.tone]} flex gap-1.5">
						<span aria-hidden="true">{problem.tone === 'bad' ? '✕' : '⚠'}</span><span
							>{problem.text}</span
						>
					</div>
				{/each}
			</div>
		{/if}

		<section class="flex flex-col gap-2">
			<h2 class="text-lg text-white">Now</h2>
			<div class="flex flex-wrap gap-3">
				{#each channelsNow(detail, now) as channel (channelKey(channel.ref))}
					{@const warning = channel.observed
						? observationWarning(channel.observed, now)
						: 'Not observed yet'}
					<div
						class="rounded-md border border-secondary-600 dark:border-space-800 p-3 text-sm text-gray-300 flex flex-col gap-1 min-w-[18rem] flex-1"
					>
						<div class="flex items-center gap-2">
							<span class="text-white">{channelLabel(channel.ref)}</span>
							{#if warning}
								<span title={warning}
									><ExclamationCircleOutline class="w-4 h-4 text-yellow-400" /></span
								>
							{/if}
							{#if channel.previous && $promotionViaCoordinator && !channel.running}
								<ReleaseGate let:disabled>
									<Button
										size="xs"
										color="alternative"
										class="ml-auto"
										{disabled}
										title="Promote the build this channel served before"
										on:click={() => {
											rollBack(channelKey(channel.ref), channel.previous ?? '');
										}}>Roll back…</Button
									>
								</ReleaseGate>
							{/if}
						</div>
						<div class="flex items-center gap-2 flex-wrap text-xs text-gray-400">
							{#if channel.observed?.version}
								<span class="text-gray-200"><ShaLink sha={channel.observed.version} /></span>
							{:else if channel.observed?.buildId}
								<span>Steam build {channel.observed.buildId}</span>
							{:else}
								<span class="text-gray-500">no build</span>
							{/if}
							{#if channel.observed}<span>{channelSinceText(channel.observed, now)}</span>{/if}
							{#if channel.promotedBy}<span>· {channel.promotedBy}</span>{/if}
						</div>
						{#if channel.fleets}
							<div class="text-xs whitespace-pre-line {TONE_CLASS[channel.fleets.tone]}">
								{channel.fleets.text}
							</div>
						{/if}
						{#if channel.running}
							{@const { running } = channel}
							<div class="flex items-center gap-2 text-xs text-primary-300">
								<span class="truncate"
									>Promoting <ShaLink sha={running.spec.version} /> · {trayDetail(
										running,
										now
									)}</span
								>
								<button
									class="ml-auto text-primary-400 hover:underline flex-none"
									on:click={() => {
										showPromotion(running);
									}}>Details ↓</button
								>
							</div>
						{/if}
					</div>
				{/each}
			</div>
		</section>

		{@const promotions = [...detail.promotions].sort(newestFirst)}
		{@const expanded = promotions.filter(isExpandedByDefault)}
		{@const finished = promotions.filter((p) => !isExpandedByDefault(p))}
		<section class="flex flex-col gap-2">
			<h2 class="text-lg text-white">Promotions</h2>
			{#if promotions.length === 0}
				<p class="text-gray-500 text-sm">No promotions through the coordinator yet.</p>
			{/if}
			{#each expanded as promotion (promotion.metadata.name)}
				<div id="promotion-{promotion.metadata.name}">
					<PromotionCard
						{promotion}
						history={detail.promotions}
						releases={detail.releases}
						config={detail.config}
						fleets={detail.config?.fleets ?? null}
						{now}
						onCancel={$promotionViaCoordinator && isActivePromotion(promotion) ? askCancel : null}
						cancelling={cancelling === promotion.metadata.name}
					/>
				</div>
			{/each}
			{#if finished.length}
				<h3 class="text-xs uppercase text-gray-500 mt-1">Finished</h3>
				<div class="flex flex-col gap-1">
					{#each finished as promotion (promotion.metadata.name)}
						<div id="promotion-{promotion.metadata.name}">
							<PromotionCard
								{promotion}
								history={detail.promotions}
								releases={detail.releases}
								config={detail.config}
								fleets={detail.config?.fleets ?? null}
								{now}
							/>
						</div>
					{/each}
				</div>
			{/if}
		</section>

		{#if detail.config?.fleets || detail.releases.length}
			{@const sections = releaseSections(detail.releases)}
			{@const groups = [
				{ title: 'Serving', releases: sections.serving },
				{ title: 'In progress', releases: sections.inProgress },
				{ title: 'Not serving', releases: sections.notServing },
				{ title: 'Retired', releases: showRetired ? sections.retired : [] }
			]}
			<section id="releases" class="flex flex-col gap-2">
				<h2 class="text-lg text-white flex items-center gap-3">
					Releases
					{#if sections.retired.length}
						<button
							class="text-xs text-gray-400 hover:underline"
							on:click={() => {
								showRetired = !showRetired;
							}}>{showRetired ? 'Hide' : 'Show'} {sections.retired.length} retired</button
						>
					{/if}
				</h2>
				{#each groups.filter((g) => g.releases.length) as group (group.title)}
					<h3 class="text-xs uppercase text-gray-500">{group.title}</h3>
					{#each group.releases as release (release.metadata.name)}
						<ReleaseCard
							{release}
							fleets={detail.config?.fleets ?? null}
							fleetMapIds={fleetMaps(detail.spec)}
							promotions={detail.promotions}
							{now}
							reload={() => load(name)}
							readOnly={!$promotionViaCoordinator}
						/>
					{/each}
				{:else}
					<p class="text-gray-500 text-sm">
						{#if sections.retired.length}
							No releases in use.
						{:else}
							No releases. Promotions create them, or warm a build from the Builds page.
						{/if}
					</p>
				{/each}
			</section>

			{#if settingsOpen}
				<FleetSettings
					{detail}
					reload={() => load(name)}
					readOnly={!$promotionViaCoordinator}
					onCollapse={() => {
						settingsOpen = false;
					}}
				/>
			{:else}
				<section id="fleet-settings" class="flex flex-col gap-1">
					<h2 class="text-lg text-white flex items-center gap-3">
						Fleet settings
						<Button size="xs" color="alternative" on:click={openSettings}>Edit</Button>
					</h2>
					<p class="text-sm text-gray-400">{fleetSettingsLine(detail)}</p>
				</section>
			{/if}
		{/if}
	{:else if loading}
		<p class="text-gray-400">Loading…</p>
	{/if}
</Card>

<ConfirmModal
	bind:showModal={showCancelConfirm}
	title={`Cancel promotion ${cancelTarget?.metadata.name ?? ''}? ${
		cancelTarget ? cancelEffect(cancelTarget) : ''
	}`}
	resultCallback={onCancelConfirmed}
/>

<PromoteBuildModal
	bind:showModal={showPromote}
	bind:commit={promoteCommit}
	presetShard={detail?.name ?? ''}
	presetChannel={promoteChannel}
/>
