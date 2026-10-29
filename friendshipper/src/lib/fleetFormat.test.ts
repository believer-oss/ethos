import { describe, expect, it } from 'vitest';
import {
	activeBuildsFromShards,
	backendParts,
	backendWarning,
	channelSinceText,
	channelKey,
	formatAge,
	formatBackendPart,
	formatElapsed,
	isActivePromotion,
	isCancelling,
	newestBuiltPart,
	observationWarning,
	parseBackendVersion,
	PromotionTransitions,
	promotionPhaseLabel,
	promotionPreview,
	retryingMessage,
	RELEASE_CONTROLS_TOOLTIP,
	runningPromotionsText,
	showPromotionTray,
	steamChannelText,
	writeControl
} from '$lib/fleetFormat';
import type {
	ChannelStatus,
	Promotion,
	PromotionPhase,
	PromotionSpec,
	ShardDetail,
	ShardSummary
} from '$lib/types';

const SHA = '0123456789abcdef0123456789abcdef01234567';
const HOUR = 60 * 60 * 1000;
const NOW = Date.UTC(2026, 0, 2, 12, 0, 0);

describe('parseBackendVersion', () => {
	it('splits the four segments', () => {
		const v = parseBackendVersion('2026-01-02-03-04-05_1.2.3_main_abc1234');
		expect(v).toMatchObject({ release: '1.2.3', branch: 'main', sha: 'abc1234' });
		expect(v?.builtAt?.toISOString()).toBe('2026-01-02T03:04:05.000Z');
	});

	it('lets the branch contain underscores', () => {
		const v = parseBackendVersion('2026-01-02-03-04-05_1.2.3_release_1_2_abc1234');
		expect(v?.branch).toBe('release_1_2');
		expect(v?.sha).toBe('abc1234');
	});

	it('rejects malformed versions', () => {
		for (const bad of [
			'',
			'abc1234',
			'2026-01-02_1.2.3_abc1234',
			'2026-01-02__main_abc1234',
			'2026-01-02_1.2.3_main_'
		]) {
			expect(parseBackendVersion(bad), bad).toBeNull();
		}
	});

	it('keeps an unparseable build time as null', () => {
		expect(parseBackendVersion('yesterday_1.2.3_main_abc1234')?.builtAt).toBeNull();
	});
});

describe('backend parts', () => {
	const parts = backendParts({
		platform: '2026-01-02-01-00-00_1.2.3_main_aaaaaaa',
		portal: null,
		content: '2026-01-02-03-00-00_1.2.3_main_ccccccc',
		config: '2026-01-01-00-00-00_1.2.3_main_ddddddd'
	});

	it('lists set parts in a fixed order', () => {
		expect(parts.map((p) => p.name)).toEqual(['platform', 'content', 'config']);
	});

	it('picks the newest build', () => {
		expect(newestBuiltPart(parts)?.name).toBe('content');
		expect(newestBuiltPart([])).toBeNull();
	});

	it('formats a part as name, short SHA and build age', () => {
		const content = newestBuiltPart(parts);
		expect(content && formatBackendPart(content, NOW)).toBe('content ccccccc · built 9 h ago');
	});

	it('shows an unparseable part as it is', () => {
		const [odd] = backendParts({ platform: 'v1' });
		expect(formatBackendPart(odd, NOW)).toBe('platform v1');
	});
});

describe('ages', () => {
	it('formats compactly', () => {
		expect(formatAge(new Date(NOW - 30_000), NOW)).toBe('now');
		expect(formatAge(new Date(NOW - 12 * 60_000), NOW)).toBe('12 m');
		expect(formatAge(new Date(NOW - 9 * HOUR), NOW)).toBe('9 h');
		expect(formatAge(new Date(NOW - 72 * HOUR), NOW)).toBe('3 d');
		expect(formatAge(new Date(NOW - 72 * HOUR).toISOString(), NOW)).toBe('3 d');
		expect(formatAge(null, NOW)).toBe('');
		expect(formatAge('not a date', NOW)).toBe('');
	});

	it('formats elapsed time', () => {
		expect(formatElapsed(new Date(NOW - 45_000).toISOString(), NOW)).toBe('45 s');
		expect(formatElapsed(new Date(NOW - 190_000).toISOString(), NOW)).toBe('3 m 10 s');
		expect(formatElapsed(new Date(NOW - 65 * 60_000).toISOString(), NOW)).toBe('1 h 5 m');
	});
});

describe('observationWarning', () => {
	const at = (msAgo: number) => new Date(NOW - msAgo).toISOString();

	it('is quiet for a fresh observation without error', () => {
		expect(observationWarning({ error: '', observedAt: at(60_000) }, NOW)).toBeNull();
	});

	it('warns on an error, whatever the age', () => {
		expect(observationWarning({ error: 'AccessDenied', observedAt: at(0) }, NOW)).toBe(
			'AccessDenied'
		);
	});

	it('says when the last success was, since observedAt only moves on success', () => {
		expect(observationWarning({ error: '', observedAt: at(3 * 60_000) }, NOW)).toBeNull();
		expect(observationWarning({ error: '', observedAt: at(4 * 60_000) }, NOW)).toBe(
			'Last successful observation 4 m ago'
		);
		expect(observationWarning({ error: 'HTTP 502', observedAt: at(20 * 60_000) }, NOW)).toBe(
			'HTTP 502; last successful observation 20 m ago'
		);
		expect(observationWarning({ error: '', observedAt: null }, NOW)).toBe(
			'Never observed successfully'
		);
	});
});

const steam = (fields: Partial<ChannelStatus>): ChannelStatus => ({
	key: 'steam/test',
	kind: 'steam',
	branch: 'test',
	version: '',
	error: '',
	...fields
});

describe('channels', () => {
	it('builds keys like the crate', () => {
		expect(channelKey({ kind: 'steam', branch: 'test' })).toBe('steam/test');
		expect(channelKey({ kind: 'launcher', metadataPath: 'stable-1.0' })).toBe(
			'launcher/stable-1.0'
		);
	});

	it('describes Steam channels', () => {
		expect(steamChannelText(steam({ version: SHA }))).toBe('test: 0123456');
		expect(steamChannelText(steam({ buildId: 1234, error: 'BuildNotFound' }))).toBe(
			'test: build 1234'
		);
		expect(steamChannelText(steam({}))).toBe('test: no build');
	});
});

// Who created the promotions below, unless a test says otherwise.
const CREATOR = 'creator';

const promotion = (
	name: string,
	phase: PromotionPhase | null,
	createdAt = new Date(NOW).toISOString(),
	message = ''
): Promotion => ({
	metadata: {
		name,
		creationTimestamp: createdAt,
		annotations: { 'fleet.believer.dev/changed-by': CREATOR }
	},
	spec: {
		shard: 'test',
		channel: { kind: 'steam', branch: 'test' },
		version: SHA,
		deployBackend: false
	},
	status: phase === null ? null : { phase, reason: '', message, steps: [], conditions: [] }
});

const cancelled = (p: Promotion, by = 'someone', at = '2026-01-02T12:00:00Z'): Promotion => ({
	...p,
	metadata: {
		...p.metadata,
		annotations: {
			...p.metadata.annotations,
			'fleet.believer.dev/cancel-requested-at': at,
			'fleet.believer.dev/cancel-requested-by': by
		}
	}
});

describe('promotion phases', () => {
	it('treats no status as submitted and active', () => {
		const submitted = promotion('a', null);
		expect(promotionPhaseLabel(submitted)).toBe('Submitted');
		expect(isActivePromotion(submitted)).toBe(true);
		expect(isActivePromotion(promotion('b', 'Running'))).toBe(true);
		expect(isActivePromotion(promotion('c', 'Rejected'))).toBe(false);
	});

	it('shows a requested cancel until the promotion finishes', () => {
		expect(promotionPhaseLabel(cancelled(promotion('a', null)))).toBe('Cancelling…');
		expect(promotionPhaseLabel(cancelled(promotion('b', 'Running')))).toBe('Cancelling…');
		expect(isCancelling(cancelled(promotion('b', 'Running')))).toBe(true);
		expect(isActivePromotion(cancelled(promotion('b', 'Running')))).toBe(true);
		expect(promotionPhaseLabel(cancelled(promotion('c', 'Cancelled')))).toBe(
			'Cancelled by someone'
		);
		// Finished before the cancel took: the real result, with the coordinator's message.
		const late = cancelled(promotion('d', 'Succeeded'));
		expect(promotionPhaseLabel(late)).toBe('Succeeded');
		expect(isCancelling(late)).toBe(false);

		// The time's presence is the request, whatever it holds; a blank name is unknown.
		expect(promotionPhaseLabel(cancelled(promotion('f', 'Running'), 'someone', 'soon'))).toBe(
			'Cancelling…'
		);
		expect(promotionPhaseLabel(cancelled(promotion('g', 'Cancelled'), ' '))).toBe('Cancelled');
		const notRequested = promotion('h', 'Running');
		notRequested.metadata.annotations = { 'fleet.believer.dev/cancel-requested-by': 'someone' };
		expect(isCancelling(notRequested)).toBe(false);

		const base = promotion('e', 'Cancelled');
		const deleted = { ...base, status: { ...base.status, reason: 'Deleted' } } as Promotion;
		expect(promotionPhaseLabel(deleted)).toBe('Cancelled (deleted)');
	});

	it('counts running promotions', () => {
		expect(runningPromotionsText(1)).toBe('1 promotion running');
		expect(runningPromotionsText(3)).toBe('3 promotions running');
	});
});

describe('PromotionTransitions', () => {
	const created = new Date(NOW).toISOString();

	it('treats the first poll as history', () => {
		const t = new PromotionTransitions();
		expect(
			t.update(
				[promotion('old', 'Succeeded', created), promotion('old2', 'Failed', created)],
				CREATOR
			)
		).toEqual([]);
		expect(t.update([promotion('old', 'Succeeded', created)], CREATOR)).toEqual([]);
	});

	it('toasts a promotion that finishes while the app is open', () => {
		const t = new PromotionTransitions();
		expect(t.update([promotion('p', 'Running', created)], CREATOR)).toEqual([]);
		expect(t.update([promotion('p', 'Running', created)], CREATOR)).toEqual([]);
		const [toast] = t.update([promotion('p', 'Succeeded', created)], CREATOR);
		expect(toast).toEqual({
			kind: 'success',
			message: 'Promotion succeeded: test steam: test → 0123456',
			shard: 'test'
		});
		// Only once.
		expect(t.update([promotion('p', 'Succeeded', created)], CREATOR)).toEqual([]);
	});

	it('toasts failures with the message', () => {
		const t = new PromotionTransitions();
		t.update([promotion('p', null, created)], CREATOR);
		const [toast] = t.update([promotion('p', 'Failed', created, 'workflow failed')], CREATOR);
		expect(toast.kind).toBe('error');
		expect(toast.message).toContain('workflow failed');
	});

	it('toasts a name first seen after the first poll, even if already rejected', () => {
		const t = new PromotionTransitions();
		t.update([], CREATOR);
		// The creation time doesn't matter, even one far in the past by the local clock.
		const old = new Date(NOW - 24 * HOUR).toISOString();
		const [toast] = t.update([promotion('quick', 'Rejected', old, 'ChannelBusy')], CREATOR);
		expect(toast.message).toBe('Promotion rejected: test steam: test → 0123456: ChannelBusy');
	});

	it('does not toast a name that comes back after missing a poll', () => {
		const t = new PromotionTransitions();
		t.update([promotion('p', 'Succeeded', created)], CREATOR);
		t.update([], CREATOR);
		expect(t.update([promotion('p', 'Succeeded', created)], CREATOR)).toEqual([]);
	});

	it('toasts cancellations to the canceller, even when someone else created it', () => {
		const t = new PromotionTransitions();
		t.update([], 'someone');
		t.update([cancelled(promotion('p', 'Running', created))], 'someone');
		const [toast] = t.update([cancelled(promotion('p', 'Cancelled', created))], 'someone');
		expect(toast.message).toBe('Cancelled by someone: test steam: test → 0123456');
	});

	it('toasts only the creator and the canceller', () => {
		const run = (localName: string | null, finished: Promotion) => {
			const t = new PromotionTransitions();
			t.update([], localName);
			t.update([promotion('p', 'Running', created)], localName);
			return t.update([finished], localName);
		};
		const succeeded = promotion('p', 'Succeeded', created);
		expect(run(CREATOR, succeeded)).toHaveLength(1);
		expect(run('someone else', succeeded)).toEqual([]);
		// No local name: no toasts, even for promotions with no creator recorded.
		expect(run(null, succeeded)).toEqual([]);
		expect(run('', { ...succeeded, metadata: { name: 'p' } })).toEqual([]);
		// Exact match only.
		expect(run('Creator', succeeded)).toEqual([]);

		// Finished despite a cancel: the canceller hears the real result too.
		const late = cancelled(promotion('p', 'Succeeded', created), 'someone');
		expect(run('someone', late)).toHaveLength(1);
		expect(run(CREATOR, late)).toHaveLength(1);
		expect(run('someone else', late)).toEqual([]);
	});
});

describe('activeBuildsFromShards', () => {
	it('makes one row per channel', () => {
		const shards: ShardSummary[] = [
			{
				name: 'test',
				displayName: 'Test',
				config: null,
				status: {
					channels: [
						steam({ version: SHA, since: '2026-01-01T00:00:00Z' }),
						{
							key: 'launcher/latest',
							kind: 'launcher',
							metadataPath: 'latest',
							version: '',
							error: 'AccessDenied'
						}
					],
					promotions: [],
					conditions: []
				}
			}
		];
		expect(activeBuildsFromShards(shards)).toEqual([
			{
				displayName: 'Test (steam: test)',
				steamBranch: 'test',
				sha: SHA,
				deployedAt: undefined,
				status: 'resolved',
				error: undefined
			},
			{
				displayName: 'Test (launcher: latest)',
				steamBranch: undefined,
				sha: undefined,
				deployedAt: undefined,
				status: 'tbd',
				error: 'AccessDenied'
			}
		]);
	});
});

describe('showPromotionTray', () => {
	it('shows only with the switch and Release controls both on', () => {
		expect(showPromotionTray(true, true)).toBe(true);
		expect(showPromotionTray(true, false)).toBe(false);
		expect(showPromotionTray(false, true)).toBe(false);
	});
});

describe('writeControl', () => {
	it('disables write controls with a pointer to Preferences until enabled', () => {
		expect(writeControl(false)).toEqual({ disabled: true, tooltip: RELEASE_CONTROLS_TOOLTIP });
		expect(writeControl(undefined).disabled).toBe(true);
		expect(writeControl(true)).toEqual({ disabled: false, tooltip: '' });
	});
});

describe('retryingMessage', () => {
	it('explains a submitted promotion the coordinator is retrying', () => {
		const retrying = promotion('r', null);
		retrying.status = {
			reason: 'Retrying',
			message: 'build manifest unreadable',
			steps: [],
			conditions: []
		};
		expect(retryingMessage(retrying)).toBe('build manifest unreadable');
		expect(promotionPhaseLabel(retrying)).toBe('Submitted');
	});

	it('is null otherwise', () => {
		expect(retryingMessage(promotion('a', null))).toBeNull();
		expect(retryingMessage(promotion('b', 'Rejected', undefined, 'ChannelBusy'))).toBeNull();
	});
});

describe('promotionPreview', () => {
	const NEW_SHA = 'fedcba9876543210fedcba9876543210fedcba98';
	const spec: PromotionSpec = {
		shard: 'test',
		channel: { kind: 'steam', branch: 'test' },
		version: NEW_SHA,
		gameConfig: 'Shipping',
		deployBackend: true
	};
	const detail = (promotions: Promotion[] = []): ShardDetail => ({
		name: 'test',
		displayName: 'Test',
		config: {
			shardName: 'test',
			healthcheckUrl: 'https://backend.example.com/v1/healthcheck',
			channels: [{ kind: 'steam', branch: 'test', gameConfig: 'Test' }],
			promotion: { allowDeployBackend: true }
		},
		status: {
			channels: [
				steam({ version: SHA, since: new Date(NOW - 2 * HOUR).toISOString(), gameConfig: 'Test' })
			],
			promotions: [],
			conditions: []
		},
		promotions
	});

	it('shows current and requested side by side', () => {
		const { rows, blocked } = promotionPreview(detail(), spec, NOW);
		expect(blocked).toBe('');
		expect(rows).toEqual([
			{
				label: 'Version',
				current: '0123456 · build first seen on the branch 2 h ago',
				requested: 'fedcba9'
			},
			{ label: 'Game config', current: 'Test', requested: 'Shipping' },
			{ label: 'Deploy backend', current: '', requested: 'Yes' },
			{ label: 'Sign', current: '', requested: 'Yes (default)' }
		]);
	});

	it('leaves Sign out for launcher channels', () => {
		const launcher: PromotionSpec = {
			...spec,
			channel: { kind: 'launcher', metadataPath: 'stable-1.0' }
		};
		const d = detail();
		d.config?.channels.push({ kind: 'launcher', metadataPath: 'stable-1.0', gameConfig: 'Test' });
		const { rows, blocked } = promotionPreview(d, launcher, NOW);
		expect(blocked).toBe('');
		expect(rows.map((r) => r.label)).not.toContain('Sign');
		expect(rows[0].current).toBe('nothing known');
		expect(rows[1].current).toBe('not reported');
	});

	it('blocks a busy channel', () => {
		const { blocked } = promotionPreview(detail([promotion('other', 'Running')]), spec, NOW);
		expect(blocked).toBe('steam: test is busy: other is running.');
		expect(promotionPreview(detail([promotion('done', 'Succeeded')]), spec, NOW).blocked).toBe('');
	});

	it('blocks a shard or channel that is gone', () => {
		expect(promotionPreview(null, spec, NOW).blocked).toBe('Shard test no longer exists.');
		const d = detail();
		if (d.config) d.config.channels = [];
		expect(promotionPreview(d, spec, NOW).blocked).toBe(
			'steam: test is no longer configured on test.'
		);
	});
});

describe('channelSinceText', () => {
	it('names since by kind, or as recorded from a promotion', () => {
		const since = new Date(NOW - 2 * HOUR).toISOString();
		expect(channelSinceText(steam({ since, source: 'observed' }), NOW)).toBe(
			'build first seen on the branch 2 h ago'
		);
		const launcher: ChannelStatus = {
			key: 'launcher/stable-1.0',
			kind: 'launcher',
			metadataPath: 'stable-1.0',
			version: SHA,
			since,
			source: 'observed',
			error: ''
		};
		expect(channelSinceText(launcher, NOW)).toBe('pointer changed 2 h ago');
		expect(channelSinceText(steam({ since, source: 'promotion' }), NOW)).toBe(
			'recorded from a promotion 2 h ago'
		);
		expect(channelSinceText(steam({}), NOW)).toBe('');
	});
});

describe('backendWarning', () => {
	const backend = {
		healthy: false,
		parts: {},
		observedAt: new Date(NOW - 60_000).toISOString(),
		error: 'HTTP 502'
	};
	const deploying = (name: string, phase: PromotionPhase | null, deployBackend = true) => {
		const p = promotion(name, phase);
		p.spec.deployBackend = deployBackend;
		return p;
	};

	it('notes a running promotion that deploys the backend', () => {
		expect(backendWarning(backend, [deploying('p', 'Running')], 'test', NOW)).toBe(
			'HTTP 502; a promotion that deploys the backend is in progress (p)'
		);
	});

	it('leaves out promotions that are not deploying the backend now', () => {
		for (const p of [
			deploying('a', 'Running', false),
			deploying('b', null),
			deploying('c', 'Succeeded')
		]) {
			expect(backendWarning(backend, [p], 'test', NOW), p.metadata.name).toBe('HTTP 502');
		}
		expect(backendWarning(backend, [deploying('d', 'Running')], 'other', NOW)).toBe('HTTP 502');
	});

	it('is quiet when the backend is fine', () => {
		const fine = { ...backend, healthy: true, error: '' };
		expect(backendWarning(fine, [deploying('p', 'Running')], 'test', NOW)).toBeNull();
	});
});
