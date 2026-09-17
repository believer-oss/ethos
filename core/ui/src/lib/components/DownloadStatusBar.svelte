<script lang="ts">
	import { Button, Progressbar, Spinner } from 'flowbite-svelte';
	import { CloseOutline } from 'flowbite-svelte-icons';
	import { listen } from '@tauri-apps/api/event';
	import {
		SyncTracker,
		formatBytes,
		formatDuration,
		syncKindLabel,
		type SyncEvent,
		type SyncKind
	} from '../types/sync.js';

	/**
	 * Cancel one download. Cancelling keeps whatever has been fetched, so re-running the
	 * same sync resumes rather than starting over - which is what "pause" means here.
	 */
	export let onCancel: (kind: SyncKind) => void = () => {};

	/** How long a finished or failed row stays up before it disappears. */
	export let settleMs = 4000;

	interface Row {
		kind: SyncKind;
		tracker: SyncTracker;
		/** null while the current phase reports no byte total - render indeterminate. */
		percent: number | null;
		phase: string;
		elapsed: string;
		remaining: string;
		rate: string;
		state: 'running' | 'finished' | 'cancelled' | 'failed';
		message: string;
	}

	// Keyed by kind so the three downloads occupy at most one row each, and so an event
	// can be attributed without guessing which download it came from.
	let rows = new Map<SyncKind, Row>();

	const timers = new Map<SyncKind, ReturnType<typeof setTimeout>>();

	const retire = (kind: SyncKind) => {
		const existing = timers.get(kind);
		if (existing) clearTimeout(existing);
		timers.set(
			kind,
			setTimeout(() => {
				rows.delete(kind);
				rows = rows;
				timers.delete(kind);
			}, settleMs)
		);
	};

	const blank = (kind: SyncKind): Row => ({
		kind,
		tracker: new SyncTracker(),
		percent: null,
		phase: '',
		elapsed: '',
		remaining: '',
		rate: '',
		state: 'running',
		message: ''
	});

	void listen<SyncEvent>('sync-event', (event) => {
		const { payload } = event;
		const row = rows.get(payload.kind) ?? blank(payload.kind);

		switch (payload.type) {
			case 'started': {
				const fresh = blank(payload.kind);
				rows.set(payload.kind, fresh);
				const pending = timers.get(payload.kind);
				if (pending) {
					clearTimeout(pending);
					timers.delete(payload.kind);
				}
				break;
			}
			case 'progress': {
				row.tracker.update(payload.progress);
				row.percent = row.tracker.percent;
				row.phase = row.tracker.phase;
				row.elapsed = formatDuration(row.tracker.elapsedSeconds);
				row.remaining = formatDuration(row.tracker.remainingSeconds(payload.progress));
				row.rate =
					row.tracker.bytesPerSecond > 0 ? `${formatBytes(row.tracker.bytesPerSecond)}/s` : '';
				row.state = 'running';
				rows.set(payload.kind, row);
				break;
			}
			case 'finished': {
				row.state = 'finished';
				row.percent = 100;
				row.message = `${formatBytes(payload.summary.bytesWritten)} written`;
				rows.set(payload.kind, row);
				retire(payload.kind);
				break;
			}
			case 'cancelled': {
				row.state = 'cancelled';
				row.message = 'Cancelled - progress is kept, syncing again resumes';
				rows.set(payload.kind, row);
				retire(payload.kind);
				break;
			}
			case 'failed': {
				row.state = 'failed';
				row.message = payload.error.summary;
				rows.set(payload.kind, row);
				retire(payload.kind);
				break;
			}
			default:
				break;
		}

		rows = rows;
	});
</script>

{#each [...rows.values()] as row (row.kind)}
	<div
		class="flex gap-2 items-center bg-secondary-700 dark:bg-space-900 h-6 max-h-6 w-full py-1 px-2 z-50"
	>
		<code class="text-xs text-gray-400 dark:text-gray-400 text-nowrap">
			{syncKindLabel(row.kind)}
		</code>

		{#if row.state === 'running'}
			<Spinner size="2" />
			<code class="text-xs text-gray-500 dark:text-gray-500 text-nowrap truncate max-w-48">
				{row.phase}
			</code>
			{#if row.percent !== null}
				<Progressbar progress={row.percent} size="h-1" />
			{:else}
				<!-- The phase reports no total, so there is nothing honest to fill a bar with. -->
				<div class="w-full" />
			{/if}
			<code class="text-xs text-gray-400 dark:text-gray-400 text-nowrap">
				{row.elapsed} / {row.remaining}{row.rate ? ` · ${row.rate}` : ''}
			</code>
			<Button
				outline
				color="dark"
				size="xs"
				title="Cancel - progress is kept, so syncing again resumes"
				class="p-1 my-1 hover:bg-secondary-800 text-gray-400 dark:hover:bg-space-950 border-0 focus-within:ring-0 dark:focus-within:ring-0 focus-within:bg-secondary-800 dark:focus-within:bg-space-950"
				on:click={() => {
					onCancel(row.kind);
				}}
			>
				<CloseOutline class="h-3 w-3" />
			</Button>
		{:else}
			<code
				class="text-xs text-nowrap truncate {row.state === 'failed'
					? 'text-red-400'
					: 'text-gray-400 dark:text-gray-400'}"
			>
				{row.message}
			</code>
			<div class="w-full" />
		{/if}
	</div>
{/each}
