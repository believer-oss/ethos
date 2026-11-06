import { describe, expect, it } from 'vitest';
import {
	backendDiff,
	buildLine,
	deployBackendDefault,
	olderParts,
	releasePlan,
	rollbackOf,
	rollbackText,
	stepsLine
} from '$lib/promotePlan';
import type {
	ChannelRef,
	CommitWorkflowInfo,
	FleetRelease,
	FleetReleasePhase,
	Promotion,
	PromotionPlan
} from '$lib/types';

const A = 'aaaaaaaa00000000000000000000000000000000';
const B = 'bbbbbbbb00000000000000000000000000000000';
const C = 'cccccccc00000000000000000000000000000000';
const HOUR = 3_600_000;
const NOW = Date.UTC(2026, 0, 10, 12, 0, 0);
const ago = (ms: number) => new Date(NOW - ms).toISOString();
const CHANNEL: ChannelRef = { kind: 'launcher', metadataPath: 'test/sandbox' };

const succeeded = (from: string, to: string, finishedAgo: number): Promotion => ({
	metadata: { name: `p-${to.slice(0, 4)}` },
	spec: { shard: 'test', channel: CHANNEL, version: to, deployBackend: false },
	status: {
		phase: 'Succeeded',
		reason: '',
		message: '',
		previousVersion: from,
		steps: [],
		finishedAt: ago(finishedAgo),
		conditions: []
	}
});

const release = (
	phase: FleetReleasePhase,
	conditions: FleetRelease['status'] = null
): FleetRelease => ({
	metadata: { name: 'test-aaaaaaaa' },
	spec: { shard: 'test', version: A, phase: 'Warm', pinned: false, force: false },
	status: conditions ?? {
		phase,
		phaseSince: ago(2 * HOUR),
		servedBy: [],
		maps: [],
		clusters: [],
		conditions: []
	}
});

describe('promote plan', () => {
	it('recognises a rollback from the channel’s retained promotions', () => {
		// A → B two days ago, B → C an hour ago: C is live.
		const history = [succeeded(A, B, 48 * HOUR), succeeded(B, C, HOUR)];
		const back = rollbackOf(history, 'test', CHANNEL, B, C);
		expect(back).toEqual({ version: B, replacedBy: C, until: ago(HOUR) });
		expect(back && rollbackText(back, NOW)).toBe(
			'↩ Rollback. bbbbbbbb is the build this channel served before cccccccc (until 1 h ago).'
		);
		expect(rollbackOf(history, 'test', CHANNEL, A, C)?.replacedBy).toBe(B);
		// The current build, an unknown build and another channel are not rollbacks.
		expect(rollbackOf(history, 'test', CHANNEL, C, C)).toBeNull();
		expect(rollbackOf(history, 'test', CHANNEL, '1'.repeat(40), C)).toBeNull();
		expect(rollbackOf(history, 'test', { kind: 'steam', branch: 'sandbox' }, B, C)).toBeNull();
	});

	it('says what happens to the build’s release', () => {
		expect(releasePlan(null, null, NOW)).toEqual({
			text: 'A new release warms (about 3–4 min)',
			ready: false,
			blocks: ''
		});
		expect(releasePlan(release('Warm'), null, NOW).text).toBe('Fleets ready now');
		expect(releasePlan(release('Draining'), null, NOW).text).toBe(
			'Release Draining: reused at once, no warm-up'
		);
		expect(releasePlan(release('Retired'), null, NOW).text).toBe(
			'Release Retired 2 h ago: recreated, about 3–4 min warm-up'
		);
		const retiring = release('Live');
		retiring.spec.phase = 'Retired';
		expect(releasePlan(retiring, null, NOW).text).toBe(
			'Release Retiring: waits for it to finish retiring, then warms afresh'
		);
		const failed = releasePlan(release('Failed'), null, NOW);
		expect(failed.text).toBe('Release Failed: Failed.');
		expect(failed.blocks).toMatch(/skip the warm gate/);
	});

	it('lists the steps', () => {
		const ready = { text: '', ready: true, blocks: '' };
		const warmUp = { ...ready, ready: false };
		expect(stepsLine(true, { plan: ready, skipGate: false })).toBe(
			'Steps: backend deploy · Fleets ready now · publish client (~3 m) · switch the channel (~10 s).'
		);
		expect(stepsLine(false, { plan: warmUp, skipGate: false })).toBe(
			'Steps: backend skipped · Fleets warm ~3–4 min · publish client (~3 m) · switch the channel (~10 s).'
		);
		expect(stepsLine(false, { plan: warmUp, skipGate: true })).toMatch(/Fleets warm alongside/);
		expect(stepsLine(false, null)).toBe(
			'Steps: backend skipped · publish client (~3 m) · switch the channel (~10 s).'
		);
	});

	it('compares the build’s backend parts with the shard’s, by build time', () => {
		const plan: PromotionPlan = {
			manifest: {
				backend: {
					platform: '2026-01-09-12-00-00_1.0_main_11111111aa',
					content: '2026-01-01-12-00-00_1.0_main_22222222aa',
					config: '2026-01-05-12-00-00_1.0_main_33333333aa'
				},
				mapIds: []
			},
			shardBackend: {
				platform: '2026-01-08-12-00-00_1.0_main_44444444aa',
				content: '2026-01-05-12-00-00_1.0_main_55555555aa',
				config: '2026-01-05-12-00-00_1.0_main_33333333aa'
			}
		};
		const rows = backendDiff(plan, [], NOW);
		expect(rows.map((r) => [r.name, r.direction])).toEqual([
			['platform', 'newer'],
			['content', 'older'],
			['config', 'same']
		]);
		expect(rows[1]).toEqual({
			name: 'content',
			direction: 'older',
			shard: 'shard now 55555555, built 5 d ago',
			build: 'this build 22222222, built 9 d ago'
		});
		expect(olderParts(rows)).toEqual(['content']);
		// No manifest: nothing to compare.
		expect(backendDiff({ manifest: null, shardBackend: null }, [], NOW)).toEqual([]);
	});

	it('names the build by branch and when CI picked it up', () => {
		const entry = {
			commit: A,
			branch: 'main',
			creationTimestamp: ago(2 * HOUR),
			// As the backend sends them: clock times without a date.
			workflows: [{ status: { finishedAt: '07:34:59 AM' } }]
		} as unknown as CommitWorkflowInfo;
		expect(buildLine(entry, NOW)).toBe('main · pushed 2 h ago');
		expect(buildLine({ ...entry, creationTimestamp: '' }, NOW)).toBe('main');
		expect(buildLine(undefined, NOW)).toBe('');
	});
});

describe('deployBackendDefault', () => {
	const manifest = { backend: {}, mapIds: [] };
	it('starts Deploy backend on only with a readable manifest and nothing going backwards', () => {
		expect(deployBackendDefault({ manifest, shardBackend: null }, false, [])).toBe(true);
		expect(deployBackendDefault({ manifest, shardBackend: null }, true, [])).toBe(false);
		expect(deployBackendDefault({ manifest, shardBackend: null }, false, ['content'])).toBe(false);
		// No manifest (loading, unreadable or refused): nothing to compare, so off.
		expect(deployBackendDefault({ manifest: null, shardBackend: null }, false, [])).toBe(false);
		expect(deployBackendDefault(null, false, [])).toBe(false);
	});
});
