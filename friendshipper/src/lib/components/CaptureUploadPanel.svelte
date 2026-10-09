<script lang="ts">
	import { Button, Progressbar, Spinner } from 'flowbite-svelte';
	import { listen, type UnlistenFn } from '@tauri-apps/api/event';
	import { onDestroy, onMount } from 'svelte';
	import { formatBytes } from '@ethos/core';
	import { cancelCapture, getCaptureStatus } from '$lib/capture';
	import type { CaptureFileStatus, CaptureSessionStatus, CaptureStatus } from '$lib/types';

	type Tone = 'neutral' | 'yellow' | 'red' | 'green';

	interface Strip {
		id: string;
		tone: Tone;
		text: string;
		spinner: boolean;
		progress: number | null;
		cancelLabel: string | null;
		files: CaptureFileStatus[];
	}

	const MAX_STRIPS = 2;
	const UPLOADED_LINGER_MS = 60_000;

	const toneClass: Record<Tone, string> = {
		neutral:
			'bg-secondary-800 dark:bg-space-950 border-secondary-700 dark:border-space-900 text-gray-300',
		yellow: 'bg-yellow-900 border-yellow-700 text-yellow-100',
		red: 'bg-red-900 border-red-700 text-red-100',
		green: 'bg-green-900 border-green-700 text-green-100'
	};

	let status: CaptureStatus | null = null;
	let now = Date.now();
	let expanded: Record<string, boolean> = {};
	const closedSeenAt: Record<string, number> = {};
	let unlisten: UnlistenFn | undefined;
	let destroyed = false;
	let timer: ReturnType<typeof setInterval> | undefined;

	const sum = (files: CaptureFileStatus[], pick: (f: CaptureFileStatus) => number) =>
		files.reduce((total, f) => total + pick(f), 0);

	const doneBytes = (f: CaptureFileStatus) => (f.state === 'uploaded' ? f.size : f.uploadedBytes);

	const percent = (f: CaptureFileStatus) =>
		f.size > 0 ? Math.floor((doneBytes(f) / f.size) * 100) : 0;

	const formatTime = (iso: string | null) =>
		iso ? new Date(iso).toLocaleTimeString([], { hour: '2-digit', minute: '2-digit' }) : 'soon';

	const isUploadedClose = (s: CaptureSessionStatus) =>
		s.state === 'closed' && s.files.length > 0 && s.files.every((f) => f.state === 'uploaded');

	const isVisible = (s: CaptureSessionStatus, at: number) => {
		if (s.state !== 'closed') return true;
		if (!isUploadedClose(s)) return false;
		return at - (closedSeenAt[s.id] ?? 0) < UPLOADED_LINGER_MS;
	};

	const makeStrip = (
		tone: Tone,
		text: string,
		cancelLabel: string | null = 'Cancel',
		extra: Partial<Strip> = {}
	) => ({ tone, text, spinner: false, progress: null, cancelLabel, ...extra });

	/**
	 * Which single condition a session's strip reports when several hold. A failed file
	 * outranks everything. Then the pause level, Hard > Blocked > Soft (the backend's
	 * priority), then file activity Retrying > Uploading > Hashing, then Preparing >
	 * Capturing. Hard and blocked pause rows apply only once the game has exited; while a
	 * session is Launched/Running its own "Capturing" line is the useful one. Soft pause
	 * sits below file activity so an in-flight upload keeps showing progress.
	 */
	const describe = (s: CaptureSessionStatus, st: CaptureStatus) => {
		const byState = (state: CaptureFileStatus['state']) => s.files.filter((f) => f.state === state);
		const totalSize = sum(s.files, (f) => f.size);

		if (s.state === 'closed') {
			return makeStrip(
				'green',
				`Uploaded ${s.files.length} capture files (${formatBytes(totalSize)}) for ${s.playtest}`,
				null
			);
		}

		const failed = byState('failed')[0];
		if (failed) {
			return makeStrip(
				'red',
				`Could not upload ${failed.name}: ${failed.message ?? 'unknown error'}`
			);
		}

		const exited = s.state === 'exited';
		if (exited && st.pause === 'hard') {
			return makeStrip('yellow', 'Upload paused while a game is running');
		}
		if (exited && st.pause === 'blocked') {
			return makeStrip(
				'yellow',
				`Upload waiting: ${st.blockedReason ?? 'Capture upload is disabled'}`
			);
		}

		const retrying = byState('retrying')[0];
		if (retrying) {
			return makeStrip(
				'yellow',
				`Upload retrying ${retrying.name}: ${retrying.message ?? 'error'}. Next try ${formatTime(
					retrying.nextAttemptAt
				)}`
			);
		}

		if (byState('uploading').length > 0) {
			const pct = totalSize > 0 ? Math.floor((sum(s.files, doneBytes) / totalSize) * 100) : 0;
			const rate = sum(byState('uploading'), (f) => f.bytesPerSec ?? 0);
			const rateText = rate > 0 ? `, ${formatBytes(rate)}/s` : '';
			return makeStrip(
				'neutral',
				`Uploading ${s.playtest}: ${byState('uploaded').length}/${
					s.files.length
				} files, ${pct}% of ${formatBytes(totalSize)}${rateText}`,
				'Cancel',
				{ progress: pct }
			);
		}

		const hashing = byState('hashing')[0];
		if (hashing) {
			return makeStrip(
				'neutral',
				`Checking ${hashing.name} (${formatBytes(hashing.size)})`,
				'Cancel',
				{ spinner: true }
			);
		}

		if (exited && st.pause === 'soft') {
			return makeStrip('neutral', 'Upload paused briefly (repository operation running)');
		}

		if (exited && byState('waiting').length > 0) {
			return makeStrip(
				'neutral',
				`Preparing ${s.playtest}: ${s.files.length} files, ${formatBytes(totalSize)}`,
				"Don't upload"
			);
		}

		if (exited) {
			return makeStrip(
				'neutral',
				`Uploading ${s.playtest}: ${byState('uploaded').length}/${s.files.length} files`
			);
		}

		return makeStrip(
			'neutral',
			`Capturing ${s.playtest}: logs and traces upload after you quit the game`,
			"Don't upload"
		);
	};

	let strips: Strip[] = [];
	let hiddenCount = 0;

	$: {
		const visible = (status?.sessions ?? [])
			.filter((s) => isVisible(s, now))
			.sort((a, b) => Date.parse(b.launchedAt) - Date.parse(a.launchedAt));
		hiddenCount = Math.max(0, visible.length - MAX_STRIPS);
		strips = status
			? visible
					.slice(0, MAX_STRIPS)
					.map((s) => ({ id: s.id, files: s.files, ...describe(s, status as CaptureStatus) }))
			: [];
	}

	const applyStatus = (next: CaptureStatus) => {
		const at = Date.now();
		for (const s of next.sessions) {
			if (s.state === 'closed' && closedSeenAt[s.id] === undefined) {
				const exitedAt = s.exitedAt ? Date.parse(s.exitedAt) : 0;
				const firstLoad = status === null;
				closedSeenAt[s.id] = firstLoad && at - exitedAt > UPLOADED_LINGER_MS ? 0 : at;
			}
		}
		now = at;
		status = next;
	};

	const handleCancel = async (id: string) => {
		try {
			await cancelCapture(id);
			applyStatus(await getCaptureStatus());
		} catch {
			// The next capture-event reports the real state; a failed cancel needs no toast.
		}
	};

	const toggle = (id: string) => {
		expanded = { ...expanded, [id]: !expanded[id] };
	};

	onMount(async () => {
		timer = setInterval(() => {
			now = Date.now();
		}, 5000);
		try {
			const off = await listen<CaptureStatus>('capture-event', (event) => {
				applyStatus(event.payload);
			});
			if (destroyed) off();
			else unlisten = off;
			applyStatus(await getCaptureStatus());
		} catch {
			status = null;
		}
	});

	onDestroy(() => {
		destroyed = true;
		unlisten?.();
		if (timer) clearInterval(timer);
	});
</script>

{#each strips as strip, index (strip.id)}
	{#if expanded[strip.id] && strip.files.length > 0}
		<div class="flex flex-col gap-1 w-full px-2 py-1 border-t z-50 {toneClass.neutral}">
			{#each strip.files as file (file.name)}
				<div class="flex gap-2 items-center text-xs">
					<code class="truncate w-1/3" title={file.name}>{file.name}</code>
					<code class="w-16 text-nowrap">{formatBytes(file.size)}</code>
					<code class="w-20 text-nowrap">{file.state}</code>
					<div class="w-32">
						<Progressbar progress={percent(file)} size="h-1" />
					</div>
				</div>
			{/each}
		</div>
	{/if}
	<!-- svelte-ignore a11y-click-events-have-key-events a11y-no-static-element-interactions -->
	<div
		class="flex gap-2 items-center h-6 max-h-6 w-full py-1 px-2 z-50 border-t cursor-pointer {toneClass[
			strip.tone
		]}"
		on:click={() => {
			toggle(strip.id);
		}}
	>
		{#if strip.spinner}
			<Spinner size="3" />
		{/if}
		<code class="text-xs text-nowrap truncate">
			{strip.text}{status?.persistFailed ? ' - Cannot save upload progress (disk full?)' : ''}
		</code>
		{#if strip.progress !== null}
			<div class="w-24 shrink-0">
				<Progressbar progress={strip.progress} size="h-1" />
			</div>
		{/if}
		<div class="w-full" />
		{#if hiddenCount > 0 && index === strips.length - 1}
			<code class="text-xs text-nowrap">+{hiddenCount} more</code>
		{/if}
		{#if strip.cancelLabel}
			<Button
				outline
				color="dark"
				size="xs"
				class="p-1 my-1 border-0 whitespace-nowrap hover:bg-black/20 focus-within:ring-0 dark:focus-within:ring-0"
				on:click={(e) => {
					e.stopPropagation();
					void handleCancel(strip.id);
				}}
			>
				{strip.cancelLabel}
			</Button>
		{/if}
	</div>
{/each}
