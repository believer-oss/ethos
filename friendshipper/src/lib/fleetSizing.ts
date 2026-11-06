// Pure helpers for Fleet sizing (Agones Buffer policies) in the shard's Fleet settings. They follow
// fleet-api's `Sizing`: the same rules, so the form refuses what the API would.

import type {
	ChannelStatus,
	FleetRelease,
	Nullable,
	ShardSpec,
	Sizing,
	SizingOverride
} from '$lib/types';

export type SizingField = 'bufferSize' | 'minReplicas' | 'maxReplicas';

export interface SizingIssue {
	// The input it's about; absent for a problem with the whole row.
	field?: SizingField;
	message: string;
}

// The form's label for each field, as the inputs show it.
const SIZING_FIELD_LABELS: Record<SizingField, string> = {
	bufferSize: 'Buffer',
	minReplicas: 'Min',
	maxReplicas: 'Max'
};

/** An issue as people read it: "Buffer: must be at least 1". */
export const sizingIssueText = (issue: SizingIssue): string =>
	issue.field ? `${SIZING_FIELD_LABELS[issue.field]}: ${issue.message}` : issue.message;

/** A bufferSize as typed in a form: "2" or "20%". Null when empty; an Error when invalid. */
export const parseBufferSize = (input: string): Nullable<number | string> | Error => {
	const text = input.trim();
	if (text === '') return null;
	const percent = /^([1-9][0-9]?)%$/.exec(text);
	if (percent) return `${percent[1]}%`;
	if (/^[1-9][0-9]*$/.test(text)) return Number(text);
	return new Error('a whole number of at least 1, or a percentage from 1% to 99%');
};

/** A replica count as typed in a form. Null when empty; an Error when invalid. */
export const parseReplicas = (input: string, min: number): Nullable<number> | Error => {
	const text = input.trim();
	if (text === '') return null;
	if (!/^[0-9]+$/.test(text) || Number(text) < min) {
		return new Error(`a whole number of at least ${min}`);
	}
	return Number(text);
};

const isPercent = (b: number | string | undefined): b is string =>
	typeof b === 'string' && b.endsWith('%');

const percentValue = (b: string): number => Number(b.slice(0, -1));

/**
 * The rules the API checks on one sizing object, on the fields it sets, plus the shard's
 * per-fleet limit on maxReplicas. A partial object is fine; a parked one always is.
 */
export const partialSizingIssues = (sizing: Sizing, limit?: Nullable<number>): SizingIssue[] => {
	if (sizing.parked) return [];
	const issues: SizingIssue[] = [];
	const { bufferSize: buffer, minReplicas: min, maxReplicas: max } = sizing;
	if (min !== undefined && max !== undefined && min > max) {
		issues.push({ field: 'minReplicas', message: 'must not exceed Max' });
	}
	if (max !== undefined && limit && max > limit) {
		issues.push({ field: 'maxReplicas', message: `must not exceed the shard's limit of ${limit}` });
	}
	if (buffer !== undefined) {
		if (isPercent(buffer)) {
			const p = percentValue(buffer);
			if (!(p >= 1 && p <= 99)) {
				issues.push({ field: 'bufferSize', message: 'a percentage must be 1% to 99%' });
			}
			if (min !== undefined && min < 1) {
				issues.push({
					field: 'minReplicas',
					message: 'must be at least 1 with a percentage buffer'
				});
			}
		} else {
			const b = Number(buffer);
			if (!(b >= 1)) issues.push({ field: 'bufferSize', message: 'must be at least 1' });
			if (max !== undefined && b > max) {
				issues.push({ field: 'bufferSize', message: 'must not exceed Max' });
			}
			if (min !== undefined && min !== 0 && min < b) {
				issues.push({ field: 'minReplicas', message: 'must be 0 or at least the buffer' });
			}
		}
	}
	return issues;
};

/**
 * The rules on a resolved sizing, as fleet-api's `Sizing::validate`: bufferSize and maxReplicas are
 * required, an unset minReplicas counts as 0, and every Buffer rule applies.
 */
export const resolvedSizingIssues = (sizing: Sizing, limit?: Nullable<number>): SizingIssue[] => {
	if (sizing.parked) return [];
	const issues: SizingIssue[] = [];
	if (sizing.bufferSize === undefined) issues.push({ field: 'bufferSize', message: 'is required' });
	if (sizing.maxReplicas === undefined) {
		issues.push({ field: 'maxReplicas', message: 'is required' });
	}
	return issues.concat(
		partialSizingIssues({ ...sizing, minReplicas: sizing.minReplicas ?? 0 }, limit)
	);
};

/** Unset fields of `sizing` taken from `base`, the next less specific level. */
export const filledFrom = (sizing: Nullable<Sizing> | undefined, base: Sizing): Sizing => {
	const out: Sizing = { ...base };
	(Object.keys(sizing ?? {}) as (keyof Sizing)[]).forEach((key) => {
		const value = sizing?.[key];
		if (value !== undefined) (out as Record<string, unknown>)[key] = value;
	});
	return out;
};

/**
 * The live sizing for one (map, cluster), most specific first: the release's override, the
 * shard's override, the shard's per-map sizing, then its default. Fields left unset at every level
 * fall to the coordinator's defaults, which this doesn't know.
 */
export const resolveLiveSizing = (
	spec: Nullable<ShardSpec> | undefined,
	mapId: string,
	cluster: string,
	releaseOverrides: SizingOverride[] = []
): Sizing => {
	const live = spec?.live;
	const find = (list: SizingOverride[] | undefined) =>
		list?.find((o) => o.mapId === mapId && o.cluster === cluster)?.sizing;
	return [
		find(releaseOverrides),
		find(live?.overrides),
		live?.maps?.[mapId],
		live?.default
	].reduce<Sizing>((acc, level) => filledFrom(acc, level ?? {}), {});
};

/** "Buffer 20% · min 1 · max 10", "Parked", or what is set of that. */
export const policyText = (sizing: Nullable<Sizing> | undefined): string => {
	if (!sizing) return 'unset';
	if (sizing.parked) return 'Parked';
	const parts: string[] = [];
	if (sizing.bufferSize !== undefined) parts.push(`Buffer ${sizing.bufferSize}`);
	if (sizing.minReplicas !== undefined) parts.push(`min ${sizing.minReplicas}`);
	if (sizing.maxReplicas !== undefined) parts.push(`max ${sizing.maxReplicas}`);
	return parts.length ? parts.join(' · ') : 'unset';
};

// fleet_api's MAP_ID_PATTERN and MAP_ID_MAX_LEN: a map ID is a label value.
const MAP_ID = /^[A-Za-z0-9]([-A-Za-z0-9_.]*[A-Za-z0-9])?$/;
export const MAP_ID_MAX_LEN = 63;
// fleet_api's MAP_SLUG_MAX_LEN: what a Fleet name, `<shard>-<sha8>-<map slug>`, leaves for the slug.
export const MAP_SLUG_MAX_LEN = 26;
const SLUG_HASH_LEN = 6;

export const isValidMapId = (mapId: string): boolean =>
	mapId.length <= MAP_ID_MAX_LEN && MAP_ID.test(mapId);

/** A map ID lowercased with every other character turned into `-`: map IDs that match here would
 * name the same Fleet, which the Shard's CEL refuses. */
export const normalizedMapId = (mapId: string): string =>
	mapId
		.toLowerCase()
		.replace(/[^a-z0-9]/g, '-')
		.replace(/^-+|-+$/g, '');

/** fleet_api's map_slug: the map ID as it appears in its Fleet's name. One longer than
 * MAP_SLUG_MAX_LEN is cut to 19 characters, then `-` and 6 hex characters of its SHA-256. */
export const mapSlug = async (mapId: string): Promise<string> => {
	const slug = normalizedMapId(mapId);
	if (slug.length <= MAP_SLUG_MAX_LEN) return slug;
	const cut = slug.slice(0, MAP_SLUG_MAX_LEN - 1 - SLUG_HASH_LEN).replace(/-+$/, '');
	const digest = await crypto.subtle.digest('SHA-256', new TextEncoder().encode(mapId));
	const hash = Array.from(new Uint8Array(digest), (b) => b.toString(16).padStart(2, '0')).join('');
	return `${cut}-${hash.slice(0, SLUG_HASH_LEN)}`;
};

/** Why a map can't join `maps` as a fleet map, or '' if it can. */
export const fleetMapProblem = (mapId: string, maps: string[]): string => {
	if (!MAP_ID.test(mapId))
		return 'letters, digits, -, _ and ., starting and ending with a letter or digit';
	if (mapId.length > MAP_ID_MAX_LEN) return `at most ${MAP_ID_MAX_LEN} characters`;
	const twin = maps.find((m) => m !== mapId && normalizedMapId(m) === normalizedMapId(mapId));
	return twin ? `matches ${twin} once case and -, _, . are ignored` : '';
};

/** The maps that get Fleets. There is no default map: Fleets on needs at least one. */
export const fleetMaps = (spec: Nullable<ShardSpec> | undefined): string[] => spec?.maps ?? [];

/**
 * What a shard's spec lacks for Fleets to be on, as the API's rules require: a warm buffer and at
 * least one fleet map. Empty when nothing is missing.
 */
export const fleetsOnMissing = (spec: Nullable<ShardSpec> | undefined): string[] => {
	const missing: string[] = [];
	if (spec?.warm?.bufferSize === undefined) missing.push('a warm buffer');
	if (!spec?.maps?.length) missing.push('at least one fleet map');
	return missing;
};

export interface BuildMaps {
	mapIds: string[];
	// The release whose build listed them.
	release: string;
}

/**
 * The maps a build on this shard has levels for, from its releases: the Live release's, else the
 * newest release's (by creation) whose manifest has been read. Null when no release lists any.
 */
export const buildMapChoices = (releases: FleetRelease[]): Nullable<BuildMaps> => {
	const created = (r: FleetRelease) => Date.parse(r.metadata.creationTimestamp ?? '') || 0;
	const listed = releases
		.filter((r) => r.status?.buildMapIds?.length)
		.sort((a, b) => created(b) - created(a));
	const pick = listed.find((r) => r.status?.phase === 'Live') ?? listed.at(0);
	if (!pick?.status?.buildMapIds) return null;
	return { mapIds: pick.status.buildMapIds, release: pick.metadata.name ?? pick.spec.shard };
};

/**
 * The builds the shard's channels serve, the most recently arrived first, each once: whose manifest
 * the Fleet settings map list reads first.
 */
export const servedVersions = (channels: ChannelStatus[]): string[] => {
	const since = (c: ChannelStatus) => Date.parse(c.since ?? '') || 0;
	return [
		...new Set(
			channels
				.filter((c) => c.version)
				.sort((a, b) => since(b) - since(a))
				.map((c) => c.version)
		)
	];
};

/** A sizing object with empty fields dropped, as the API stores it; undefined when nothing is set. */
export const compactSizing = (sizing: Sizing): Sizing | undefined => {
	const out: Sizing = {};
	if (sizing.parked) out.parked = true;
	if (sizing.bufferSize !== undefined && sizing.bufferSize !== '')
		out.bufferSize = sizing.bufferSize;
	if (sizing.minReplicas !== undefined) out.minReplicas = sizing.minReplicas;
	if (sizing.maxReplicas !== undefined) out.maxReplicas = sizing.maxReplicas;
	return Object.keys(out).length ? out : undefined;
};

/** A sizing as a form edits it: text fields, so partial and invalid input can be shown. */
export interface SizingForm {
	bufferSize: string;
	minReplicas: string;
	maxReplicas: string;
	parked: boolean;
}

export const sizingToForm = (sizing: Nullable<Sizing> | undefined): SizingForm => ({
	bufferSize: sizing?.bufferSize === undefined ? '' : String(sizing.bufferSize),
	minReplicas: sizing?.minReplicas === undefined ? '' : String(sizing.minReplicas),
	maxReplicas: sizing?.maxReplicas === undefined ? '' : String(sizing.maxReplicas),
	parked: sizing?.parked === true
});

/**
 * The form's sizing, and every problem with it: unparseable fields, then the API's rules, the
 * shard's limit, and (with `absoluteOnly`, for the warm buffer) a percentage buffer.
 */
export const formToSizing = (
	form: SizingForm,
	limit?: Nullable<number>,
	absoluteOnly = false
): { sizing: Sizing; issues: SizingIssue[] } => {
	const issues: SizingIssue[] = [];
	const sizing: Sizing = {};
	const buffer = parseBufferSize(form.bufferSize);
	if (buffer instanceof Error) issues.push({ field: 'bufferSize', message: buffer.message });
	else if (buffer !== null) sizing.bufferSize = buffer;
	const min = parseReplicas(form.minReplicas, 0);
	if (min instanceof Error) issues.push({ field: 'minReplicas', message: min.message });
	else if (min !== null) sizing.minReplicas = min;
	const max = parseReplicas(form.maxReplicas, 1);
	if (max instanceof Error) issues.push({ field: 'maxReplicas', message: max.message });
	else if (max !== null) sizing.maxReplicas = max;
	if (form.parked) sizing.parked = true;
	if (absoluteOnly && typeof sizing.bufferSize === 'string') {
		issues.push({ field: 'bufferSize', message: 'must be a number of servers, not a percentage' });
	}
	return { sizing, issues: issues.concat(partialSizingIssues(sizing, limit)) };
};

/** One override as the editor holds it. */
export interface OverrideRow {
	cluster: string;
	mapId: string;
	form: SizingForm;
}

export const overridesToRows = (overrides: SizingOverride[] | undefined): OverrideRow[] =>
	(overrides ?? []).map((o) => ({
		cluster: o.cluster,
		mapId: o.mapId,
		form: sizingToForm(o.sizing)
	}));

/**
 * One override row's problems, as the overrides editor shows them: its sizing's, a second row for the
 * same map and cluster, and a row with nothing set (which saving would drop).
 */
export const overrideRowIssues = (
	row: OverrideRow,
	all: OverrideRow[],
	limit?: Nullable<number>
): SizingIssue[] => {
	const { issues, sizing } = formToSizing(row.form, limit);
	if (all.filter((r) => r.cluster === row.cluster && r.mapId === row.mapId).length > 1)
		issues.push({ message: 'a second override for this map and cluster' });
	if (!issues.length && !compactSizing(sizing))
		issues.push({ message: 'Set a value or Parked, or remove this row' });
	return issues;
};

/** The rows as overrides; rows with nothing set are dropped. */
export const rowsToOverrides = (rows: OverrideRow[]): SizingOverride[] =>
	rows.flatMap((row) => {
		const sizing = compactSizing(formToSizing(row.form).sizing);
		return sizing ? [{ cluster: row.cluster, mapId: row.mapId, sizing }] : [];
	});

/** The Fleet settings form: everything in a Shard's spec except the on/off switch. */
export interface FleetSettingsForm {
	maps: string[];
	warm: SizingForm;
	liveDefault: SizingForm;
	liveMaps: Record<string, SizingForm>;
	overrides: OverrideRow[];
}

export const fleetSettingsToForm = (spec: Nullable<ShardSpec> | undefined): FleetSettingsForm => ({
	maps: [...(spec?.maps ?? [])],
	warm: sizingToForm(spec?.warm),
	liveDefault: sizingToForm(spec?.live?.default),
	liveMaps: Object.fromEntries(
		Object.entries(spec?.live?.maps ?? {}).map(([mapId, sizing]) => [mapId, sizingToForm(sizing)])
	),
	overrides: overridesToRows(spec?.live?.overrides)
});

/** What differs between two Shard specs, in the form's words: "Fleets on/off", "maps", "warm buffer", … */
export const specChanges = (
	before: Nullable<ShardSpec> | undefined,
	after: Nullable<ShardSpec> | undefined
): string[] => {
	const same = (a: unknown, b: unknown) => JSON.stringify(a ?? null) === JSON.stringify(b ?? null);
	return [
		['Fleets on/off', !!before?.fleetsEnabled === !!after?.fleetsEnabled],
		['maps', same(before?.maps, after?.maps)],
		['warm buffer', same(before?.warm, after?.warm)],
		['live default', same(before?.live?.default, after?.live?.default)],
		['per-map live sizing', same(before?.live?.maps, after?.live?.maps)],
		['per-cluster overrides', same(before?.live?.overrides, after?.live?.overrides)]
	]
		.filter(([, unchanged]) => !unchanged)
		.map(([what]) => what as string);
};

/**
 * Where the form stands against the Shard. `dirty`: edited since it was loaded. `load`: unedited
 * and the spec has a new generation (or the first one), so the form should load it. `stale`: edited
 * and the spec has a new generation, so a save would be refused.
 */
export const formSync = (
	formJson: string,
	loadedJson: string,
	generation: Nullable<number> | undefined,
	loadedAt: Nullable<number>
): { dirty: boolean; load: boolean; stale: boolean } => {
	const dirty = formJson !== loadedJson;
	const moved = (generation ?? null) !== loadedAt;
	return { dirty, load: !dirty && moved, stale: dirty && moved };
};

/**
 * The spec the form describes, keeping `fleetsEnabled` from `base`. Per-map sizing and overrides are
 * kept only for fleet maps (ticked ones); an unticked map's are neither saved nor checked.
 * `issues` counts every problem the form's sizings and override rows have (`overrideRowIssues`),
 * once each.
 */
export const formToShardSpec = (
	base: Nullable<ShardSpec> | undefined,
	form: FleetSettingsForm,
	limit?: Nullable<number>
): { spec: ShardSpec; issues: number } => {
	let issues = 0;
	const parse = (f: SizingForm, absoluteOnly = false) => {
		const result = formToSizing(f, limit, absoluteOnly);
		issues += result.issues.length;
		return compactSizing(result.sizing);
	};
	const spec: ShardSpec = { fleetsEnabled: base?.fleetsEnabled === true };
	if (form.maps.length) spec.maps = [...form.maps];
	const warm = parse({ ...form.warm, parked: false }, true);
	if (warm) spec.warm = warm;
	const live: NonNullable<ShardSpec['live']> = {};
	const liveDefault = parse(form.liveDefault);
	if (liveDefault) live.default = liveDefault;
	const liveMaps: Record<string, Sizing> = {};
	Object.entries(form.liveMaps).forEach(([mapId, f]) => {
		if (!form.maps.includes(mapId)) return;
		const sizing = parse(f);
		if (sizing) liveMaps[mapId] = sizing;
	});
	if (Object.keys(liveMaps).length) live.maps = liveMaps;
	// An unticked map's overrides go with its per-map sizing: neither saved nor checked.
	const ticked = form.overrides.filter((row) => form.maps.includes(row.mapId));
	ticked.forEach((row) => {
		issues += overrideRowIssues(row, ticked, limit).length;
	});
	const overrides = rowsToOverrides(ticked);
	if (overrides.length) live.overrides = overrides;
	if (Object.keys(live).length) spec.live = live;
	return { spec, issues };
};

/**
 * The maps whose saved live sizing or per-cluster overrides the next save drops, because they're
 * unticked in the form.
 */
export const droppedOnSave = (
	saved: Nullable<ShardSpec> | undefined,
	form: FleetSettingsForm
): string[] => {
	const withSizing = [
		...Object.keys(saved?.live?.maps ?? {}),
		...(saved?.live?.overrides ?? []).map((o) => o.mapId)
	];
	return [...new Set(withSizing)].filter((m) => !form.maps.includes(m)).sort();
};

/** What a row's empty cells inherit, as the grid shows them in grey: "20% (default)", "50 (limit)". */
export type Inherited = Record<SizingField, string>;

/** The Defaults row's warm cells: the buffer has no default; min is 0, max the shard's limit. */
export const warmInherited = (limit: Nullable<number> | undefined): Inherited => ({
	bufferSize: '',
	minReplicas: '0',
	maxReplicas: limit ? `${limit} (limit)` : ''
});

const isAbsolute = (buffer: string): boolean => buffer !== '' && !buffer.endsWith('%');

/**
 * What a row's empty live cells inherit: the first less specific row (`above`, nearest first) that
 * sets the field, else the coordinator's default (`builtIn`). A built-in min under an absolute
 * buffer is 0, as the coordinator fills it.
 */
export const liveInherited = (
	own: SizingForm,
	above: SizingForm[],
	builtIn: Nullable<Sizing> | undefined
): Inherited => {
	const pick = (field: SizingField): { value: string; builtIn: boolean } => {
		const set = above.map((f) => f[field].trim()).find((v) => v !== '');
		if (set !== undefined) return { value: set, builtIn: false };
		const value = builtIn?.[field];
		return { value: value === undefined ? '' : String(value), builtIn: true };
	};
	const show = ({ value, builtIn: fromDefault }: { value: string; builtIn: boolean }) =>
		value && fromDefault ? `${value} (default)` : value;
	const buffer = own.bufferSize.trim() || pick('bufferSize').value;
	const min = pick('minReplicas');
	return {
		bufferSize: show(pick('bufferSize')),
		minReplicas: min.builtIn && isAbsolute(buffer) ? '0 (default)' : show(min),
		maxReplicas: show(pick('maxReplicas'))
	};
};

/**
 * The live sizing a (map, cluster) Fleet runs: the shard's levels, then the coordinator's default
 * (`builtIn`, its max already capped at the limit), with a built-in min of 0 under an absolute
 * buffer.
 */
export const resolvedLive = (
	spec: Nullable<ShardSpec> | undefined,
	mapId: string,
	cluster: string,
	builtIn: Nullable<Sizing> | undefined
): Sizing => {
	const set = resolveLiveSizing(spec, mapId, cluster);
	const out = filledFrom(set, builtIn ?? {});
	if (set.minReplicas === undefined && typeof out.bufferSize === 'number' && !out.parked) {
		out.minReplicas = 0;
	}
	return out;
};

/** A map row's live preview, one line per cluster: "now Buffer 2 · min 0 · max 4 → after …". */
export const livePreviewText = (
	saved: Nullable<ShardSpec> | undefined,
	next: ShardSpec,
	mapId: string,
	clusters: string[],
	builtIn: Nullable<Sizing> | undefined
): string =>
	clusters
		.map((cluster) => {
			const before = policyText(resolvedLive(saved, mapId, cluster, builtIn));
			const after = policyText(resolvedLive(next, mapId, cluster, builtIn));
			const what = before === after ? `${after} (unchanged)` : `now ${before} → after ${after}`;
			return clusters.length > 1 ? `${cluster}: ${what}` : what;
		})
		.join('\n');

/**
 * The Fleet settings map list's source, the first with any maps: the manifest of a served build
 * (`served` in order), the releases' list, then the newest successful build's manifest.
 * `manifestMapIds` holds the manifests read so far, by version.
 *
 * The order is a best guess, likely to need revision with experience. It suits maps the serving
 * build already has; revisit it if warming a map only a newer build has becomes a real case.
 */
export const pickBuildMaps = (
	served: string[],
	fromRelease: Nullable<BuildMaps>,
	releaseVersion: string | undefined,
	newestBuild: string | undefined,
	manifestMapIds: Partial<Record<string, string[]>>
): Nullable<{ version: string; mapIds: string[] }> => {
	for (const version of served) {
		const mapIds = manifestMapIds[version];
		if (mapIds?.length) return { version, mapIds };
	}
	if (fromRelease) return { version: releaseVersion ?? '', mapIds: fromRelease.mapIds };
	const newest = newestBuild ? manifestMapIds[newestBuild] : undefined;
	return newestBuild && newest?.length ? { version: newestBuild, mapIds: newest } : null;
};

/** Why a save didn't go: the spec changed since the form was loaded. */
export const STALE_SAVE =
	'these settings changed since you opened them. Reset to see the changes, then save again.';

/**
 * The generation a save tests: the one the form was loaded at, while it is still the current one.
 * Null when the spec has moved on since (saving would overwrite changes the form never showed) or
 * nothing is loaded yet.
 */
export const saveGeneration = (
	loadedAt: Nullable<number>,
	current: Nullable<number> | undefined
): Nullable<number> => (loadedAt !== null && loadedAt === (current ?? null) ? loadedAt : null);
