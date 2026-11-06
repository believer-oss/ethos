import { describe, expect, it } from 'vitest';
import {
	backendSummary,
	channelsNow,
	fleetSettingsLine,
	fleetsAttention,
	partDeployedAt,
	previousVersion,
	settingsStartOpen,
	shardProblems
} from '$lib/shardPage';
import { backendParts } from '$lib/fleetFormat';
import type {
	FleetRelease,
	FleetReleasePhase,
	Promotion,
	ShardDetail,
	ShardSummary
} from '$lib/types';

const A = 'aaaaaaaa00000000000000000000000000000000';
const B = 'bbbbbbbb00000000000000000000000000000000';
const MINUTE = 60_000;
const NOW = Date.UTC(2026, 0, 10, 12, 0, 0);
const ago = (ms: number) => new Date(NOW - ms).toISOString();
const CONTENT = '2026-01-09-12-00-00_1.0_main_55555555aa';

const promotion = (version: string, from: string): Promotion => ({
	metadata: {
		name: 'test-launcher-abcde',
		annotations: { 'fleet.believer.dev/changed-by': 'alice' }
	},
	spec: {
		shard: 'test',
		channel: { kind: 'launcher', metadataPath: 'test/sandbox' },
		version,
		deployBackend: true
	},
	status: {
		phase: 'Succeeded',
		reason: '',
		message: '',
		previousVersion: from,
		finishedAt: ago(MINUTE),
		plan: {
			backend: {
				deploy: true,
				parts: [{ name: 'content', from: 'older', to: CONTENT }]
			}
		},
		changes: [{ kind: 'BackendDeployed', detail: '', at: ago(30 * MINUTE) }],
		steps: [],
		conditions: []
	}
});

const release = (version: string, phase: FleetReleasePhase): FleetRelease => ({
	metadata: { name: `test-${version.slice(0, 8)}` },
	spec: { shard: 'test', version, phase: 'Live', pinned: false, force: false },
	status: {
		phase,
		servedBy: [],
		maps: [{ mapId: 'Arena', check: 'OK', levelUrl: '' }],
		buildMapIds: ['Arena', 'Docks', 'Plaza'],
		clusters: [
			{
				name: 'cluster-a',
				observedAt: ago(10 * MINUTE),
				error: '',
				maps: [
					{ mapId: 'Arena', fleet: 'f', policy: 'Buffer 2', desired: 2, ready: 2, allocated: 0 }
				]
			}
		],
		conditions: []
	}
});

const detail = (releases: FleetRelease[], promotions: Promotion[] = []): ShardDetail => ({
	name: 'test',
	displayName: 'Test',
	generation: 1,
	config: {
		shardName: 'test',
		healthcheckUrl: '',
		channels: [{ kind: 'launcher', metadataPath: 'test/sandbox', gameConfig: 'Test' }],
		promotion: { allowDeployBackend: true },
		fleets: {
			clusters: [{ name: 'cluster-a', zone: 'zone-a' }],
			imageRepository: 'registry.example.com/game',
			limits: { maxReplicasPerFleet: 4 },
			gate: { minReady: 1, stableFor: '120s', timeout: '1200s' }
		}
	},
	spec: { fleetsEnabled: true, maps: ['Arena'], warm: { bufferSize: 2 } },
	status: {
		channels: [
			{
				key: 'launcher/test/sandbox',
				kind: 'launcher',
				metadataPath: 'test/sandbox',
				version: B,
				error: '',
				promotion: 'test-launcher-abcde',
				observedAt: ago(MINUTE)
			}
		],
		promotions: [],
		conditions: []
	},
	promotions,
	releases
});

describe('shard page', () => {
	it('summarises the backend, with deploy times only where recorded', () => {
		const parts = backendParts({ content: CONTENT, platform: 'unparsed' });
		expect(backendSummary(parts)).toBe('platform unparsed · content 55555555');
		const content = parts[1];
		expect(partDeployedAt([promotion(B, A)], content)).toBe(ago(30 * MINUTE));
		expect(partDeployedAt([], content)).toBeNull();
	});

	it('gives each channel its build, who promoted it, its Fleets and what to roll back to', () => {
		const live = release(B, 'Live');
		const cluster = live.status?.clusters[0];
		if (cluster) cluster.observedAt = ago(MINUTE);
		const d = detail([live], [promotion(B, A)]);
		const [now] = channelsNow(d, NOW);
		expect(now.promotedBy).toBe('promoted by alice');
		expect(now.fleets).toEqual({
			tone: 'ok',
			text: 'Healthy: 0 matches running, 2 servers Ready.'
		});
		expect(now.previous).toBe(A);
		expect(now.running).toBeNull();
		expect(previousVersion([promotion(B, A)], 'test', now.ref, A)).toBeNull();

		const gone = release(B, 'Retired');
		if (gone.status) gone.status.phaseSince = ago(90 * MINUTE);
		const retired = detail([gone]);
		const [stale] = channelsNow(retired, NOW);
		expect(stale.fleets).toEqual({
			tone: 'warn',
			text: 'No Fleets: release retired 1 h ago · matches start on demand'
		});
		expect(stale.promotedBy).toBe('via promotion test-launcher-abcde');
		expect(channelsNow(detail([]), NOW)[0].fleets?.text).toBe(
			'No Fleets: no release for this build · matches start on demand'
		);
	});

	it('lists shard-level problems once', () => {
		const d = detail([release(A, 'Warm')]);
		expect(shardProblems(d, NOW)).toEqual([
			{
				tone: 'bad',
				text: "Can't reach cluster-a: counts are from 10 m ago. Its Fleets keep running on their own."
			},
			{
				tone: 'warn',
				text: 'launcher/test/sandbox serves bbbbbbbb, which has no Live release: its matches start on on-demand servers.'
			}
		]);
		const gate = release(B, 'Live');
		if (!gate.status) throw new Error('fixture');
		gate.status.clusters = [];
		gate.status.conditions = [
			{
				type: 'ConfigValid',
				status: 'False',
				reason: 'GateCannotPass',
				message: 'timeout 1m is shorter than 2m'
			}
		];
		expect(shardProblems(detail([gate]), NOW)).toEqual([
			{
				tone: 'bad',
				text: "The shard's gate can never pass: timeout 1m is shorter than 2m. Fix the gate in the shard's ShardConfig."
			}
		]);
	});

	it('names a build without Fleets once, however many channels serve it', () => {
		const d = detail([release(B, 'Retired')]);
		const sandbox = d.status?.channels.at(0);
		if (!d.status || !sandbox) throw new Error('fixture');
		d.status.channels = [
			sandbox,
			{ ...sandbox, key: 'launcher/test/qa', metadataPath: 'test/qa' },
			{ ...sandbox, key: 'steam/qa', kind: 'steam', metadataPath: undefined, branch: 'qa' }
		];
		expect(shardProblems(d, NOW)).toEqual([
			{
				tone: 'warn',
				text: 'launcher/test/sandbox, launcher/test/qa and steam/qa serve bbbbbbbb, whose release retired: its matches start on on-demand servers, or fail where the backend has no fallback. Warm it again from its release card, or promote another build.'
			}
		]);
	});

	it('collapses Fleet settings to a line, open only when Fleets are off and not set up', () => {
		const d = detail([release(B, 'Live')]);
		expect(fleetSettingsLine(d)).toBe(
			'1 of 3 maps · warm buffer 2 · live default sizing · 1 cluster · gate 1 Ready for 2 m, timeout 20 m'
		);
		expect(settingsStartOpen(d)).toBe(false);
		expect(settingsStartOpen({ ...d, spec: { fleetsEnabled: false } })).toBe(true);
	});

	it('marks the Shards list when a channel’s build has no Fleets', () => {
		const shard = (phase: FleetReleasePhase): ShardSummary => ({
			...detail([]),
			status: {
				channels: [{ key: 'launcher/test/sandbox', kind: 'launcher', version: B, error: '' }],
				promotions: [],
				releases: [
					{
						name: 'test-bbbbbbbb',
						version: B,
						phase,
						servedBy: ['launcher/test/sandbox'],
						ready: 0,
						allocated: 0
					}
				],
				conditions: []
			}
		});
		expect(fleetsAttention(shard('Live'))).toBeNull();
		expect(fleetsAttention(shard('Retired'))).toBe(
			'launcher/test/sandbox has no Fleets for its build; a retired release is still served.'
		);
		expect(fleetsAttention(shard('Failed'))).toBe(
			'launcher/test/sandbox has no Fleets for its build; failed: test-bbbbbbbb.'
		);
	});
});
