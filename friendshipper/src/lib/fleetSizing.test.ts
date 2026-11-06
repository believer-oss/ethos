import { describe, expect, it } from 'vitest';
import {
	compactSizing,
	formToSizing,
	sizingToForm,
	overridesToRows,
	fleetSettingsToForm,
	formToShardSpec,
	rowsToOverrides,
	filledFrom,
	fleetMaps,
	parseBufferSize,
	parseReplicas,
	partialSizingIssues,
	policyText,
	resolveLiveSizing,
	resolvedSizingIssues,
	buildMapChoices,
	fleetsOnMissing,
	isValidMapId,
	mapSlug,
	normalizedMapId,
	fleetMapProblem,
	formSync,
	overrideRowIssues,
	pickBuildMaps,
	saveGeneration,
	servedVersions,
	sizingIssueText,
	specChanges,
	droppedOnSave,
	liveInherited,
	livePreviewText,
	resolvedLive,
	warmInherited
} from '$lib/fleetSizing';
import type { ChannelStatus, FleetRelease, FleetReleasePhase, ShardSpec } from '$lib/types';

const fields = (issues: { field?: string }[]) => issues.map((i) => i.field);

describe('parseBufferSize', () => {
	it('takes integers and percentages', () => {
		expect(parseBufferSize('2')).toBe(2);
		expect(parseBufferSize(' 20% ')).toBe('20%');
		expect(parseBufferSize('')).toBeNull();
	});

	it('refuses what the API refuses', () => {
		for (const bad of ['0', '0%', '100%', '1.5', '-1', '5 %', 'x', '+2']) {
			expect(parseBufferSize(bad), bad).toBeInstanceOf(Error);
		}
	});
});

describe('parseReplicas', () => {
	it('takes whole numbers at or above the minimum', () => {
		expect(parseReplicas('0', 0)).toBe(0);
		expect(parseReplicas('', 1)).toBeNull();
		expect(parseReplicas('0', 1)).toBeInstanceOf(Error);
		expect(parseReplicas('2.5', 0)).toBeInstanceOf(Error);
	});
});

describe('partialSizingIssues', () => {
	it('accepts partial and parked sizings', () => {
		expect(partialSizingIssues({})).toEqual([]);
		expect(partialSizingIssues({ minReplicas: 2, maxReplicas: 20 })).toEqual([]);
		expect(partialSizingIssues({ bufferSize: '20%', minReplicas: 1, maxReplicas: 10 })).toEqual([]);
		expect(partialSizingIssues({ parked: true, minReplicas: 5, maxReplicas: 1 })).toEqual([]);
	});

	it('applies the Agones Buffer rules', () => {
		expect(fields(partialSizingIssues({ minReplicas: 3, maxReplicas: 2 }))).toEqual([
			'minReplicas'
		]);
		expect(fields(partialSizingIssues({ bufferSize: 5, maxReplicas: 4 }))).toEqual(['bufferSize']);
		expect(fields(partialSizingIssues({ bufferSize: 2, minReplicas: 1 }))).toEqual(['minReplicas']);
		expect(partialSizingIssues({ bufferSize: 2, minReplicas: 0 })).toEqual([]);
		expect(fields(partialSizingIssues({ bufferSize: '20%', minReplicas: 0 }))).toEqual([
			'minReplicas'
		]);
	});

	it('bounds maxReplicas by the shard limit', () => {
		expect(partialSizingIssues({ maxReplicas: 50 }, 50)).toEqual([]);
		expect(partialSizingIssues({ maxReplicas: 51 }, 50)).toEqual([
			{ field: 'maxReplicas', message: "must not exceed the shard's limit of 50" }
		]);
		expect(partialSizingIssues({ maxReplicas: 51, parked: true }, 50)).toEqual([]);
	});
});

describe('resolvedSizingIssues', () => {
	it('needs bufferSize and maxReplicas, and treats unset minReplicas as 0', () => {
		expect(fields(resolvedSizingIssues({}))).toEqual(['bufferSize', 'maxReplicas']);
		expect(resolvedSizingIssues({ bufferSize: 2, maxReplicas: 5 })).toEqual([]);
		// A percentage needs minReplicas ≥ 1, and unset counts as 0.
		expect(fields(resolvedSizingIssues({ bufferSize: '20%', maxReplicas: 5 }))).toEqual([
			'minReplicas'
		]);
		expect(resolvedSizingIssues({ parked: true })).toEqual([]);
	});
});

describe('resolveLiveSizing', () => {
	const spec: ShardSpec = {
		fleetsEnabled: true,
		live: {
			default: { bufferSize: '20%', minReplicas: 1, maxReplicas: 10 },
			maps: { Arena: { minReplicas: 2, maxReplicas: 20 } },
			overrides: [{ cluster: 'cluster-a', mapId: 'Arena', sizing: { parked: true } }]
		}
	};

	it('takes the most specific level per field', () => {
		expect(resolveLiveSizing(spec, 'Arena', 'cluster-b')).toEqual({
			bufferSize: '20%',
			minReplicas: 2,
			maxReplicas: 20
		});
		expect(resolveLiveSizing(spec, 'Other', 'cluster-b')).toEqual(spec.live?.default);
		expect(resolveLiveSizing(spec, 'Arena', 'cluster-a').parked).toBe(true);
	});

	it('puts a release override above the shard', () => {
		const resolved = resolveLiveSizing(spec, 'Arena', 'cluster-a', [
			{ cluster: 'cluster-a', mapId: 'Arena', sizing: { parked: false, maxReplicas: 30 } }
		]);
		expect(resolved).toEqual({ bufferSize: '20%', minReplicas: 2, maxReplicas: 30, parked: false });
	});

	it('is empty with nothing set', () => {
		expect(resolveLiveSizing(null, 'Arena', 'cluster-a')).toEqual({});
	});

	it('fills only unset fields', () => {
		expect(filledFrom({ maxReplicas: 3 }, { bufferSize: 1, maxReplicas: 9 })).toEqual({
			bufferSize: 1,
			maxReplicas: 3
		});
	});
});

describe('policyText', () => {
	it('describes what is set', () => {
		expect(policyText({ bufferSize: '20%', minReplicas: 1, maxReplicas: 10 })).toBe(
			'Buffer 20% · min 1 · max 10'
		);
		expect(policyText({ parked: true, bufferSize: 2 })).toBe('Parked');
		expect(policyText({})).toBe('unset');
		expect(policyText(null)).toBe('unset');
	});
});

describe('fleet maps', () => {
	it('has no default map', () => {
		expect(fleetMaps({ fleetsEnabled: true, maps: ['Arena'] })).toEqual(['Arena']);
		expect(fleetMaps({ fleetsEnabled: true })).toEqual([]);
		expect(fleetMaps(null)).toEqual([]);
	});

	it('says what Fleets on still needs', () => {
		expect(
			fleetsOnMissing({ fleetsEnabled: true, maps: ['Arena'], warm: { bufferSize: 2 } })
		).toEqual([]);
		expect(fleetsOnMissing({ fleetsEnabled: false })).toEqual([
			'a warm buffer',
			'at least one fleet map'
		]);
		// A warm sizing without bufferSize isn't a buffer.
		expect(
			fleetsOnMissing({ fleetsEnabled: true, maps: ['Arena'], warm: { maxReplicas: 4 } })
		).toEqual(['a warm buffer']);
		expect(fleetsOnMissing(null)).toHaveLength(2);
	});

	it('takes map IDs that are label values', () => {
		expect(isValidMapId('TS_Arena.v2')).toBe(true);
		expect(isValidMapId('_Arena')).toBe(false);
		expect(isValidMapId('Arena Two')).toBe(false);
		expect(isValidMapId('Lab_ClockworkShutdowns')).toBe(true);
		expect(isValidMapId('a'.repeat(63))).toBe(true);
		expect(isValidMapId('a'.repeat(64))).toBe(false);
	});

	it('refuses fleet maps that would name the same Fleet', () => {
		expect(normalizedMapId('QA_BOX_Rotator')).toBe('qa-box-rotator');
		expect(normalizedMapId('Arena.v2')).toBe('arena-v2');
		expect(fleetMapProblem('Arena_Tutorial', ['Arena'])).toBe('');
		expect(fleetMapProblem('QA-BOX-Rotator', ['QA_BOX_Rotator'])).toBe(
			'matches QA_BOX_Rotator once case and -, _, . are ignored'
		);
		expect(fleetMapProblem('a'.repeat(64), [])).toMatch(/at most 63/);
		expect(fleetMapProblem('_Arena', [])).toMatch(/letters, digits/);
	});

	it('shortens long map slugs as fleet_api does', async () => {
		// The same cases as fleet_api's naming tests.
		expect(await mapSlug('Lab_ClockworkShutdowns')).toBe('lab-clockworkshutdowns');
		expect(await mapSlug('Abcdefghijklmnopqrstuvwxyz')).toBe('abcdefghijklmnopqrstuvwxyz');
		expect(await mapSlug('Abcdefghijklmnopqrstuvwxyz1')).toBe('abcdefghijklmnopqrs-ebf512');
		expect(await mapSlug('Abcdefghijklmnopqr_Stuvwxyz12')).toBe('abcdefghijklmnopqr-765839');
		expect(await mapSlug('m'.repeat(63))).toHaveLength(26);
	});

	const release = (
		name: string,
		phase: FleetReleasePhase,
		buildMapIds?: string[]
	): FleetRelease => ({
		metadata: { name },
		spec: { shard: 'test', version: name, phase: 'Warm', pinned: false, force: false },
		status: { phase, servedBy: [], buildMapIds, maps: [], clusters: [], conditions: [] }
	});

	it('offers the Live build’s maps, else the newest listed', () => {
		const newest = release('test-new', 'Warm', ['Arena', 'Gym']);
		const live = release('test-live', 'Live', ['Arena']);
		const unread = release('test-unread', 'Warming');
		expect(buildMapChoices([unread, newest, live])).toEqual({
			mapIds: ['Arena'],
			release: 'test-live'
		});
		expect(buildMapChoices([unread, newest])).toEqual({
			mapIds: ['Arena', 'Gym'],
			release: 'test-new'
		});
		expect(buildMapChoices([unread])).toBeNull();
		// Newest by creation, not list order.
		const older = release('test-old', 'Warm', ['Docks']);
		older.metadata.creationTimestamp = '2026-01-01T00:00:00Z';
		newest.metadata.creationTimestamp = '2026-01-02T00:00:00Z';
		expect(buildMapChoices([older, newest])?.release).toBe('test-new');
	});

	it('reads the map list from a served build first, newest-served first', () => {
		const channel = (key: string, version: string, since: string): ChannelStatus => ({
			key,
			kind: 'launcher',
			version,
			since,
			error: ''
		});
		const served = servedVersions([
			channel('launcher/a', 'old', '2026-01-01T00:00:00Z'),
			channel('launcher/b', 'new', '2026-01-02T00:00:00Z'),
			channel('launcher/c', 'old', '2026-01-03T00:00:00Z'),
			channel('launcher/d', '', '2026-01-04T00:00:00Z')
		]);
		expect(served).toEqual(['old', 'new']);

		const fromRelease = { mapIds: ['Arena'], release: 'test-cancelled' };
		// A served build's manifest wins; one without maps (an older manifest) is passed over.
		expect(
			pickBuildMaps(served, fromRelease, 'cancelled', 'ci', { old: [], new: ['Arena', 'Docks'] })
		).toEqual({ version: 'new', mapIds: ['Arena', 'Docks'] });
		// Then the releases' list, then the newest successful build's manifest.
		expect(pickBuildMaps(served, fromRelease, 'cancelled', 'ci', {})).toEqual({
			version: 'cancelled',
			mapIds: ['Arena']
		});
		expect(pickBuildMaps(served, null, undefined, 'ci', { ci: ['Plaza'] })).toEqual({
			version: 'ci',
			mapIds: ['Plaza']
		});
		expect(pickBuildMaps([], null, undefined, undefined, {})).toBeNull();
	});
});

describe('compactSizing', () => {
	it('drops empty fields', () => {
		expect(compactSizing({ bufferSize: 2, parked: false })).toEqual({ bufferSize: 2 });
		expect(compactSizing({})).toBeUndefined();
		expect(compactSizing({ parked: true })).toEqual({ parked: true });
	});
});

describe('sizing forms', () => {
	it('round-trips a sizing', () => {
		const sizing = { bufferSize: '20%', minReplicas: 1, maxReplicas: 10 };
		expect(formToSizing(sizingToForm(sizing))).toEqual({ sizing, issues: [] });
		expect(sizingToForm(null)).toEqual({
			bufferSize: '',
			minReplicas: '',
			maxReplicas: '',
			parked: false
		});
	});

	it('reports bad input and broken rules by field', () => {
		const { issues } = formToSizing({
			bufferSize: 'x',
			minReplicas: '3',
			maxReplicas: '2',
			parked: false
		});
		expect(fields(issues)).toEqual(['bufferSize', 'minReplicas']);
		expect(
			fields(
				formToSizing({ bufferSize: '', minReplicas: '', maxReplicas: '60', parked: false }, 50)
					.issues
			)
		).toEqual(['maxReplicas']);
	});

	it('refuses a percentage warm buffer', () => {
		const { issues } = formToSizing(
			{ bufferSize: '20%', minReplicas: '1', maxReplicas: '', parked: false },
			null,
			true
		);
		expect(issues).toEqual([
			{ field: 'bufferSize', message: 'must be a number of servers, not a percentage' }
		]);
	});
});

describe('override rows', () => {
	it('round-trip overrides and drop empty rows', () => {
		const overrides = [{ cluster: 'cluster-a', mapId: 'Arena', sizing: { parked: true } }];
		const rows = overridesToRows(overrides);
		expect(rowsToOverrides(rows)).toEqual(overrides);
		rows.push({ cluster: 'cluster-b', mapId: 'Arena', form: sizingToForm({}) });
		expect(rowsToOverrides(rows)).toEqual(overrides);
	});
});

describe('Fleet settings form', () => {
	const spec: ShardSpec = {
		fleetsEnabled: true,
		maps: ['Arena'],
		warm: { bufferSize: 2 },
		live: {
			default: { bufferSize: '20%', minReplicas: 1, maxReplicas: 10 },
			maps: { Arena: { maxReplicas: 20 } },
			overrides: [{ cluster: 'cluster-a', mapId: 'Arena', sizing: { parked: true } }]
		}
	};

	it('round-trips a spec', () => {
		expect(formToShardSpec(spec, fleetSettingsToForm(spec))).toEqual({ spec, issues: 0 });
		expect(formToShardSpec(null, fleetSettingsToForm(null))).toEqual({
			spec: { fleetsEnabled: false },
			issues: 0
		});
	});

	it('loads the first spec into a new form, and flags one edited under a new generation', () => {
		const empty = JSON.stringify(fleetSettingsToForm(null));
		// A new form is unedited, so the first generation loads instead of flagging it stale.
		expect(formSync(empty, empty, 3, null)).toEqual({ dirty: false, load: true, stale: false });
		// The Shard object doesn't exist yet: nothing to load.
		expect(formSync(empty, empty, null, null)).toEqual({
			dirty: false,
			load: false,
			stale: false
		});
		const loaded = JSON.stringify(fleetSettingsToForm(spec));
		const edited = JSON.stringify({
			...fleetSettingsToForm(spec),
			maps: ['Arena', 'Arena_Tutorial']
		});
		expect(formSync(edited, loaded, 3, 3)).toEqual({ dirty: true, load: false, stale: false });
		expect(formSync(edited, loaded, 4, 3)).toEqual({ dirty: true, load: false, stale: true });
		expect(formSync(loaded, loaded, 4, 3)).toEqual({ dirty: false, load: true, stale: false });
	});

	it('saves and checks per-map sizing only for ticked maps, and counts duplicate overrides once', () => {
		const form = fleetSettingsToForm(spec);
		// Unticked Arena keeps a hidden, invalid sizing: neither saved nor counted.
		form.maps = ['Arena_Tutorial'];
		form.liveMaps.Arena = sizingToForm({ minReplicas: 5, maxReplicas: 2 });
		form.overrides = [];
		const built = formToShardSpec(spec, form);
		expect(built.issues).toBe(0);
		expect(built.spec.live?.maps).toBeUndefined();

		// So are an unticked map's overrides: dropped on save, and said so.
		const unticked = fleetSettingsToForm(spec);
		unticked.maps = ['Arena_Tutorial'];
		unticked.overrides.push({ cluster: 'cluster-a', mapId: 'Arena', form: sizingToForm({}) });
		const dropping = formToShardSpec(spec, unticked);
		expect(dropping.issues).toBe(0);
		expect(dropping.spec.live?.overrides).toBeUndefined();
		expect(droppedOnSave(spec, unticked)).toEqual(['Arena']);
		expect(droppedOnSave(spec, fleetSettingsToForm(spec))).toEqual([]);

		const twice = fleetSettingsToForm(spec);
		twice.overrides = [...twice.overrides, ...twice.overrides];
		// Two rows for one (map, cluster): one problem each, as the overrides editor shows them.
		expect(formToShardSpec(spec, twice).issues).toBe(2);
	});

	it('flags an override row with nothing set, which saving would drop', () => {
		const empty = { cluster: 'cluster-a', mapId: 'Arena', form: sizingToForm({}) };
		expect(overrideRowIssues(empty, [empty])).toEqual([
			{ message: 'Set a value or Parked, or remove this row' }
		]);
		expect(sizingIssueText(overrideRowIssues(empty, [empty])[0])).toBe(
			'Set a value or Parked, or remove this row'
		);
		const parked = { ...empty, form: sizingToForm({ parked: true }) };
		expect(overrideRowIssues(parked, [parked])).toEqual([]);
		// In the shard's form it counts as a problem, so Save stays off.
		const form = fleetSettingsToForm(spec);
		form.overrides = [...form.overrides, { ...empty, cluster: 'cluster-b' }];
		expect(formToShardSpec(spec, form).issues).toBe(1);
	});

	it('names what changed underneath an edit', () => {
		expect(specChanges(spec, spec)).toEqual([]);
		expect(
			specChanges(spec, { ...spec, maps: ['Arena', 'Arena_Tutorial'], warm: { bufferSize: 3 } })
		).toEqual(['maps', 'warm buffer']);
		expect(specChanges(null, spec)).toEqual([
			'Fleets on/off',
			'maps',
			'warm buffer',
			'live default',
			'per-map live sizing',
			'per-cluster overrides'
		]);
	});

	it('labels issues with the field names the form shows', () => {
		expect(sizingIssueText({ field: 'bufferSize', message: 'must be at least 1' })).toBe(
			'Buffer: must be at least 1'
		);
		expect(sizingIssueText({ field: 'maxReplicas', message: 'is required' })).toBe(
			'Max: is required'
		);
	});

	it('drops empty levels and counts problems', () => {
		const form = fleetSettingsToForm(spec);
		form.liveMaps.Arena = sizingToForm({});
		form.warm.bufferSize = '20%';
		form.liveDefault.maxReplicas = '99';
		const { spec: out, issues } = formToShardSpec(spec, form, 50);
		expect(out.live?.maps).toBeUndefined();
		expect(issues).toBe(2);
	});
});

describe('Fleet settings grid', () => {
	const builtIn = { bufferSize: '20%', minReplicas: 1, maxReplicas: 10 };

	it('shows what empty cells inherit', () => {
		expect(warmInherited(50)).toEqual({
			bufferSize: '',
			minReplicas: '0',
			maxReplicas: '50 (limit)'
		});
		const empty = sizingToForm({});
		expect(liveInherited(empty, [], builtIn)).toEqual({
			bufferSize: '20% (default)',
			minReplicas: '1 (default)',
			maxReplicas: '10 (default)'
		});
		// A map row inherits what Defaults sets, and the coordinator's default for the rest.
		const defaults = sizingToForm({ maxReplicas: 20 });
		expect(liveInherited(empty, [defaults], builtIn)).toEqual({
			bufferSize: '20% (default)',
			minReplicas: '1 (default)',
			maxReplicas: '20'
		});
		// Under an absolute buffer the coordinator fills min as 0.
		const absolute = sizingToForm({ bufferSize: 2 });
		expect(liveInherited(absolute, [defaults], builtIn).minReplicas).toBe('0 (default)');
		expect(liveInherited(empty, [absolute], builtIn).minReplicas).toBe('0 (default)');
	});

	it('resolves the live sizing a Fleet runs, the coordinator default last', () => {
		const spec: ShardSpec = {
			fleetsEnabled: true,
			maps: ['Arena'],
			live: { maps: { Arena: { bufferSize: 2 } } }
		};
		expect(resolvedLive(spec, 'Arena', 'cluster-a', builtIn)).toEqual({
			bufferSize: 2,
			minReplicas: 0,
			maxReplicas: 10
		});
		expect(resolvedLive(null, 'Arena', 'cluster-a', builtIn)).toEqual(builtIn);
		const next: ShardSpec = { ...spec, live: { maps: { Arena: { bufferSize: 3 } } } };
		expect(livePreviewText(spec, next, 'Arena', ['cluster-a'], builtIn)).toBe(
			'now Buffer 2 · min 0 · max 10 → after Buffer 3 · min 0 · max 10'
		);
		expect(livePreviewText(spec, spec, 'Arena', ['cluster-a', 'cluster-b'], builtIn)).toBe(
			'cluster-a: Buffer 2 · min 0 · max 10 (unchanged)\ncluster-b: Buffer 2 · min 0 · max 10 (unchanged)'
		);
	});
});

describe('saveGeneration', () => {
	it('saves against the generation the form was loaded at, never a newer one', () => {
		expect(saveGeneration(4, 4)).toBe(4);
		// A poll brought generation 5 behind the open form: the save doesn't go.
		expect(saveGeneration(4, 5)).toBeNull();
		expect(saveGeneration(null, 4)).toBeNull();
		expect(saveGeneration(null, undefined)).toBeNull();
	});
});
