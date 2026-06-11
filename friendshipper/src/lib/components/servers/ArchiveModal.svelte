<script lang="ts">
	import { Button, Modal, Select, Spinner } from 'flowbite-svelte';
	import {
		DownloadOutline,
		PlayOutline,
		ArrowLeftOutline,
		CalendarMonthOutline,
		LinkOutline
	} from 'flowbite-svelte-icons';
	import { onDestroy, onMount } from 'svelte';
	import { save as saveDialog } from '@tauri-apps/plugin-dialog';
	import { emit, listen, type UnlistenFn } from '@tauri-apps/api/event';
	import {
		downloadUtrace,
		getRecentUtraces,
		getUtraceDates,
		getUtracesForDate,
		openUtraceInInsights
	} from '$lib/utrace';
	import type { TraceEntry } from '$lib/types';
	import { handleError } from '$lib/utils';

	export let showModal: boolean;

	type Mode = 'recent' | 'date';

	const PAGE_SIZE = 25;

	let mode: Mode = 'recent';
	let traces: TraceEntry[] = [];
	let nextCursor: string | null = null;
	let loading = false;
	let loadingMore = false;

	let datesAvailable: string[] = [];
	let datesLoaded = false;
	let datesLoading = false;
	let selectedDate = '';

	const busyDownload = new Set<string>();
	const busyOpen = new Set<string>();
	const refreshBusy = () => {
		busyDownload.clear();
		busyOpen.clear();
	};

	const loadRecent = async () => {
		loading = true;
		try {
			const resp = await getRecentUtraces(PAGE_SIZE, null);
			traces = resp.traces;
			nextCursor = resp.nextCursor;
		} catch (e) {
			await handleError(e);
		}
		loading = false;
	};

	const handleLoadMore = async () => {
		if (!nextCursor) return;
		loadingMore = true;
		try {
			const resp = await getRecentUtraces(PAGE_SIZE, nextCursor);
			traces = [...traces, ...resp.traces];
			nextCursor = resp.nextCursor;
		} catch (e) {
			await handleError(e);
		}
		loadingMore = false;
	};

	const ensureDatesLoaded = async () => {
		if (datesLoaded || datesLoading) return;
		datesLoading = true;
		try {
			datesAvailable = await getUtraceDates();
			datesLoaded = true;
		} catch (e) {
			await handleError(e);
		}
		datesLoading = false;
	};

	const enterDateMode = async () => {
		mode = 'date';
		await ensureDatesLoaded();
	};

	const exitDateMode = async () => {
		mode = 'recent';
		selectedDate = '';
		await loadRecent();
	};

	const handleDateChange = async () => {
		if (!selectedDate) {
			traces = [];
			return;
		}
		loading = true;
		try {
			traces = await getUtracesForDate(selectedDate);
		} catch (e) {
			await handleError(e);
		}
		loading = false;
	};

	const handleDownload = async (trace: TraceEntry) => {
		const dest = await saveDialog({
			defaultPath: trace.filename,
			filters: [{ name: 'Unreal Insights Trace', extensions: ['utrace'] }]
		});
		if (!dest) return;

		busyDownload.add(trace.key);
		traces = traces;
		try {
			await downloadUtrace(trace.key, dest);
			await emit('success', `Saved ${trace.filename} to ${dest}`);
		} catch (e) {
			await handleError(e);
		}
		busyDownload.delete(trace.key);
		traces = traces;
	};

	const handleCopyLink = async (trace: TraceEntry) => {
		const link = `friendshipper://traces/${encodeURIComponent(trace.date)}/${encodeURIComponent(
			trace.serverName
		)}/${encodeURIComponent(trace.filename)}`;
		try {
			await navigator.clipboard.writeText(link);
			await emit('success', 'Copied trace link');
		} catch (e) {
			await handleError(e);
		}
	};

	const handleOpen = async (trace: TraceEntry) => {
		busyOpen.add(trace.key);
		traces = traces;
		try {
			await openUtraceInInsights(trace.key);
		} catch (e) {
			await handleError(e);
		}
		busyOpen.delete(trace.key);
		traces = traces;
	};

	const formatSize = (bytes: number): string => {
		if (bytes < 1024) return `${bytes} B`;
		const kb = bytes / 1024;
		if (kb < 1024) return `${kb.toFixed(1)} KB`;
		const mb = kb / 1024;
		if (mb < 1024) return `${mb.toFixed(1)} MB`;
		return `${(mb / 1024).toFixed(2)} GB`;
	};

	const formatRelative = (iso: string | null): string => {
		if (!iso) return '';
		const then = new Date(iso);
		const now = new Date();
		const ms = now.getTime() - then.getTime();
		const sec = Math.floor(ms / 1000);
		if (sec < 60) return `${sec}s ago`;
		const min = Math.floor(sec / 60);
		if (min < 60) return `${min}m ago`;
		const hr = Math.floor(min / 60);
		if (hr < 24) return `${hr}h ago`;
		const days = Math.floor(hr / 24);
		if (days < 7) return `${days}d ago`;
		return `${then.toISOString().slice(0, 16).replace('T', ' ')} UTC`;
	};

	$: dateSelectItems = [
		{ value: '', name: 'Select a date…' },
		...datesAvailable.map((d) => ({ value: d, name: d }))
	];

	$: if (showModal) {
		mode = 'recent';
		traces = [];
		nextCursor = null;
		selectedDate = '';
		refreshBusy();
		void loadRecent();
	}

	let unlistenDeepLink: UnlistenFn | null = null;

	onMount(async () => {
		unlistenDeepLink = await listen('trace-deeplink-received', () => {
			showModal = false;
		});
	});

	onDestroy(() => {
		unlistenDeepLink?.();
	});
</script>

<Modal
	size="lg"
	color="none"
	class="archive-modal bg-secondary-700 dark:bg-space-900"
	bodyClass="!border-t-0"
	backdropClass="fixed mt-8 inset-0 z-40 bg-gray-900 bg-opacity-50 dark:bg-opacity-80"
	dialogClass="fixed mt-8 top-0 start-0 end-0 h-modal md:inset-0 md:h-full z-50 w-full p-4 pb-12 flex"
	bind:open={showModal}
	autoclose={false}
	outsideclose
>
	<svelte:fragment slot="header">
		<div class="flex items-center justify-between gap-2 w-full pr-2">
			<h4 class="text-lg font-semibold text-primary-400">Trace Archive</h4>
			{#if mode === 'recent'}
				<Button size="xs" outline on:click={enterDateMode} disabled={loading}>
					<CalendarMonthOutline class="w-4 h-4 mr-1" />
					Jump to date
				</Button>
			{:else}
				<Button size="xs" outline on:click={exitDateMode} disabled={loading}>
					<ArrowLeftOutline class="w-4 h-4 mr-1" />
					Back to recent
				</Button>
			{/if}
		</div>
	</svelte:fragment>

	<div class="flex flex-col gap-3">
		{#if mode === 'date'}
			<div class="flex items-center gap-2">
				{#if datesLoading}
					<Spinner size="4" />
					<span class="text-xs text-gray-400">Loading dates…</span>
				{:else}
					<Select
						items={dateSelectItems}
						bind:value={selectedDate}
						on:change={handleDateChange}
						size="sm"
						class="w-48 bg-secondary-600 dark:bg-space-800 text-white border-gray-500"
					/>
				{/if}
			</div>
		{/if}

		<div
			class="flex flex-col gap-1 min-h-[16rem] max-h-[28rem] overflow-y-auto rounded border border-gray-700 bg-secondary-800 dark:bg-space-950 p-2"
		>
			{#if loading}
				<div class="flex items-center justify-center py-8 gap-2 text-gray-400">
					<Spinner size="4" />
					<span class="text-xs">Loading traces…</span>
				</div>
			{:else if traces.length === 0}
				<div class="flex items-center justify-center py-8 text-xs text-gray-400">
					{#if mode === 'date' && !selectedDate}
						Pick a date to see traces.
					{:else}
						No traces found.
					{/if}
				</div>
			{:else}
				{#each traces as trace (trace.key)}
					<div
						class="flex items-center justify-between gap-2 px-2 py-1.5 rounded hover:bg-secondary-700 dark:hover:bg-space-900"
					>
						<div class="flex flex-col min-w-0 flex-1">
							<div class="flex items-center gap-2 min-w-0">
								<button
									type="button"
									class="shrink-0 text-gray-300 hover:text-primary-400 focus:outline-none"
									title="Copy link to this trace"
									on:click={() => handleCopyLink(trace)}
								>
									<LinkOutline class="w-3.5 h-3.5" />
								</button>
								<span class="text-sm text-white truncate" title={trace.filename}>
									{trace.filename}
								</span>
								<span
									class="shrink-0 text-[10px] px-1.5 py-0.5 rounded bg-secondary-600 dark:bg-space-800 text-gray-300 border border-gray-600"
									title={trace.serverName}
								>
									{trace.serverName}
								</span>
							</div>
							<div class="text-[11px] text-gray-400">
								{formatSize(trace.size)} · {formatRelative(trace.lastModified)}
							</div>
						</div>
						<div class="flex items-center gap-1 shrink-0">
							<Button
								size="xs"
								outline
								title="Download trace…"
								disabled={busyDownload.has(trace.key) || busyOpen.has(trace.key)}
								on:click={() => handleDownload(trace)}
							>
								{#if busyDownload.has(trace.key)}
									<Spinner size="4" />
								{:else}
									<DownloadOutline class="w-4 h-4" />
								{/if}
							</Button>
							<Button
								size="xs"
								outline
								title="Open with UnrealInsights"
								disabled={busyOpen.has(trace.key) || busyDownload.has(trace.key)}
								on:click={() => handleOpen(trace)}
							>
								{#if busyOpen.has(trace.key)}
									<Spinner size="4" />
								{:else}
									<PlayOutline class="w-4 h-4" />
								{/if}
							</Button>
						</div>
					</div>
				{/each}
			{/if}
		</div>

		<div class="flex items-center justify-between text-xs text-gray-400">
			<span>{traces.length} trace{traces.length === 1 ? '' : 's'}</span>
			{#if mode === 'recent' && nextCursor}
				<Button size="xs" outline disabled={loadingMore} on:click={handleLoadMore}>
					{#if loadingMore}
						<Spinner size="4" class="mr-1" />
					{/if}
					Load more
				</Button>
			{/if}
		</div>
	</div>
</Modal>

<style>
	/* color="none" leaves the close button without a text color; restore visibility */
	:global(.archive-modal button[aria-label='Close modal']) {
		color: rgb(156 163 175); /* tailwind gray-400 */
	}
	:global(.archive-modal button[aria-label='Close modal']:hover) {
		color: rgb(255 255 255);
	}
</style>
