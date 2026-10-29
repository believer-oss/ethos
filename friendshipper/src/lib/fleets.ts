import { invoke } from '@tauri-apps/api/core';
import { derived, get, writable } from 'svelte/store';
import type {
	CreatePromotionRequest,
	Nullable,
	Promotion,
	ShardDetail,
	ShardSummary
} from '$lib/types';
import {
	ACTIVE_POLL_MS,
	IDLE_POLL_MS,
	isActivePromotion,
	PromotionTransitions,
	type PromotionToast
} from '$lib/fleetFormat';
import { dynamicConfig } from '$lib/stores';
import { logInfo } from '$lib/utils';

export const getShards = async (): Promise<ShardSummary[]> => invoke('get_shards');

export const getShard = async (name: string): Promise<ShardDetail> => invoke('get_shard', { name });

export const getPromotions = async (shard?: string, channel?: string): Promise<Promotion[]> =>
	invoke('get_promotions', { shard, channel });

export const createPromotion = async (request: CreatePromotionRequest): Promise<Promotion> =>
	invoke('create_promotion', { request });

/** The name Friendshipper writes as changed-by and cancel-requested-by, if one is set. */
export const getPromoterName = async (): Promise<Nullable<string>> => invoke('get_promoter_name');

/** Asks the coordinator to cancel; the promotion stays, and finishes as Cancelled or its real result. */
export const cancelPromotion = async (name: string): Promise<void> =>
	invoke('cancel_promotion', { name });

export const promotionViaCoordinator = derived(
	dynamicConfig,
	($dynamicConfig) => $dynamicConfig.promotionViaCoordinator === true
);

// Every shard and promotion, refreshed by the poller below.
export const fleetShards = writable(<ShardSummary[]>[]);
export const fleetPromotions = writable(<Promotion[]>[]);
// Whether Shard objects can be listed: true after a successful poll, false again after a few
// failures in a row. With the switch off, the Shards nav item shows only when this is true.
export const fleetsAvailable = writable(false);
// Why the last poll failed, cleared by the next success.
export const fleetError = writable('');
// Bumped after each successful poll, so pages showing their own copy know to refetch.
export const fleetRevision = writable(0);

export const activePromotions = derived(fleetPromotions, ($p) => $p.filter(isActivePromotion));

// Failures in a row before fleetsAvailable goes false, so one blip doesn't hide anything.
const FAILURES_BEFORE_UNAVAILABLE = 3;
// While something watches, repeated failures slow polling down to this, no further.
const WATCHED_BACKOFF_MS = 60_000;
// The availability probe while nothing watches and fleets look unavailable.
const UNAVAILABLE_PROBE_MS = 5 * 60_000;

/**
 * Polls shards and promotions. With the switch on it polls all the time, because the bottom-bar
 * tray and the promotion toasts need it. With the switch off it probes at startup (for the Shards
 * nav item and Release controls), again every 5 minutes while that probe fails, and otherwise
 * polls only while a Shards page watches.
 */
class FleetPoller {
	private timer: ReturnType<typeof setTimeout> | null = null;

	private started = false;

	private switchOn = false;

	private watchers = 0;

	private inFlight: Promise<void> | null = null;

	private failures = 0;

	// Whether the last successful poll saw an active promotion, to poll faster while it runs.
	private sawActive = false;

	// Only while the switch is on: off, polls are too sparse to tell what changed when.
	private transitions: PromotionTransitions | null = null;

	private onToast: Nullable<(toast: PromotionToast) => void> = null;

	private unsubscribe: Nullable<() => void> = null;

	start(onToast: (toast: PromotionToast) => void) {
		this.onToast = onToast;
		if (this.started) return;
		this.started = true;
		this.unsubscribe = promotionViaCoordinator.subscribe((on) => {
			if (on === this.switchOn) return;
			this.switchOn = on;
			this.transitions = on ? new PromotionTransitions() : null;
			if (on) {
				this.pollInBackground();
			} else {
				this.schedule();
			}
		});
		// The startup probe; with the switch on it is also the first poll.
		this.pollInBackground();
	}

	stop() {
		this.started = false;
		this.unsubscribe?.();
		this.unsubscribe = null;
		this.switchOn = false;
		this.transitions = null;
		this.clearTimer();
		fleetPromotions.set([]);
	}

	/** Keeps polling while the returned function hasn't been called. */
	watch(): () => void {
		const wasWatched = this.watched();
		this.watchers += 1;
		// With nothing watching, the lists may be old or cleared, so fetch them now.
		if (!wasWatched && this.started) this.pollInBackground();
		let released = false;
		return () => {
			if (released) return;
			released = true;
			this.watchers -= 1;
			this.schedule();
		};
	}

	/** Polls now. A poll already in flight may predate the change, so wait for it, then poll. */
	async refresh() {
		if (this.inFlight) await this.inFlight;
		await this.poll();
	}

	private watched(): boolean {
		return this.switchOn || this.watchers > 0;
	}

	/** The delay to the next poll, or null to stop polling. */
	private nextDelay(): Nullable<number> {
		if (!this.started) return null;
		if (this.watched()) {
			if (this.failures >= FAILURES_BEFORE_UNAVAILABLE) return WATCHED_BACKOFF_MS;
			if (this.failures > 0) return IDLE_POLL_MS;
			return this.sawActive ? ACTIVE_POLL_MS : IDLE_POLL_MS;
		}
		return get(fleetsAvailable) ? null : UNAVAILABLE_PROBE_MS;
	}

	private clearTimer() {
		if (this.timer) clearTimeout(this.timer);
		this.timer = null;
	}

	private schedule() {
		this.clearTimer();
		if (this.inFlight) return;
		const delay = this.nextDelay();
		if (delay === null) {
			// Nothing keeps promotions current now, so don't leave a stale list for the tray.
			fleetPromotions.set([]);
			return;
		}
		this.timer = setTimeout(() => {
			this.pollInBackground();
		}, delay);
	}

	private pollInBackground() {
		this.poll().catch(() => {
			// fetch logs its own failures.
		});
	}

	// Joins the poll in flight rather than starting a second one.
	private poll(): Promise<void> {
		if (!this.inFlight) {
			this.clearTimer();
			this.inFlight = this.fetch().finally(() => {
				this.inFlight = null;
				this.schedule();
			});
		}
		return this.inFlight;
	}

	private async fetch() {
		try {
			const [shards, promotions] = await Promise.all([getShards(), getPromotions()]);
			fleetShards.set(shards);
			fleetPromotions.set(promotions);
			fleetsAvailable.set(true);
			fleetError.set('');
			fleetRevision.update((n) => n + 1);
			this.failures = 0;
			if (this.transitions) {
				// Read each time: connecting GitHub or changing the display name changes it.
				const localName = await getPromoterName().catch(() => null);
				this.transitions.update(promotions, localName).forEach((t) => {
					this.onToast?.(t);
				});
			}
			this.sawActive = promotions.some(isActivePromotion);
		} catch (e) {
			const message = (e as { message?: string }).message ?? String(e);
			fleetError.set(message);
			this.failures += 1;
			if (this.failures >= FAILURES_BEFORE_UNAVAILABLE) fleetsAvailable.set(false);
			// Shown on the Shards pages and in the promote form, so log rather than toast.
			await logInfo(`Failed to refresh shards and promotions: ${message}`);
		}
	}
}

const poller = new FleetPoller();

/** Starts the poller: a startup probe, then polling as the switch and open pages need. */
export const startFleetPolling = (toast: (toast: PromotionToast) => void) => {
	poller.start(toast);
};

export const stopFleetPolling = () => {
	poller.stop();
};

/** For pages that show fleet state: keeps the poller running until the returned function runs. */
export const watchFleets = (): (() => void) => poller.watch();

/** Polls now, e.g. after creating or cancelling a promotion, and resolves once it has. */
export const refreshFleets = async () => poller.refresh();
