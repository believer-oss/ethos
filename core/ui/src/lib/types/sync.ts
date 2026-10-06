/**
 * Artifact download events, emitted by the backend on the `sync-event` channel.
 *
 * Mirrors `ethos_core::artifact_sync`. The Rust side has a test asserting the tag and
 * field names here still match what it serialises.
 */

export type SyncKind = 'client' | 'engine' | 'editorDlls';

export type SyncErrorClass =
	| 'cancelled'
	| 'notFound'
	| 'unauthorized'
	| 'transient'
	| 'invalidInput'
	| 'corrupt'
	/** A file is open in another program. Closing it and retrying is the fix. */
	| 'blocked'
	| 'io'
	| 'internal';

export interface SyncProgress {
	/** A total of 0 means that dimension is not known yet - show it as indeterminate. */
	doneBytes: number;
	totalBytes: number;
	doneItems: number;
	totalItems: number;
	/** longtail's own phase, e.g. "Updating version". */
	phase: string;
}

export interface SyncSummary {
	bytesWritten: number;
	assetsWritten: number;
	assetsRemoved: number;
	blocksFetched: number;
}

export interface SyncError {
	class: SyncErrorClass;
	/** One or two sentences to show the user. */
	summary: string;
	/** The cause chain, for a disclosure or a copy button. */
	detail: string;
}

export type SyncEvent =
	| { type: 'started'; kind: SyncKind }
	| { type: 'progress'; kind: SyncKind; progress: SyncProgress }
	| { type: 'finished'; kind: SyncKind; summary: SyncSummary }
	/**
	 * The artifact is in place and recorded as installed. `finished` only means the
	 * transfer ended - the editor binaries are still being copied into the repo at that
	 * point, so anything reading what is installed must wait for this.
	 */
	| { type: 'installed'; kind: SyncKind }
	| { type: 'failed'; kind: SyncKind; error: SyncError }
	| { type: 'cancelled'; kind: SyncKind };

export const syncKindLabel = (kind: SyncKind): string => {
	switch (kind) {
		case 'client':
			return 'game client';
		case 'engine':
			return 'engine';
		case 'editorDlls':
			return 'editor binaries';
		default:
			return kind;
	}
};

/**
 * Running view of one download, derived from the progress stream.
 *
 * Rate and ETA have to be computed here: they used to be scraped out of longtail's
 * progress-bar text, which no longer exists now that progress arrives as numbers.
 */
export class SyncTracker {
	private startedAt = Date.now();

	private firstBytes: number | null = null;

	private firstBytesAt = 0;

	/** 0-100, or null when the current phase reports no total. */
	percent: number | null = null;

	phase = '';

	doneItems = 0;

	totalItems = 0;

	bytesPerSecond = 0;

	reset(): void {
		this.startedAt = Date.now();
		this.firstBytes = null;
		this.firstBytesAt = 0;
		this.percent = null;
		this.phase = '';
		this.doneItems = 0;
		this.totalItems = 0;
		this.bytesPerSecond = 0;
	}

	update(progress: SyncProgress): void {
		this.phase = progress.phase;
		this.doneItems = progress.doneItems;
		this.totalItems = progress.totalItems;

		this.percent =
			progress.totalBytes > 0
				? Math.min(100, (progress.doneBytes / progress.totalBytes) * 100)
				: null;

		// Measure from the first sample that carried bytes rather than from the start, so
		// a long index-reading phase does not drag the average down for the whole run.
		if (progress.doneBytes > 0) {
			if (this.firstBytes === null) {
				this.firstBytes = progress.doneBytes;
				this.firstBytesAt = Date.now();
			} else {
				const elapsed = (Date.now() - this.firstBytesAt) / 1000;
				if (elapsed > 0.5) {
					this.bytesPerSecond = (progress.doneBytes - this.firstBytes) / elapsed;
				}
			}
		}
	}

	get elapsedSeconds(): number {
		return (Date.now() - this.startedAt) / 1000;
	}

	/** Seconds remaining, or null when there is not enough to go on. */
	remainingSeconds(progress: SyncProgress): number | null {
		if (progress.totalBytes <= 0 || this.bytesPerSecond <= 0) return null;
		const left = progress.totalBytes - progress.doneBytes;
		return left > 0 ? left / this.bytesPerSecond : 0;
	}
}

export const formatDuration = (seconds: number | null): string => {
	if (seconds === null || !Number.isFinite(seconds)) return '--';
	const total = Math.max(0, Math.round(seconds));
	const h = Math.floor(total / 3600);
	const m = Math.floor((total % 3600) / 60);
	const s = total % 60;
	if (h > 0) return `${h}h${m}m`;
	if (m > 0) return `${m}m${s}s`;
	return `${s}s`;
};

export const formatBytes = (bytes: number): string => {
	if (bytes <= 0) return '0 B';
	const units = ['B', 'KB', 'MB', 'GB', 'TB'];
	const i = Math.min(units.length - 1, Math.floor(Math.log(bytes) / Math.log(1024)));
	const value = bytes / 1024 ** i;
	return `${value.toFixed(i === 0 ? 0 : 1)} ${units[i]}`;
};
