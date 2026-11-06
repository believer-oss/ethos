import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { get, writable } from 'svelte/store';

// The poller under test talks to the backend through invoke; each test scripts its replies.
const invoke = vi.fn();
const dynamicConfig = writable({ promotionViaCoordinator: false });

vi.mock('@tauri-apps/api/core', () => ({ invoke }));
vi.mock('$lib/stores', () => ({ dynamicConfig }));
vi.mock('$lib/utils', () => ({ logInfo: vi.fn(async () => {}) }));

type Fleets = typeof import('$lib/fleets');

// A fresh module per test, so each gets its own poller.
const load = async (): Promise<Fleets> => {
	vi.resetModules();
	return import('$lib/fleets');
};

const succeed = () => {
	invoke.mockImplementation(() => Promise.resolve([]));
};

const fail = () => {
	invoke.mockImplementation(() => Promise.reject(new Error('forbidden')));
};

// A promotion the coordinator hasn't finished, for the tray.
const running = {
	metadata: { name: 'test-steam-abcde' },
	spec: { shard: 'test', channel: { kind: 'steam', branch: 'test' } },
	status: { phase: 'Running' }
};

const withPromotions = (promotions: unknown[]) => {
	invoke.mockImplementation((command: string) =>
		Promise.resolve(command === 'get_promotions' ? promotions : [])
	);
};

// Each poll lists shards and promotions.
const polls = () => invoke.mock.calls.filter(([command]) => command === 'get_shards').length;

describe('fleet poller', () => {
	let fleets: Fleets;

	beforeEach(async () => {
		vi.useFakeTimers();
		invoke.mockReset();
		dynamicConfig.set({ promotionViaCoordinator: false });
		fleets = await load();
	});

	afterEach(() => {
		fleets.stopFleetPolling();
		vi.useRealTimers();
	});

	it('probes once with the switch off, then polls only while watched', async () => {
		succeed();
		fleets.startFleetPolling(() => {});
		await vi.advanceTimersByTimeAsync(0);
		expect(polls()).toBe(1);
		expect(get(fleets.fleetsAvailable)).toBe(true);

		await vi.advanceTimersByTimeAsync(10 * 60_000);
		expect(polls()).toBe(1);

		// Watching fetches at once, then on the usual cadence.
		const release = fleets.watchFleets();
		await vi.advanceTimersByTimeAsync(0);
		expect(polls()).toBe(2);
		await vi.advanceTimersByTimeAsync(30_000);
		expect(polls()).toBe(3);

		release();
		await vi.advanceTimersByTimeAsync(10 * 60_000);
		expect(polls()).toBe(3);
	});

	it('polls all the time with the switch on', async () => {
		succeed();
		dynamicConfig.set({ promotionViaCoordinator: true });
		fleets.startFleetPolling(() => {});
		await vi.advanceTimersByTimeAsync(0);
		expect(polls()).toBe(1);
		await vi.advanceTimersByTimeAsync(90_000);
		expect(polls()).toBe(4);
	});

	it('stops and clears promotions when the switch goes off, and resumes when it comes back', async () => {
		withPromotions([running]);
		dynamicConfig.set({ promotionViaCoordinator: true });
		fleets.startFleetPolling(() => {});
		await vi.advanceTimersByTimeAsync(0);
		expect(get(fleets.activePromotions)).toHaveLength(1);
		// An active promotion polls faster.
		await vi.advanceTimersByTimeAsync(5_000);
		expect(polls()).toBe(2);

		dynamicConfig.set({ promotionViaCoordinator: false });
		await vi.advanceTimersByTimeAsync(0);
		expect(get(fleets.activePromotions)).toHaveLength(0);
		await vi.advanceTimersByTimeAsync(10 * 60_000);
		expect(polls()).toBe(2);

		dynamicConfig.set({ promotionViaCoordinator: true });
		await vi.advanceTimersByTimeAsync(0);
		expect(polls()).toBe(3);
		expect(get(fleets.activePromotions)).toHaveLength(1);
		await vi.advanceTimersByTimeAsync(5_000);
		expect(polls()).toBe(4);
	});

	it('backs off to a minute while watched, and recovers', async () => {
		succeed();
		dynamicConfig.set({ promotionViaCoordinator: true });
		fleets.startFleetPolling(() => {});
		await vi.advanceTimersByTimeAsync(0);
		expect(get(fleets.fleetsAvailable)).toBe(true);

		fail();
		await vi.advanceTimersByTimeAsync(30_000);
		expect(get(fleets.fleetError)).toBe('forbidden');
		expect(get(fleets.fleetsAvailable)).toBe(true);
		await vi.advanceTimersByTimeAsync(60_000);
		expect(polls()).toBe(4);
		expect(get(fleets.fleetsAvailable)).toBe(false);

		// Every minute now, not every 5.
		await vi.advanceTimersByTimeAsync(59_000);
		expect(polls()).toBe(4);
		await vi.advanceTimersByTimeAsync(1_000);
		expect(polls()).toBe(5);

		succeed();
		await fleets.refreshFleets();
		expect(get(fleets.fleetError)).toBe('');
		expect(get(fleets.fleetsAvailable)).toBe(true);
	});

	it('retries a failed startup probe every 5 minutes with the switch off', async () => {
		fail();
		fleets.startFleetPolling(() => {});
		await vi.advanceTimersByTimeAsync(0);
		expect(polls()).toBe(1);
		expect(get(fleets.fleetsAvailable)).toBe(false);

		await vi.advanceTimersByTimeAsync(5 * 60_000);
		expect(polls()).toBe(2);

		succeed();
		await vi.advanceTimersByTimeAsync(5 * 60_000);
		expect(polls()).toBe(3);
		expect(get(fleets.fleetsAvailable)).toBe(true);

		// Available again, so no more probes until something watches.
		await vi.advanceTimersByTimeAsync(10 * 60_000);
		expect(polls()).toBe(3);
	});

	it("toasts a promotion's finish to the local user who created it", async () => {
		const mine = (phase: string) => ({
			...running,
			metadata: { ...running.metadata, annotations: { 'fleet.believer.dev/changed-by': 'me' } },
			status: { phase }
		});
		let promotions = [mine('Running')];
		let name: string | null = 'me';
		invoke.mockImplementation((command: string) => {
			if (command === 'get_promotions') return Promise.resolve(promotions);
			if (command === 'get_promoter_name') return Promise.resolve(name);
			return Promise.resolve([]);
		});
		const toasts: unknown[] = [];
		dynamicConfig.set({ promotionViaCoordinator: true });
		fleets.startFleetPolling((toast) => toasts.push(toast));
		await vi.advanceTimersByTimeAsync(0);

		promotions = [mine('Succeeded')];
		await fleets.refreshFleets();
		expect(toasts).toHaveLength(1);

		// Someone else's name (or none) on this machine: no toast.
		name = null;
		promotions = [{ ...mine('Failed'), metadata: { ...mine('Failed').metadata, name: 'other' } }];
		await fleets.refreshFleets();
		expect(toasts).toHaveLength(1);
	});

	it('waits for a poll in flight, then polls again', async () => {
		// Holds both calls of the first poll until released.
		const pending: (() => void)[] = [];
		invoke.mockImplementation(
			() =>
				new Promise((resolve) => {
					pending.push(() => {
						resolve([]);
					});
				})
		);
		fleets.startFleetPolling(() => {});
		await vi.advanceTimersByTimeAsync(0);
		expect(polls()).toBe(1);

		let refreshed = false;
		const refresh = fleets.refreshFleets().then(() => {
			refreshed = true;
		});
		await vi.advanceTimersByTimeAsync(0);
		expect(refreshed).toBe(false);

		succeed();
		pending.forEach((resolve) => {
			resolve();
		});
		await refresh;
		expect(polls()).toBe(2);
	});
});
