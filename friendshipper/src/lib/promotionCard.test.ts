import { describe, expect, it } from 'vitest';
import {
	backendPlanText,
	cancelTooLate,
	cancellingText,
	endSummary,
	flipBuildId,
	gameConfigNote,
	historyLine,
	historyOutcome,
	isExpandedByDefault,
	localizeTimes,
	promotionCardPhase,
	promotionStepRows,
	splitEndMessage,
	trayDetail,
	typicalStepMs,
	waitingForPerson
} from '$lib/promotionCard';
import { localTime } from '$lib/fleetFormat';
import type {
	ChannelRef,
	FleetRelease,
	Promotion,
	PromotionPhase,
	PromotionStep,
	ShardConfigSpec
} from '$lib/types';

const SHA = '1234567890abcdef1234567890abcdef12345678';
const PREVIOUS = 'abcdef1234567890abcdef1234567890abcdef12';
const MINUTE = 60_000;
const NOW = Date.UTC(2026, 0, 2, 12, 0, 0);
const ago = (ms: number) => new Date(NOW - ms).toISOString();

const step = (name: string, phase: string, extra: Partial<PromotionStep> = {}): PromotionStep => ({
	name,
	workflow: '',
	phase,
	message: '',
	...extra
});

const promotion = (
	phase: PromotionPhase | null,
	steps: PromotionStep[] = [],
	{
		channel = { kind: 'launcher', metadataPath: 'test/sandbox' } as ChannelRef,
		annotations = {} as Record<string, string>,
		message = '',
		name = 'test-launcher-abcde'
	} = {}
): Promotion => ({
	metadata: {
		name,
		creationTimestamp: ago(10 * MINUTE),
		annotations: { 'fleet.believer.dev/changed-by': 'alice', ...annotations }
	},
	spec: { shard: 'test', channel, version: SHA, deployBackend: true },
	status: {
		phase,
		reason: '',
		message,
		previousVersion: PREVIOUS,
		steps,
		startedAt: ago(9 * MINUTE),
		finishedAt: phase && phase !== 'Running' && phase !== 'Pending' ? ago(MINUTE) : null,
		conditions: []
	}
});

const ctx = { history: [], releases: [], fleets: null, now: NOW };

describe('promotion card', () => {
	it('puts coordinator times in local time', () => {
		const at = '2026-01-03T19:19:00Z';
		expect(localizeTimes(`retires on its TTL at ${at} unless used`, NOW)).toBe(
			`retires on its TTL at ${localTime(at, NOW)} unless used`
		);
	});

	it('shows the backend plan once, on the deploy-backend row', () => {
		const p = promotion('Running', [
			step('deploy-backend', 'Running', { workflow: 'deploy-x', startedAt: ago(MINUTE) }),
			step('publish-client', 'Pending', { workflow: '' })
		]);
		const { status } = p;
		if (!status) throw new Error('fixture');
		status.plan = {
			backend: {
				deploy: true,
				parts: [
					{
						name: 'content',
						from: '2026-01-01-00-00-00_1.0_main_aaaaaaaa11',
						to: '2026-01-02-00-00-00_1.0_main_bbbbbbbb22'
					},
					{ name: 'platform', from: 'same', to: 'same' }
				]
			}
		};
		expect(backendPlanText(p)).toBe('content aaaaaaaa → bbbbbbbb');
		const [deploy, publish] = promotionStepRows(p, ctx);
		expect(deploy).toMatchObject({
			state: 'running',
			detail: 'content aaaaaaaa → bbbbbbbb',
			workflow: 'deploy-x',
			elapsed: '1 m 0 s'
		});
		// A pending step links nothing yet.
		expect(publish.workflow).toBeNull();
	});

	it('shows a cancelled step as stopped, and the rest as not run', () => {
		const p = promotion('Cancelled', [
			step('deploy-backend', 'Succeeded'),
			step('publish-client', 'Failed', { message: 'cancel requested by bob', workflow: 'w' }),
			step('flip-channel', 'Skipped', { message: 'the promotion ended before this step ran' })
		]);
		expect(promotionStepRows(p, ctx).map((r) => [r.state, r.detail])).toEqual([
			['succeeded', ''],
			['stopped', 'stopped by the cancel'],
			['notRun', 'not run']
		]);
		// Only a step that started links its Workflow.
		expect(promotionStepRows(p, ctx).map((r) => r.workflow)).toEqual([null, null, null]);
		const started = promotion('Running', [
			step('deploy-backend', 'Skipped', { workflow: 'deploy-x', startedAt: ago(MINUTE) }),
			step('publish-client', 'Running', { workflow: 'publish-x', startedAt: ago(MINUTE) })
		]);
		expect(promotionStepRows(started, ctx).map((r) => r.workflow)).toEqual([null, 'publish-x']);
		expect(historyOutcome(p)).toBe('Cancelled during Publish client · nothing reached players');
	});

	it('links the warm-fleets step to its release', () => {
		const p = promotion('Running', [
			step('warm-fleets', 'Running', { fleetRelease: 'test-12345678' })
		]);
		const { status } = p;
		if (!status) throw new Error('fixture');
		status.fleetRelease = 'test-12345678';
		expect(promotionStepRows(p, ctx)[0].release).toBe('test-12345678');
	});

	it('gives a pending step the usual duration on this shard', () => {
		const done = (minutes: number) =>
			promotion('Succeeded', [
				step('publish-client', 'Succeeded', {
					startedAt: ago(60 * MINUTE),
					finishedAt: ago((60 - minutes) * MINUTE)
				})
			]);
		const history = [done(2), done(3), done(10)];
		expect(typicalStepMs(history, 'publish-client')).toBe(3 * MINUTE);
		expect(typicalStepMs([], 'publish-client')).toBeNull();
		const p = promotion('Running', [step('publish-client', 'Pending')]);
		expect(promotionStepRows(p, { ...ctx, history })[0].typical).toBe('usually ~3 m');
	});

	it('says who is cancelling, and when a cancel came too late', () => {
		const cancelling = promotion('Running', [], {
			annotations: {
				'fleet.believer.dev/cancel-requested-at': ago(2 * MINUTE),
				'fleet.believer.dev/cancel-requested-by': 'bob'
			}
		});
		expect(cancellingText(cancelling, NOW)).toBe('by bob, 2 m ago');
		expect(trayDetail(cancelling, NOW)).toBe('Cancelling… · alice · 9 m 0 s');
		if (cancelling.status) cancelling.status.currentStep = 'publish-client';
		expect(trayDetail(cancelling, NOW)).toBe('cancelling during Publish client · alice · 9 m 0 s');
		// The step's detail comes last, so a long one doesn't hide who and how long.
		const busy = promotion('Running', [
			step('publish-client', 'Running', { message: 'uploading' })
		]);
		if (busy.status) busy.status.currentStep = 'publish-client';
		expect(trayDetail(busy, NOW)).toBe('Publish client · alice · 9 m 0 s · uploading');
		const late = promotion('Succeeded', [], {
			annotations: { 'fleet.believer.dev/cancel-requested-at': ago(MINUTE) }
		});
		expect(cancelTooLate(late)).toBe(true);
		expect(cancelTooLate(promotion('Succeeded'))).toBe(false);
		expect(historyOutcome(late)).toBe('cancel came too late · completed · backend skipped');
	});

	it('waits for a person to set a Steam public build live', () => {
		const p = promotion(
			'Running',
			[step('flip-channel', 'Running', { outputs: { build_id: '1234' } })],
			{ channel: { kind: 'steam', branch: 'public' } }
		);
		expect(waitingForPerson(p)).toBe(true);
		expect(promotionCardPhase(p)).toBe('Waiting for you');
		expect(flipBuildId(p)).toBe('1234');
		expect(isExpandedByDefault(p)).toBe(true);
		expect(trayDetail(p, NOW)).toBe('waiting for you · alice · 9 m 0 s');
		// A Workflow flips any other branch.
		const sandbox = promotion(
			'Running',
			[step('flip-channel', 'Running', { workflow: 'flip-x' })],
			{ channel: { kind: 'steam', branch: 'sandbox' } }
		);
		expect(waitingForPerson(sandbox)).toBe(false);
	});

	it('shows the game config only when it isn’t the channel default', () => {
		const p = promotion('Succeeded');
		const { status } = p;
		if (!status) throw new Error('fixture');
		status.gameConfig = 'Test';
		const config = {
			channels: [{ kind: 'launcher', metadataPath: 'test/sandbox', gameConfig: 'Test' }]
		} as ShardConfigSpec;
		expect(gameConfigNote(p, config)).toBe('');
		status.gameConfig = 'Shipping';
		expect(gameConfigNote(p, config)).toBe('Game config Shipping');
	});

	it('summarises an ended promotion in two lines', () => {
		const message =
			'flip-channel: Steam rejected the build: backend deployed; release test-12345678 kept, retires on its TTL unless used. Client not published, channel unchanged';
		expect(splitEndMessage(message)).toEqual({
			cause: 'flip-channel: Steam rejected the build',
			summary:
				'backend deployed; release test-12345678 kept, retires on its TTL unless used. Client not published, channel unchanged'
		});
		const failed = promotion('Failed', [], { message });
		const release = {
			metadata: { name: 'test-12345678' },
			status: { warmUntil: '2026-01-03T19:19:00Z' }
		} as FleetRelease;
		expect(endSummary(failed, [release], NOW)).toEqual([
			'Changed: backend deployed. Not done: client not published, channel unchanged.',
			`Left: release test-12345678 kept, expires ${localTime(
				'2026-01-03T19:19:00Z',
				NOW
			)} unless used.`
		]);
		expect(historyOutcome(failed)).toBe(
			'Failed: flip-channel: Steam rejected the build · channel unchanged'
		);

		const quiet = promotion('Cancelled', [], {
			message:
				'cancel requested by bob: release test-12345678 kept, retires on its TTL unless used. Client not published, channel unchanged'
		});
		expect(endSummary(quiet, [], NOW)).toEqual([
			'Nothing reached players: client not published, channel unchanged.',
			'Left: release test-12345678 kept, retires on its TTL unless used.'
		]);
		const nothing = promotion('Cancelled', [], {
			message: 'cancel requested by bob: nothing changed outside the cluster'
		});
		expect(endSummary(nothing, [], NOW)).toEqual([
			'Nothing reached players: nothing changed outside the cluster.'
		]);
		expect(endSummary(promotion('Succeeded'), [], NOW)).toEqual([]);
	});

	it('gives a finished promotion one line', () => {
		const p = promotion('Succeeded', [step('deploy-backend', 'Skipped')]);
		expect(historyLine(p, NOW)).toEqual({
			icon: '●',
			tone: 'ok',
			channel: 'launcher: test/sandbox',
			versions: 'abcdef12 → 12345678',
			who: 'alice',
			when: '1 m ago',
			took: '8 m 0 s',
			outcome: 'backend skipped'
		});
		expect(isExpandedByDefault(p)).toBe(false);
	});
});
