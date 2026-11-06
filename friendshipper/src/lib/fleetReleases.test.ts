import { describe, expect, it } from 'vitest';
import {
	fleetsColumnText,
	isReleaseMine,
	releaseAllocated,
	releaseChips,
	releaseDetails,
	releaseMapTotals,
	releasePhaseLabel,
	releaseSections,
	releaseStatus,
	ReleaseTransitions,
	releaseWho,
	retireDialog,
	warmCheck,
	retireBlocked
} from '$lib/fleetReleases';
import type {
	FleetRelease,
	FleetReleasePhase,
	FleetsConfig,
	K8sCondition,
	Promotion,
	ShardSummary
} from '$lib/types';
import { formatSpan, parseDuration } from '$lib/fleetFormat';
import { clockTime } from '$lib/pollErrors';

const SHA = 'abc12345e89abcdef0123456789abcdef0123456';
const MINUTE = 60_000;
const NOW = Date.UTC(2026, 0, 2, 12, 0, 0);
const ago = (ms: number) => new Date(NOW - ms).toISOString();

const release = (
	phase: FleetReleasePhase | null,
	{
		by = 'someone',
		pinned = false,
		desired = 'Warm' as FleetRelease['spec']['phase'],
		allocated = [0, 0]
	} = {}
): FleetRelease => ({
	metadata: { name: 'test-abc12345', annotations: { 'fleet.believer.dev/changed-by': by } },
	spec: { shard: 'test', version: SHA, phase: desired, pinned, force: false },
	status:
		phase === null
			? null
			: {
					phase,
					servedBy: [],
					maps: [{ mapId: 'Arena', check: 'OK', levelUrl: '' }],
					clusters: [
						{
							name: 'cluster-a',
							gate: 'Passed',
							error: '',
							maps: [
								{
									mapId: 'Arena',
									fleet: 'f',
									policy: 'Buffer 2',
									desired: 3,
									ready: 2,
									allocated: allocated[0]
								}
							]
						},
						{
							name: 'cluster-b',
							gate: 'Pending',
							error: '',
							maps: [
								{
									mapId: 'Arena',
									fleet: 'f',
									policy: 'Buffer 2',
									desired: 2,
									ready: 1,
									allocated: allocated[1]
								}
							]
						}
					],
					conditions: []
			  }
});

const statusOf = (r: FleetRelease) => {
	if (!r.status) throw new Error('fixture has no status');
	return r.status;
};

describe('parseDuration', () => {
	it('reads Go durations', () => {
		expect(parseDuration('30m')).toBe(30 * MINUTE);
		expect(parseDuration('1h30m')).toBe(90 * MINUTE);
		expect(parseDuration('2.5s')).toBe(2500);
		expect(parseDuration('')).toBeNull();
		expect(parseDuration('5 m')).toBeNull();
		expect(parseDuration('-5m')).toBeNull();
	});

	it('formats spans', () => {
		expect(formatSpan(45_000)).toBe('45 s');
		expect(formatSpan(12 * MINUTE)).toBe('12 m');
		expect(formatSpan(65 * MINUTE)).toBe('1 h 5 m');
		expect(formatSpan(120 * MINUTE)).toBe('2 h');
	});
});

describe('release cards', () => {
	it('totals each map across clusters', () => {
		const [arena] = releaseMapTotals(release('Live', { allocated: [1, 2] }));
		expect(arena).toMatchObject({ mapId: 'Arena', desired: 5, ready: 3, allocated: 3 });
		expect(arena.clusters.map((c) => c.name)).toEqual(['cluster-a', 'cluster-b']);
		expect(releaseAllocated(release('Live', { allocated: [1, 2] }))).toBe(3);
	});

	it('labels phases, including a retire not yet picked up', () => {
		expect(releasePhaseLabel(release(null))).toBe('Submitted');
		expect(releasePhaseLabel(release('Draining', { desired: 'Retired' }))).toBe('Retiring…');
		expect(releasePhaseLabel(release('Retiring', { desired: 'Retired' }))).toBe('Retiring');
	});

	it('blocks Retire while servers are allocated', () => {
		expect(retireBlocked(release('Live'))).toBeNull();
		expect(retireBlocked(release('Live', { allocated: [1, 0] }))).toBe('1 server still in a match');
		expect(retireBlocked(release('Live', { allocated: [1, 2] }))).toBe(
			'3 servers still in a match'
		);
	});
});

const FLEETS: FleetsConfig = {
	clusters: [],
	imageRepository: '',
	limits: {},
	gate: { minReady: 2, stableFor: '3m', timeout: '20m' },
	retireAfterIdle: '30m'
};

const condition = (type: string, reason: string, message: string): K8sCondition => ({
	type,
	status: 'False',
	reason,
	message
});

const promotionWarming = (releaseName: string, startedAt: string): Promotion => ({
	metadata: { name: 'test-promo', annotations: { 'fleet.believer.dev/changed-by': 'alice' } },
	spec: {
		shard: 'test',
		channel: { kind: 'launcher', metadataPath: 'test/sandbox' },
		version: SHA,
		deployBackend: false
	},
	status: {
		phase: 'Running',
		reason: '',
		message: '',
		steps: [
			{
				name: 'warm-fleets',
				workflow: '',
				fleetRelease: releaseName,
				phase: 'Running',
				startedAt,
				message: ''
			}
		],
		conditions: []
	}
});

describe('releaseStatus', () => {
	const status = (r: FleetRelease, promotions: Promotion[] = []) =>
		releaseStatus(r, FLEETS, promotions, NOW);

	it('warns that a retired build a channel serves has no Fleets', () => {
		const retired = release('Retired', { desired: 'Retired' });
		statusOf(retired).servedBy = ['test/sandbox'];
		expect(status(retired)).toEqual({
			tone: 'warn',
			text: 'test/sandbox still serves this build, but it has no Fleets: its matches start on on-demand servers, or fail where the backend has no fallback.',
			hint: 'Warm it again to bring its Fleets back, or promote another build to test/sandbox.'
		});
		expect(status(release('Retired', { desired: 'Retired' })).tone).toBe('info');
	});

	it('says a retirement is stuck, per cluster, from when retiring began', () => {
		const stuck = release('Retiring', { desired: 'Retired', allocated: [1, 0] });
		statusOf(stuck).phaseSince = ago(135 * MINUTE);
		statusOf(stuck).clusters[0].retireStuck = { since: ago(15 * MINUTE), message: 'waiting' };
		const began = clockTime(new Date(NOW - 135 * MINUTE));
		expect(status(stuck)).toEqual({
			tone: 'warn',
			text: `Stuck: retiring since ${began} (2 h 15 m) on cluster-a, 1 server still in a match. Retire never ends matches on its own.`
		});

		// An unreachable cluster comes first, and the count is as last seen.
		statusOf(stuck).clusters[0].observedAt = ago(14 * MINUTE);
		expect(status(stuck).text.split('\n')).toEqual([
			"Can't reach cluster-a for 14 m. Counts are from then; its Fleets keep running on their own.",
			`Stuck: retiring since ${began} (2 h 15 m) on cluster-a, 1 server still in a match (as last seen). Retire never ends matches on its own.`
		]);
	});

	it('says what a retirement waits on', () => {
		const waiting = release('Retiring', { desired: 'Retired', allocated: [1, 2] });
		statusOf(waiting).phaseSince = ago(10 * MINUTE);
		expect(status(waiting)).toEqual({
			tone: 'info',
			text: 'Retiring: 3 servers still in a match, 10 m so far.'
		});
		expect(status(release('Retiring', { desired: 'Retired' })).text).toBe('Removing Fleets.');
		// Asked, but the coordinator hasn't started: no elapsed time from the old phase.
		const asked = release('Live', { desired: 'Retired', allocated: [1, 0] });
		expect(status(asked).text).toBe('Retiring: 1 server still in a match.');
	});

	it('flags an unreachable cluster', () => {
		const live = release('Live');
		statusOf(live).clusters[1].observedAt = ago(14 * MINUTE);
		statusOf(live).clusters[1].error = 'client error (SendRequest)';
		expect(status(live)).toEqual({
			tone: 'bad',
			text: "Can't reach cluster-b for 14 m. Counts are from then; its Fleets keep running on their own."
		});
		// A recent reading is fine.
		statusOf(live).clusters[1].observedAt = ago(MINUTE);
		statusOf(live).clusters[1].error = '';
		expect(status(live).tone).toBe('ok');
	});

	it('names the maps a failed build has no level for', () => {
		const failed = release('Failed');
		statusOf(failed).maps = [
			{ mapId: 'Arena', check: 'NotInBuild', levelUrl: '' },
			{ mapId: 'Docks', check: 'NotCooked', levelUrl: '' },
			{ mapId: 'Plaza', check: 'OK', levelUrl: '' }
		];
		statusOf(failed).conditions = [condition('MapsChecked', 'MapsUnavailable', 'Arena, Docks')];
		expect(status(failed)).toEqual({
			tone: 'bad',
			text: "This build has no level for fleet maps Arena (not in the build) / Docks (not cooked), so its Fleets can't start.",
			hint: 'Untick them in Fleet settings, or promote a build that has them.',
			settings: true
		});
	});

	it('explains a gate that can never pass and a missing image', () => {
		const failed = release('Failed');
		statusOf(failed).conditions = [
			condition(
				'ConfigValid',
				'GateCannotPass',
				'fleets.gate.timeout 1m is shorter than fleets.gate.stableFor 2m (the default), so the gate can never pass'
			)
		];
		expect(status(failed).text).toBe(
			"The shard's gate can never pass: fleets.gate.timeout 1m is shorter than fleets.gate.stableFor 2m (the default). Fix the gate in the shard's ShardConfig."
		);
		statusOf(failed).conditions = [
			condition(
				'ImageExists',
				'ImageMissing',
				'no image for this version, per cluster-a, cluster-b'
			)
		];
		expect(status(failed).text).toBe(
			"The game-server image for this build isn't in the registry (cluster-a, cluster-b)."
		);
		statusOf(failed).conditions = [condition('ConfigValid', 'NoFleets', '')];
		expect(status(failed).text).toBe("The shard's config has no Fleets section");
	});

	it('summarises other config and sync problems on a running release', () => {
		const live = release('Live');
		statusOf(live).conditions = [
			condition('ClustersSynced', 'SizingInvalid', 'cluster-a: Arena: maxReplicas 99 is above 50')
		];
		expect(status(live)).toEqual({
			tone: 'warn',
			text: "A map's sizing breaks the rules, so that cluster isn't updated: cluster-a: Arena: maxReplicas 99 is above 50"
		});
	});

	it('shows the warm gate, with the promotion’s countdown', () => {
		const warming = release('Warming');
		expect(status(warming).text).toBe('Warm gate: 3 Ready (needs 2, then 3 m with no restarts).');
		const promotion = promotionWarming('test-abc12345', ago(90_000));
		expect(status(warming, [promotion]).text).toBe(
			'Warm gate: 3 Ready (needs 2, then 3 m with no restarts) · the promotion gives up in 18 m.'
		);
		// Another release's step doesn't count.
		expect(status(warming, [promotionWarming('other', ago(90_000))]).text).not.toMatch(/gives up/);
	});

	it('says when a Warm release expires, unless pinned', () => {
		const warm = release('Warm');
		statusOf(warm).warmUntil = new Date(NOW + 65 * MINUTE).toISOString();
		expect(status(warm)).toEqual({
			tone: 'ok',
			text: 'Ready. Not served yet; expires in 1 h 5 m unless a promotion uses it.'
		});
		warm.spec.pinned = true;
		expect(status(warm).text).toBe('Ready. Pinned: never retires on its own.');
	});

	it('reports a Live release healthy, or a map with no Ready server', () => {
		expect(status(release('Live', { allocated: [1, 0] }))).toEqual({
			tone: 'ok',
			text: 'Healthy: 1 match running, 3 servers Ready.'
		});
		const empty = release('Live');
		statusOf(empty).clusters[1].maps[0].ready = 0;
		expect(status(empty)).toEqual({ tone: 'warn', text: 'No Ready server in cluster-b/Arena.' });
		// Parked: desired 0 is not a problem.
		statusOf(empty).clusters[1].maps[0].desired = 0;
		expect(status(empty).tone).toBe('ok');
	});

	it('counts down a Draining release, or waits for its matches', () => {
		const draining = release('Draining');
		statusOf(draining).idleSince = ago(10 * MINUTE);
		expect(status(draining).text).toBe('Idle 10 m · retires in 20 m.');
		draining.spec.pinned = true;
		expect(status(draining).text).toBe('Idle 10 m · pinned: never retires on its own.');
		const busy = release('Draining', { allocated: [2, 0] });
		expect(status(busy).text).toBe('2 matches still running; retires 30 m after the last ends.');
	});
});

describe('release card parts', () => {
	it('names each cluster and map in a chip, and why a map has none', () => {
		const r = release('Warming');
		statusOf(r).maps.push({ mapId: 'Docks', check: 'NotInBuild', levelUrl: '' });
		statusOf(r).clusters[0].observedAt = ago(5_000);
		statusOf(r).clusters[1].observedAt = ago(14 * MINUTE);
		statusOf(r).clusters[1].error = 'client error (SendRequest)';
		expect(releaseChips(r, NOW)).toEqual([
			{
				key: 'cluster-a/Arena',
				tone: 'ok',
				text: 'cluster-a · Arena · 0 in use / 2 ready / 3 · Buffer 2 · seen just now'
			},
			{
				key: 'cluster-b/Arena',
				tone: 'stale',
				text: 'cluster-b · Arena · 0 in use / 1 ready / 2 · Buffer 2 · last seen 14 m ago · client error (SendRequest)'
			},
			{ key: 'Docks', tone: 'refused', text: 'Docks · not in this build' }
		]);
		statusOf(r).clusters[1].error = '';
		statusOf(r).clusters[1].observedAt = ago(5_000);
		expect(releaseChips(r, NOW)[1].tone).toBe('pending');
		statusOf(r).clusters[1].gate = 'Failed';
		expect(releaseChips(r, NOW)[1].tone).toBe('failed');
		expect(releaseChips(release('Retiring', { desired: 'Retired' }), NOW)[0].tone).toBe('retiring');
		const unstarted = release('Failed');
		statusOf(unstarted).clusters = [];
		expect(releaseChips(unstarted, NOW)).toEqual([
			{ key: 'Arena', tone: 'off', text: 'Arena · OK, not started' }
		]);
	});

	it('groups releases into sections, by phase then newest', () => {
		const named = (name: string, r: FleetRelease, minutesAgo = 0): FleetRelease => ({
			...r,
			metadata: { ...r.metadata, name, creationTimestamp: ago(minutesAgo * MINUTE) }
		});
		const servedRetired = named('served-retired', release('Retired', { desired: 'Retired' }));
		statusOf(servedRetired).servedBy = ['test/sandbox'];
		const sections = releaseSections([
			named('warm-old', release('Warm'), 60),
			named('warm-new', release('Warm'), 5),
			named('failed', release('Failed')),
			named('idle', release('Draining')),
			named('busy', release('Draining', { allocated: [1, 0] })),
			named('live', release('Live')),
			servedRetired,
			named('retired', release('Retired', { desired: 'Retired' })),
			named('warming', release('Warming')),
			named('asked', release('Live', { desired: 'Retired' }))
		]);
		const names = (list: FleetRelease[]) => list.map((r) => r.metadata.name);
		expect(names(sections.serving)).toEqual(['live', 'served-retired', 'busy']);
		expect(names(sections.inProgress)).toEqual(['warming', 'asked']);
		expect(names(sections.notServing)).toEqual(['idle', 'warm-new', 'warm-old', 'failed']);
		expect(names(sections.retired)).toEqual(['retired']);
	});

	it('says who changed it, through the promotion when one did', () => {
		const promoted = release('Live', { by: 'test-promo' });
		expect(releaseWho(promoted, [promotionWarming('x', ago(0))])).toBe(
			'promoted by alice (test-promo)'
		);
		expect(releaseWho(release('Live', { by: 'bob' }), [])).toBe('by bob');
	});

	it('keeps the raw conditions and stuck reports for Details', () => {
		const r = release('Retiring', { desired: 'Retired' });
		statusOf(r).conditions = [
			condition('ClustersSynced', 'Unreachable', 'cluster-b: timed out'),
			{ type: 'GatePassed', status: 'True', reason: 'Ok', message: '' }
		];
		statusOf(r).clusters[0].retireStuck = { since: ago(0), message: 'waiting longer than 2h' };
		expect(releaseDetails(r)).toEqual([
			'ClustersSynced Unreachable: cluster-b: timed out',
			'cluster-a RetireStuck: waiting longer than 2h'
		]);
		// A gate that never passed before retiring says so, with or without a message.
		statusOf(r).conditions = [condition('GatePassed', 'Retired', '')];
		expect(releaseDetails(r)[0]).toBe('GatePassed Retired: Retired before its warm gate passed');
	});
});

describe('release card follow-ups', () => {
	it('explains a gate that can never pass on a served release, as a warning', () => {
		const live = release('Live');
		statusOf(live).conditions = [
			condition(
				'ConfigValid',
				'GateCannotPass',
				'fleets.gate.timeout 1m is shorter than 2m, so the gate can never pass'
			)
		];
		expect(releaseStatus(live, FLEETS, [], NOW)).toEqual({
			tone: 'warn',
			text: "The shard's gate can never pass: fleets.gate.timeout 1m is shorter than 2m. Fix the gate in the shard's ShardConfig."
		});
	});

	it('uses an unreachable condition’s own words when they name the cluster', () => {
		const live = release('Live');
		statusOf(live).conditions = [
			condition('ClustersSynced', 'Unreachable', "can't reach cluster-b: timed out")
		];
		expect(releaseStatus(live, FLEETS, [], NOW).text).toBe(
			"Can't reach cluster-b: timed out. Its Fleets keep running on their own."
		);
		statusOf(live).conditions = [condition('ClustersSynced', 'Unreachable', 'timed out')];
		expect(releaseStatus(live, FLEETS, [], NOW).text).toBe(
			"Can't reach a cluster: timed out. Its Fleets keep running on their own."
		);
	});

	it('puts an unreachable cluster first during a retirement', () => {
		const retiring = release('Retiring', { desired: 'Retired', allocated: [0, 1] });
		statusOf(retiring).clusters[1].error = 'client error (SendRequest)';
		expect(releaseStatus(retiring, FLEETS, [], NOW)).toEqual({
			tone: 'warn',
			text: "Can't reach cluster-b. Its Fleets keep running on their own.\nRetiring: 1 server still in a match."
		});
	});

	it('shows no chips for a retired release, and unstarted maps once for a failed one', () => {
		expect(releaseChips(release('Retired', { desired: 'Retired' }), NOW)).toEqual([]);
		const failed = release('Failed');
		statusOf(failed).maps.push({ mapId: 'Docks', check: 'OK', levelUrl: '' });
		expect(releaseChips(failed, NOW).map((c) => c.text)).toEqual([
			'cluster-a · Arena · 0 in use / 2 ready / 3 · Buffer 2',
			'cluster-b · Arena · 0 in use / 1 ready / 2 · Buffer 2',
			'Docks · OK, not started'
		]);
	});

	it('names a promotion that is no longer listed as one', () => {
		expect(releaseWho(release('Live', { by: 'test-launcher-x7k2p' }), [])).toBe(
			'by promotion test-launcher-x7k2p'
		);
	});
});

describe('retireDialog', () => {
	const labels = (r: FleetRelease, end = false) =>
		retireDialog(r, end).buttons.map((b) => `${b.label}${b.force ? ' (force)' : ''}`);

	it('retires an unused release plainly', () => {
		const dialog = retireDialog(release('Warm'));
		expect(dialog.title).toBe('Retire abc12345?');
		expect(dialog.body).toBe('Its Fleets are removed. You can warm it again later.');
		expect(dialog.warn).toBe('');
		expect(labels(release('Warm'))).toEqual(['Retire']);
	});

	it('warns about a served release, and says what retiring it does', () => {
		const served = release('Live');
		statusOf(served).servedBy = ['test/sandbox', 'test/main'];
		const dialog = retireDialog(served);
		expect(dialog.title).toBe("Retire abc12345? It's in use");
		expect(dialog.warn).toBe(
			'Served by test/sandbox and test/main. Players on those channels are running this build now.'
		);
		expect(dialog.list).toEqual([
			'removes its Fleets in cluster-a and cluster-b (3 Ready servers) once no match is running on them',
			'new matches for this build start on on-demand servers (slower to start), or fail where the backend has no on-demand fallback',
			'leaves the channels unchanged: they keep serving abc12345 until a promotion moves them'
		]);
		expect(dialog.footnote).toMatch(/Warm again from the Retired list/);
		expect(labels(served)).toEqual(['Retire while in use']);
	});

	it('offers to wait for running matches, or end them', () => {
		const busy = release('Live', { allocated: [1, 0] });
		const dialog = retireDialog(busy);
		expect(dialog.title).toBe('Retire abc12345? A match is running');
		expect(dialog.bad).toBe(
			"1 match running on this release's servers. Ending it deletes its server now: those players are dropped from the match."
		);
		expect(labels(busy)).toEqual(['Retire when the match ends', 'End 1 match and retire (force)']);

		statusOf(busy).servedBy = ['test/sandbox'];
		expect(retireDialog(busy).title).toBe("Retire abc12345? It's in use, and a match is running");
		expect(retireDialog(busy).warn).toMatch(/^Served by test\/sandbox\. Players on that channel/);

		// From a retirement that waits on them: only ending them is left.
		const waiting = release('Retiring', { desired: 'Retired', allocated: [1, 1] });
		expect(retireDialog(waiting, true).title).toBe('abc12345 is retiring: 2 matches are running');
		expect(labels(waiting, true)).toEqual(['End 2 matches and retire (force)']);
	});
});

describe('fleetsColumnText', () => {
	const shard = (fleets: boolean, enabled: boolean, phases: FleetReleasePhase[]): ShardSummary => ({
		name: 'test',
		displayName: 'Test',
		config: {
			shardName: 'test',
			healthcheckUrl: 'https://backend.example.com/v1/healthcheck',
			channels: [],
			promotion: { allowDeployBackend: false },
			fleets: fleets ? { clusters: [], imageRepository: '', limits: {}, gate: {} } : null
		},
		spec: { fleetsEnabled: enabled },
		generation: 1,
		status: {
			channels: [],
			promotions: [],
			conditions: [],
			releases: phases.map((phase, i) => ({
				name: `r${i}`,
				version: SHA,
				phase,
				servedBy: [],
				ready: 0,
				allocated: 0
			}))
		}
	});

	it('says off, on, and what is warm and live', () => {
		expect(fleetsColumnText(shard(false, false, []))).toBe('—');
		expect(fleetsColumnText(shard(true, false, ['Live']))).toBe('Off');
		expect(fleetsColumnText(shard(true, true, []))).toBe('On');
		expect(
			fleetsColumnText(shard(true, true, ['Warming', 'Warm', 'Live', 'Draining', 'Retired']))
		).toBe('On · 2 warm · 1 live · 1 draining');
	});
});

describe('ReleaseTransitions', () => {
	it('toasts only the person who made the change, after the baseline', () => {
		const t = new ReleaseTransitions();
		expect(t.update([release('Warming')], 'someone')).toEqual([]);
		expect(t.update([release('Warm')], 'someone')).toEqual([
			{
				kind: 'success',
				message: 'Release warm: test-abc12345 (abc12345) passed the gate on test',
				shard: 'test'
			}
		]);

		const other = new ReleaseTransitions();
		other.update([release('Warming')], 'else');
		expect(other.update([release('Warm')], 'else')).toEqual([]);
	});

	it('toasts a failure and a finished retirement', () => {
		const t = new ReleaseTransitions();
		t.update([release('Warming')], 'someone');
		expect(t.update([release('Failed')], 'someone')[0].kind).toBe('error');
		t.update([release('Retiring', { desired: 'Retired' })], 'someone');
		expect(t.update([release('Retired', { desired: 'Retired' })], 'someone')[0].message).toBe(
			'Release retired: test-abc12345 (abc12345) on test'
		);
	});

	it('leaves releases a promotion made to the promotion', () => {
		expect(isReleaseMine(release('Warm', { by: 'test-steam-abcde' }), 'someone')).toBe(false);
		expect(isReleaseMine(release('Warm'), null)).toBe(false);
	});
});

describe('warmCheck', () => {
	it('allows a build with no release', () => {
		expect(warmCheck(null)).toEqual({ blocked: null, note: '' });
	});

	it('recreates a Retired release', () => {
		expect(warmCheck(release('Retired', { desired: 'Retired' }))).toEqual({
			blocked: null,
			note: 'test-abc12345 is Retired: warming deletes it and creates a fresh release.'
		});
	});

	it('waits for a release still on its way to Retired', () => {
		expect(warmCheck(release('Retiring', { desired: 'Retired' })).blocked).toBe(
			"test-abc12345 is still retiring; Warm is available once it's Retired."
		);
		// Asked to retire, not picked up yet.
		expect(warmCheck(release('Live', { desired: 'Retired' })).blocked).toMatch(/still retiring/);
		const deleting = release('Retired', { desired: 'Retired' });
		deleting.metadata.deletionTimestamp = '2026-01-02T03:04:05Z';
		expect(warmCheck(deleting).blocked).toBe(
			"test-abc12345 is being deleted; Warm is available once it's gone."
		);
	});

	it('refuses a build whose release is in use', () => {
		expect(warmCheck(release('Warm')).blocked).toBe(
			'This build already has a release here: test-abc12345 (Warm).'
		);
	});
});
