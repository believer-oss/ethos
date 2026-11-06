import { describe, expect, it } from 'vitest';
import {
	activeBuildsFromShards,
	backendParts,
	cancelEffect,
	currentStepText,
	promotionProgressText,
	gatePlan,
	localTime,
	mapPreflight,
	promotionChanges,
	promotionTimeline,
	promotionWorkflow,
	backendWarning,
	channelSinceText,
	channelKey,
	formatAge,
	formatAgo,
	formatSpan,
	fleetsConfigLine,
	gateStableForMs,
	parseDuration,
	warmTtlMs,
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
	steamBuildText,
	writeControl
} from '$lib/fleetFormat';
import type {
	ChannelStatus,
	FleetRelease,
	FleetsConfig,
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

	it('says "just now" rather than "now ago"', () => {
		expect(formatAgo(new Date(NOW - 30_000), NOW)).toBe('just now');
		expect(formatAgo(new Date(NOW - 12 * 60_000), NOW)).toBe('12 m ago');
		expect(formatAgo(null, NOW)).toBe('');
		const part = parseBackendVersion('2026-01-02-03-04-05_1.2.3_main_abc12345');
		expect(
			formatBackendPart({ name: 'content', raw: '', version: part }, Date.UTC(2026, 0, 2, 3, 4, 5))
		).toBe('content abc12345 · built just now');
	});

	it('formats durations from the ShardConfig, as the local backend fills it', () => {
		expect(formatSpan(parseDuration('1200s') ?? 0)).toBe('20 m');
		expect(formatSpan(warmTtlMs({ warmTtl: '86400s' } as FleetsConfig))).toBe('24 h');
		// No fleets section, no durations: the local backend fills them from the coordinator's defaults.
		expect(gateStableForMs(null)).toBe(0);
		expect(
			fleetsConfigLine({
				clusters: [
					{ name: 'cluster-a', zone: 'cluster-a' },
					{ name: 'cluster-b', zone: 'cluster-b' }
				],
				limits: { maxReplicasPerFleet: 50 },
				gate: { stableFor: '120s', timeout: '1200s' }
			} as FleetsConfig)
		).toBe(
			'Clusters: cluster-a, cluster-b · at most 50 servers per Fleet · gate 1 Ready for 2 m, timeout 20 m'
		);
		expect(fleetsConfigLine(null)).toBe('Clusters: none');
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
		expect(steamBuildText(steam({ buildId: 1234, error: 'BuildNotFound' }))).toBe('build 1234');
		expect(steamBuildText(steam({}))).toBe('no build');
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
			message: 'Promotion succeeded: test steam: test → 01234567',
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
		expect(toast.message).toBe('Promotion rejected: test steam: test → 01234567: ChannelBusy');
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
		expect(toast.message).toBe('Cancelled by someone: test steam: test → 01234567');
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
				spec: null,
				generation: null,
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
		spec: null,
		generation: null,
		promotions,
		releases: []
	});

	it('shows current and requested side by side', () => {
		const { rows, blocked } = promotionPreview(detail(), spec, NOW);
		expect(blocked).toBe('');
		expect(rows).toEqual([
			{
				label: 'Version',
				current: '01234567 · build first seen on the branch 2 h ago',
				requested: 'fedcba98'
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
		// Not reported by a launcher: its default for promotions, said so on the label.
		expect(rows[1].current).toBe('Test');
		expect(rows[1].labelTitle).toMatch(/don't report/);
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

// The status of a fixture made with one, so tests can edit it in place.
const statusOf = (p: Promotion) => {
	if (!p.status) throw new Error('fixture has no status');
	return p.status;
};

describe('promotion steps', () => {
	const stepped = (currentStep: string, phase: PromotionPhase = 'Running'): Promotion => {
		const p = promotion('test-steam-abcde', phase);
		statusOf(p).currentStep = currentStep;
		statusOf(p).steps = [
			{ name: 'deploy-backend', workflow: '', phase: 'Skipped', message: 'no part changes' },
			{
				name: 'warm-fleets',
				workflow: '',
				fleetRelease: 'test-01234567',
				phase: 'Succeeded',
				message: ''
			},
			{
				name: 'publish-client',
				workflow: 'promote-test-steam-abcde-publish-client',
				phase: 'Running',
				message: ''
			},
			{
				name: 'flip-channel',
				workflow: 'promote-test-steam-abcde-flip-channel',
				phase: 'Pending',
				message: ''
			}
		];
		return p;
	};

	it('lists every step with its state', () => {
		const timeline = promotionTimeline(stepped('publish-client'));
		expect(timeline.map((s) => [s.label, s.state])).toEqual([
			['Deploy backend', 'skipped'],
			['Warm fleets', 'succeeded'],
			['Publish client', 'running'],
			['Flip channel', 'pending']
		]);
		expect(timeline[0].message).toBe('no part changes');
		expect(timeline[1].fleetRelease).toBe('test-01234567');
	});

	it('says where a promotion is, cancelling first', () => {
		expect(promotionProgressText(stepped('warm-fleets'))).toBe('Warm fleets');
		expect(promotionProgressText(cancelled(stepped('warm-fleets')))).toBe('Cancelling…');
		expect(promotionProgressText(promotion('p', 'Succeeded'))).toBe('Succeeded');
	});

	it('names the current step', () => {
		expect(currentStepText(stepped('warm-fleets'))).toBe('warm fleets');
		expect(currentStepText(promotion('p', 'Pending'))).toBe('pending');
	});

	it('links the running step’s workflow, else the last one that started', () => {
		expect(promotionWorkflow(stepped('publish-client'))).toBe(
			'promote-test-steam-abcde-publish-client'
		);
		const done = stepped('', 'Succeeded');
		statusOf(done).steps[3].phase = 'Succeeded';
		expect(promotionWorkflow(done)).toBe('promote-test-steam-abcde-flip-channel');
		// A promotion run as one workflow: a single promote step.
		const single = promotion('p', 'Running');
		statusOf(single).steps = [
			{ name: 'promote', workflow: 'promote-p', phase: 'Running', message: '' }
		];
		expect(promotionWorkflow(single)).toBe('promote-p');
	});

	it('says what cancelling does in each step', () => {
		expect(cancelEffect(promotion('p', null))).toMatch(/nothing is submitted/);
		expect(cancelEffect(stepped('deploy-backend'))).toMatch(/finishes first/);
		expect(cancelEffect(stepped('warm-fleets'))).toMatch(/Nothing is stopped/);
		expect(cancelEffect(stepped('publish-client'))).toMatch(/upload is stopped/);
		expect(cancelEffect(stepped('flip-channel'))).toMatch(/can’t be undone/);
	});

	it('lists what changed', () => {
		const p = stepped('', 'Cancelled');
		const expires = new Date(NOW + 26 * 60 * 60_000);
		statusOf(p).changes = [
			{
				kind: 'FleetReleaseWarming',
				detail: `test-01234567, retires on its TTL at ${expires.toISOString()} unless used`
			},
			{
				kind: 'FleetReleaseWarming',
				detail:
					'test-01234567, created afresh in place of the retired one, retires on its TTL unless used'
			},
			{ kind: 'BackendDeployed', detail: 'content a → b' },
			{ kind: 'ClientUploaded', detail: 'Steam build 1234, not live on any branch' },
			{ kind: 'FleetReleaseLive', detail: 'test-01234567' },
			{ kind: 'FleetReleaseWarming', detail: 'something else' }
		];
		expect(promotionChanges(p, NOW)).toEqual([
			`Release test-01234567 warmed (expires ${localTime(expires, NOW)} unless used)`,
			'Release test-01234567 warmed, in place of the retired one (expires on its TTL unless used)',
			'Client uploaded: Steam build 1234, not live on any branch',
			'Release test-01234567 went live',
			'Release warmed: something else'
		]);
	});

	it('names local times by their day', () => {
		const at = (day: number, hour: number) => new Date(2026, 0, day, hour, 5);
		const now = at(2, 12).getTime();
		expect(localTime(at(2, 19), now)).toBe('19:05');
		expect(localTime(at(3, 9), now)).toBe('tomorrow 09:05');
		expect(localTime(at(1, 9), now)).toBe('yesterday 09:05');
		expect(localTime(at(7, 9), now)).toBe('7 Jan 09:05');
		expect(localTime('not a time', now)).toBe('');
	});

	it('toasts a cancel with the coordinator’s summary', () => {
		const t = new PromotionTransitions();
		t.update([promotion('p', 'Running')], CREATOR);
		const [toast] = t.update(
			[promotion('p', 'Cancelled', undefined, 'release warming; client not published')],
			CREATOR
		);
		expect(toast.message).toBe(
			'Cancelled: test steam: test → 01234567: release warming; client not published'
		);
	});
});

describe('gate and map pre-flight', () => {
	const fleetDetail = (enabled: boolean, maps?: string[]): ShardDetail => ({
		name: 'test',
		displayName: 'Test',
		config: {
			shardName: 'test',
			healthcheckUrl: 'https://backend.example.com/v1/healthcheck',
			channels: [{ kind: 'steam', branch: 'test', gameConfig: 'Test' }],
			promotion: { allowDeployBackend: true },
			fleets: {
				clusters: [{ name: 'cluster-a', zone: 'a' }],
				imageRepository: 'r',
				limits: {},
				gate: { timeout: '15m' }
			}
		},
		spec: { fleetsEnabled: enabled, maps },
		generation: 1,
		status: {
			channels: [],
			promotions: [],
			conditions: []
		},
		promotions: [],
		releases: []
	});

	it('says whether the gate applies', () => {
		expect(gatePlan(fleetDetail(true), false)).toEqual({
			applies: true,
			text: "Waits for the build's Fleets to pass the warm gate (up to 15 m) before publishing the client."
		});
		expect(gatePlan(fleetDetail(true), true).applies).toBe(false);
		expect(gatePlan(fleetDetail(false), false).text).toMatch(/Fleets are off/);
		expect(gatePlan(null, false).text).toMatch(/no Fleets/);
	});

	it('checks each fleet map against any existing release of the build', () => {
		const IN_BUILD = 'in the build’s maps: its level is checked when warming';
		expect(mapPreflight(fleetDetail(true, ['Arena']), SHA)).toEqual([
			{ mapId: 'Arena', ok: true, text: 'checked against the build’s manifest when warming' }
		]);
		const withRelease = (status: NonNullable<FleetRelease['status']>) => {
			const detail = fleetDetail(true, ['Arena', 'Gone']);
			detail.releases = [
				{
					metadata: { name: 'test-01234567' },
					spec: { shard: 'test', version: SHA, phase: 'Warm', pinned: false, force: false },
					status
				}
			];
			return detail;
		};
		// The manifest is read, the check not yet written: buildMapIds already tells.
		expect(
			mapPreflight(
				withRelease({
					servedBy: [],
					buildMapIds: ['Arena'],
					maps: [],
					clusters: [],
					conditions: []
				}),
				SHA
			)
		).toEqual([
			{ mapId: 'Arena', ok: true, text: IN_BUILD },
			{ mapId: 'Gone', ok: false, text: 'not in the build’s maps: the release fails' }
		]);
		// The manifest's own map IDs, with no release yet; an empty list (an older manifest) says nothing.
		expect(
			mapPreflight(fleetDetail(true, ['Arena', 'Gone']), SHA, ['Arena']).map((c) => c.text)
		).toEqual([IN_BUILD, 'not in the build’s maps: the release fails']);
		expect(mapPreflight(fleetDetail(true, ['Gone']), SHA, [])[0].ok).toBe(true);
		const checked = mapPreflight(
			withRelease({
				servedBy: [],
				buildMapIds: ['Arena'],
				maps: [
					{ mapId: 'Arena', check: 'NotCooked', levelUrl: '' },
					{ mapId: 'Gone', check: 'NotInBuild', levelUrl: '' }
				],
				clusters: [],
				conditions: []
			}),
			SHA
		);
		expect(checked.map((c) => c.ok)).toEqual([false, false]);
		expect(checked[0].text).toBe('the build doesn’t contain its level: the release fails');
		expect(mapPreflight(fleetDetail(false), SHA)).toEqual([]);
	});

	it('adds the gate to the promote preview, and needs a reason to skip it', () => {
		const spec: PromotionSpec = {
			shard: 'test',
			channel: { kind: 'steam', branch: 'test' },
			version: SHA,
			deployBackend: false
		};
		const { rows } = promotionPreview(fleetDetail(true), spec, NOW);
		expect(rows.find((r) => r.label === 'Warm gate')?.requested).toBe('Waits for Fleets');
		const skipping = { ...spec, skipFleetGate: { reason: ' broken ' } };
		expect(promotionPreview(fleetDetail(true), skipping, NOW).rows.at(-1)?.requested).toBe(
			'Skipped: broken'
		);
		expect(
			promotionPreview(fleetDetail(true), { ...spec, skipFleetGate: { reason: ' ' } }, NOW).blocked
		).toBe('Skipping the warm gate needs a reason.');
	});
});
