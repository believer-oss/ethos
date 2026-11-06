// Pure helpers for showing FleetReleases: one build's Fleets on one shard. No Tauri imports, so
// they can be tested on their own.

import type {
	FleetRelease,
	FleetReleasePhase,
	FleetsConfig,
	K8sCondition,
	Nullable,
	Promotion,
	ReleaseCluster,
	ReleaseClusterMap,
	ShardSummary
} from '$lib/types';
import {
	STALE_AFTER_MS,
	STEP_WARM_FLEETS,
	formatAge,
	formatAgo,
	formatSpan,
	gateStableForMs,
	gateTimeoutMs,
	isActivePromotion,
	promotionChangedBy,
	retireAfterIdleMs,
	shortSha
} from '$lib/fleetFormat';
import { clockTime } from '$lib/pollErrors';

// fleet_api::ANNOTATION_CHANGED_BY: who last changed the release (a username, or a promotion's
// name when a promotion created it).
const ANNOTATION_CHANGED_BY = 'fleet.believer.dev/changed-by';

export const releasePhase = (r: FleetRelease): Nullable<FleetReleasePhase> =>
	r.status?.phase ?? null;

export const releasePhaseLabel = (r: FleetRelease): string => {
	const phase = releasePhase(r);
	if (!phase) return 'Submitted';
	// Asked to retire, but the coordinator hasn't started yet.
	if (r.spec.phase === 'Retired' && phase !== 'Retiring' && phase !== 'Retired') return 'Retiring…';
	return phase;
};

export const releasePhaseClass = (r: FleetRelease): string => {
	switch (releasePhase(r)) {
		case 'Live':
			return 'text-green-400';
		case 'Warm':
			return 'text-lime-400';
		case 'Warming':
			return 'text-primary-400';
		case 'Draining':
		case 'Retiring':
			return 'text-yellow-400';
		case 'Failed':
			return 'text-red-400';
		default:
			return 'text-gray-400';
	}
};

export const releaseChangedBy = (r: FleetRelease): string =>
	r.metadata.annotations?.[ANNOTATION_CHANGED_BY] ?? 'unknown';

/** "test-abc12345 (abc1234)" */
export const releaseLabel = (r: FleetRelease): string =>
	`${r.metadata.name ?? r.spec.shard} (${shortSha(r.spec.version)})`;

/** Allocated servers across every cluster and map. */
export const releaseAllocated = (r: FleetRelease): number =>
	(r.status?.clusters ?? []).flatMap((c) => c.maps).reduce((n, m) => n + m.allocated, 0);

export interface MapTotals {
	mapId: string;
	desired: number;
	ready: number;
	allocated: number;
	// One per cluster, for the region strip.
	clusters: { name: string; map: Nullable<ReleaseClusterMap> }[];
}

/** Per fleet map, totals across clusters, with each cluster's own row. */
export const releaseMapTotals = (r: FleetRelease): MapTotals[] => {
	const clusters = r.status?.clusters ?? [];
	const mapIds = new Set<string>();
	(r.status?.maps ?? []).forEach((m) => mapIds.add(m.mapId));
	clusters.forEach((c) => {
		c.maps.forEach((m) => mapIds.add(m.mapId));
	});
	return [...mapIds].map((mapId) => {
		const rows = clusters.map((c) => ({
			name: c.name,
			map: c.maps.find((m) => m.mapId === mapId) ?? null
		}));
		const sum = (key: 'desired' | 'ready' | 'allocated') =>
			rows.reduce((n, row) => n + (row.map?.[key] ?? 0), 0);
		return {
			mapId,
			desired: sum('desired'),
			ready: sum('ready'),
			allocated: sum('allocated'),
			clusters: rows
		};
	});
};

export interface WarmCheck {
	// Why Warm can't go ahead, or null when it can.
	blocked: Nullable<string>;
	// What warming will do to the existing release, when it does something.
	note: string;
}

/**
 * Whether a build can be warmed on a shard where it already has `existing` (null: it has none). A
 * Retired release is final, so warming deletes it and creates a fresh one; one still on its way
 * there is waited for. The fleet store polls, so Warm enables itself once it's Retired.
 */
export const warmCheck = (existing: Nullable<FleetRelease> | undefined): WarmCheck => {
	if (!existing) return { blocked: null, note: '' };
	const name = existing.metadata.name ?? existing.spec.shard;
	if (existing.metadata.deletionTimestamp) {
		return { blocked: `${name} is being deleted; Warm is available once it's gone.`, note: '' };
	}
	const phase = releasePhase(existing);
	if (phase === 'Retired') {
		return {
			blocked: null,
			note: `${name} is Retired: warming deletes it and creates a fresh release.`
		};
	}
	if (phase === 'Retiring' || existing.spec.phase === 'Retired') {
		return {
			blocked: `${name} is still retiring; Warm is available once it's Retired.`,
			note: ''
		};
	}
	return {
		blocked: `This build already has a release here: ${name} (${releasePhaseLabel(existing)}).`,
		note: ''
	};
};

/** Why Retire can't go ahead without forcing it, or null when it can. */
export const retireBlocked = (r: FleetRelease): Nullable<string> => {
	const allocated = releaseAllocated(r);
	if (allocated === 0) return null;
	return `${allocated} server${allocated === 1 ? '' : 's'} still in a match`;
};

/** Asked to retire, whether or not the coordinator has started: phase Retiring or "Retiring…". */
export const isRetiring = (r: FleetRelease): boolean => {
	const phase = releasePhase(r);
	return phase === 'Retiring' || (r.spec.phase === 'Retired' && phase !== 'Retired');
};

/** "a", "a and b", "a, b and c". */
export const listText = (items: string[]): string =>
	items.length < 2 ? items.join('') : `${items.slice(0, -1).join(', ')} and ${items.at(-1)}`;

const plural = (n: number, one: string, many = `${one}s`) => `${n} ${n === 1 ? one : many}`;
const matchCount = (n: number) => plural(n, 'match', 'matches');
const readyServers = (r: FleetRelease): number =>
	(r.status?.clusters ?? []).flatMap((c) => c.maps).reduce((n, m) => n + m.ready, 0);

// fleet_api::conditions, the FleetRelease ones the card reads.
const MAPS_CHECKED = 'MapsChecked';
const CONFIG_VALID = 'ConfigValid';
const CLUSTERS_SYNCED = 'ClustersSynced';
const IMAGE_EXISTS = 'ImageExists';
const GATE_PASSED = 'GatePassed';

const falseCondition = (r: FleetRelease, type: string): Nullable<K8sCondition> =>
	(r.status?.conditions ?? []).find((c) => c.type === type && c.status === 'False') ?? null;

/** Whether the coordinator has lost touch with a cluster: an error, or no reading for 3 minutes. */
export const isUnreachable = (c: ReleaseCluster, now: number): boolean => {
	if (c.error) return true;
	const at = c.observedAt ? Date.parse(c.observedAt) : NaN;
	return !Number.isNaN(at) && now - at > STALE_AFTER_MS;
};

const unreachableLines = (r: FleetRelease, now: number): string[] => {
	const lines = (r.status?.clusters ?? [])
		.filter((c) => isUnreachable(c, now))
		.map((c) => {
			const age = formatAge(c.observedAt, now);
			return age && age !== 'now'
				? `Can't reach ${c.name} for ${age}. Counts are from then; its Fleets keep running on their own.`
				: `Can't reach ${c.name}. Its Fleets keep running on their own.`;
		});
	if (lines.length) return lines;
	const synced = falseCondition(r, CLUSTERS_SYNCED);
	if (synced?.reason !== 'Unreachable') return [];
	// The message usually names the cluster already: "can't reach <cluster>: <error>".
	const message = synced.message.replace(/\.$/, '');
	const text = /^can't reach /i.test(message)
		? `${message.charAt(0).toUpperCase()}${message.slice(1)}`
		: `Can't reach a cluster: ${message}`;
	return [`${text}. Its Fleets keep running on their own.`];
};

// The reasons a condition's message doesn't explain on its own, in plain words.
const REASON_SUMMARIES: Record<string, string> = {
	NoShardConfig: 'The shard has no ShardConfig',
	NoShard: 'The shard has no Shard object',
	NoFleets: "The shard's config has no Fleets section",
	IncompleteShardConfig: "The shard's config is missing a field Fleets need",
	SizingInvalid: "A map's sizing breaks the rules, so that cluster isn't updated",
	WriteFailed: "Couldn't update a cluster",
	GateFailed: 'The warm gate failed',
	NoLevelAssets: "The build's manifest lists no levels",
	NoCookedMaps: "The build's manifest lists no cooked server maps",
	NoFleetMaps: 'The shard has no fleet maps',
	ManifestUnreadable: "The build's manifest can't be read",
	ManifestPending: "Waiting for the build's manifest",
	// GatePassed: retiring or retired before the gate passed; final.
	Retired: 'Retired before its warm gate passed'
};

/** "The shard's gate can never pass: <why>. Fix the gate in the shard's ShardConfig." */
export const gateCannotPassText = (c: K8sCondition): string => {
	const why = c.message.replace(/,? so the gate can never pass\.?$/, '');
	return `The shard's gate can never pass: ${why}. Fix the gate in the shard's ShardConfig.`;
};

const conditionText = (c: K8sCondition): string => {
	if (c.reason === 'GateCannotPass') return gateCannotPassText(c);
	const summary = REASON_SUMMARIES[c.reason];
	if (!summary) return c.message || `${c.type}: ${c.reason}`;
	return c.message ? `${summary}: ${c.message}` : summary;
};

export type StatusTone = 'ok' | 'info' | 'warn' | 'bad';

export interface ReleaseStatus {
	tone: StatusTone;
	// One line per cluster when several are stuck or unreachable.
	text: string;
	hint?: string;
	// The cause is fixed in Fleet settings, so the card offers that button.
	settings?: boolean;
}

const mapsFailure = (r: FleetRelease, c: K8sCondition): ReleaseStatus => {
	const checks = r.status?.maps ?? [];
	const notIn = checks.filter((m) => m.check === 'NotInBuild').map((m) => m.mapId);
	const notCooked = checks.filter((m) => m.check === 'NotCooked').map((m) => m.mapId);
	const parts = [
		notIn.length ? `${listText(notIn)} (not in the build)` : '',
		notCooked.length ? `${listText(notCooked)} (not cooked)` : ''
	].filter(Boolean);
	const n = notIn.length + notCooked.length;
	if (c.reason !== 'MapsUnavailable' || !n) {
		return { tone: 'bad', text: conditionText(c), settings: c.reason === 'NoFleetMaps' };
	}
	return {
		tone: 'bad',
		text: `This build has no level for fleet ${n === 1 ? 'map' : 'maps'} ${parts.join(
			' / '
		)}, so its Fleets can't start.`,
		hint: `Untick ${n === 1 ? 'it' : 'them'} in Fleet settings, or promote a build that has ${
			n === 1 ? 'it' : 'them'
		}.`,
		settings: true
	};
};

const failure = (r: FleetRelease): ReleaseStatus => {
	const maps = falseCondition(r, MAPS_CHECKED);
	if (maps) return mapsFailure(r, maps);
	const config = falseCondition(r, CONFIG_VALID);
	if (config?.reason === 'GateCannotPass') return { tone: 'bad', text: gateCannotPassText(config) };
	const image = falseCondition(r, IMAGE_EXISTS);
	if (image) {
		const clusters = / per (.+)$/.exec(image.message)?.[1];
		return {
			tone: 'bad',
			text: `The game-server image for this build isn't in the registry${
				clusters ? ` (${clusters})` : ''
			}.`
		};
	}
	const other = config ?? falseCondition(r, GATE_PASSED) ?? falseCondition(r, CLUSTERS_SYNCED);
	return { tone: 'bad', text: other ? conditionText(other) : 'Failed.' };
};

const stuckLine = (r: FleetRelease, c: ReleaseCluster, now: number): string => {
	// phaseSince is when retiring began; retireStuck.since only when it was flagged, hours later.
	const since = r.status?.phaseSince ?? c.retireStuck?.since;
	const start = since ? Date.parse(since) : NaN;
	const when = Number.isNaN(start)
		? ''
		: ` since ${clockTime(new Date(start))} (${formatSpan(now - start)})`;
	const allocated = c.maps.reduce((n, m) => n + m.allocated, 0);
	const lastSeen = isUnreachable(c, now) ? ' (as last seen)' : '';
	return `Stuck: retiring${when} on ${c.name}, ${plural(
		allocated,
		'server'
	)} still in a match${lastSeen}. Retire never ends matches on its own.`;
};

const retiringStatus = (r: FleetRelease, now: number): ReleaseStatus => {
	const stuck = (r.status?.clusters ?? []).filter((c) => c.retireStuck);
	if (stuck.length) {
		return {
			tone: 'warn',
			text: [...unreachableLines(r, now), ...stuck.map((c) => stuckLine(r, c, now))].join('\n')
		};
	}
	const allocated = releaseAllocated(r);
	let text: string;
	if (!allocated) text = 'Removing Fleets.';
	else if (r.spec.force) text = `Ending ${matchCount(allocated)} and removing Fleets.`;
	else {
		const started = releasePhase(r) === 'Retiring' ? r.status?.phaseSince : null;
		const elapsed = started ? `, ${formatSpan(now - Date.parse(started))} so far` : '';
		text = `Retiring: ${retireBlocked(r) ?? ''}${elapsed}.`;
	}
	// As in the stuck case: an unreachable cluster comes first, and its counts are as last seen.
	const unreachable = unreachableLines(r, now);
	return unreachable.length
		? { tone: 'warn', text: [...unreachable, text].join('\n') }
		: { tone: 'info', text };
};

/** The running promotion whose warm-fleets step waits on this release, and when that step began. */
const warmingFor = (
	r: FleetRelease,
	promotions: Promotion[]
): Nullable<{ promotion: Promotion; startedAt: string }> => {
	for (const promotion of promotions) {
		const step = promotion.status?.steps.find(
			(s) =>
				s.name === STEP_WARM_FLEETS &&
				s.fleetRelease === r.metadata.name &&
				s.startedAt &&
				!s.finishedAt
		);
		if (step?.startedAt && isActivePromotion(promotion)) {
			return { promotion, startedAt: step.startedAt };
		}
	}
	return null;
};

const warmingStatus = (
	r: FleetRelease,
	fleets: Nullable<FleetsConfig> | undefined,
	promotions: Promotion[],
	now: number
): ReleaseStatus => {
	const needs = `needs ${fleets?.gate.minReady ?? 1}, then ${formatSpan(
		gateStableForMs(fleets)
	)} with no restarts`;
	let text = `Warm gate: ${readyServers(r)} Ready (${needs})`;
	const waiting = warmingFor(r, promotions);
	if (waiting) {
		const left = gateTimeoutMs(fleets) - (now - Date.parse(waiting.startedAt));
		text += ` · the promotion gives up in ${formatSpan(left)}`;
	}
	return { tone: 'info', text: `${text}.` };
};

const warmStatus = (r: FleetRelease, now: number): ReleaseStatus => {
	if (r.spec.pinned) return { tone: 'ok', text: 'Ready. Pinned: never retires on its own.' };
	const until = r.status?.warmUntil ? Date.parse(r.status.warmUntil) : NaN;
	if (Number.isNaN(until)) return { tone: 'ok', text: 'Ready. Not served yet.' };
	const left = until - now;
	const expires = left > 0 ? `expires in ${formatSpan(left)}` : 'expiring now';
	return { tone: 'ok', text: `Ready. Not served yet; ${expires} unless a promotion uses it.` };
};

const liveStatus = (r: FleetRelease): ReleaseStatus => {
	const ok = new Set((r.status?.maps ?? []).filter((m) => m.check === 'OK').map((m) => m.mapId));
	for (const c of r.status?.clusters ?? []) {
		const empty = c.maps.find((m) => ok.has(m.mapId) && m.desired > 0 && m.ready === 0);
		if (empty) return { tone: 'warn', text: `No Ready server in ${c.name}/${empty.mapId}.` };
	}
	return {
		tone: 'ok',
		text: `Healthy: ${matchCount(releaseAllocated(r))} running, ${plural(
			readyServers(r),
			'server'
		)} Ready.`
	};
};

const drainingStatus = (
	r: FleetRelease,
	fleets: Nullable<FleetsConfig> | undefined,
	now: number
): ReleaseStatus => {
	const allocated = releaseAllocated(r);
	const pinned = 'pinned: never retires on its own';
	if (allocated) {
		const retires = r.spec.pinned
			? pinned
			: `retires ${formatSpan(retireAfterIdleMs(fleets))} after the last ends`;
		return { tone: 'info', text: `${matchCount(allocated)} still running; ${retires}.` };
	}
	const idleSince = r.status?.idleSince ? Date.parse(r.status.idleSince) : NaN;
	const idle = Number.isNaN(idleSince) ? 'Idle' : `Idle ${formatAge(r.status?.idleSince, now)}`;
	if (r.spec.pinned) return { tone: 'info', text: `${idle} · ${pinned}.` };
	if (Number.isNaN(idleSince)) return { tone: 'info', text: `${idle}.` };
	const left = idleSince + retireAfterIdleMs(fleets) - now;
	return {
		tone: 'info',
		text: left > 0 ? `${idle} · retires in ${formatSpan(left)}.` : `${idle} · retiring now.`
	};
};

/**
 * What's going on with a release, in one sentence, with what to do about it when that's something.
 * The first case that applies wins: a retired build still served, a retirement stuck or under way,
 * an unreachable cluster, a failure's cause, then each phase's own state.
 */
export const releaseStatus = (
	r: FleetRelease,
	fleets: Nullable<FleetsConfig> | undefined,
	promotions: Promotion[],
	now: number
): ReleaseStatus => {
	const phase = releasePhase(r);
	const served = r.status?.servedBy ?? [];
	if (phase === 'Retired') {
		if (!served.length) return { tone: 'info', text: 'Retired: its Fleets are gone.' };
		return {
			tone: 'warn',
			text: `${listText(served)} still ${
				served.length === 1 ? 'serves' : 'serve'
			} this build, but it has no Fleets: its matches start on on-demand servers, or fail where the backend has no fallback.`,
			hint: `Warm it again to bring its Fleets back, or promote another build to ${served.join(
				' or '
			)}.`
		};
	}
	if (isRetiring(r)) return retiringStatus(r, now);
	const unreachable = unreachableLines(r, now);
	if (unreachable.length) return { tone: 'bad', text: unreachable.join('\n') };
	if (phase === 'Failed') return failure(r);
	const unsynced = falseCondition(r, CONFIG_VALID) ?? falseCondition(r, CLUSTERS_SYNCED);
	if (unsynced) return { tone: 'warn', text: conditionText(unsynced) };
	switch (phase) {
		case 'Warming':
			return warmingStatus(r, fleets, promotions, now);
		case 'Warm':
			return warmStatus(r, now);
		case 'Live':
			return liveStatus(r);
		case 'Draining':
			return drainingStatus(r, fleets, now);
		default:
			return { tone: 'info', text: 'Submitted: waiting for the coordinator.' };
	}
};

/**
 * The conditions and stuck reports behind the status sentence, for its Details disclosure: each
 * condition's type and reason, then its message, led by plain words where the reason needs them.
 */
export const releaseDetails = (r: FleetRelease): string[] => [
	...(r.status?.conditions ?? [])
		.filter((c) => c.status === 'False' && (!!c.message || c.reason in REASON_SUMMARIES))
		.map((c) => `${c.type} ${c.reason}: ${conditionText(c)}`),
	...(r.status?.clusters ?? []).flatMap((c) =>
		c.retireStuck ? [`${c.name} RetireStuck: ${c.retireStuck.message}`] : []
	)
];

// A Promotion's name: fleet-api's generateName prefix `<shard>-<channel kind>-`, then the five
// characters the apiserver appends.
const PROMOTION_NAME = /-(?:steam|launcher)-[a-z0-9]{5}$/;

/**
 * Who last changed it: "by alice", or "promoted by alice (promotion-name)" for a promotion's. A
 * promotion no longer retained is still named as one: "by promotion <name>".
 */
export const releaseWho = (r: FleetRelease, promotions: Promotion[]): string => {
	const by = releaseChangedBy(r);
	const promotion = promotions.find((p) => p.metadata.name === by);
	if (promotion) return `promoted by ${promotionChangedBy(promotion)} (${by})`;
	return PROMOTION_NAME.test(by) ? `by promotion ${by}` : `by ${by}`;
};

export type ChipTone = 'ok' | 'stale' | 'failed' | 'pending' | 'retiring' | 'refused' | 'off';

export interface ClusterChip {
	key: string;
	tone: ChipTone;
	text: string;
}

const REFUSED_TEXT: Record<string, string> = {
	NotInBuild: 'not in this build',
	NotCooked: 'not cooked'
};

/**
 * One chip per cluster and fleet map: "<cluster> · <map> · <in use> / <ready> / <desired> · <policy>
 * · seen <age> ago". A map the build has no level for gets one chip saying why, instead of zeros.
 */
export const releaseChips = (r: FleetRelease, now: number): ClusterChip[] => {
	const clusters = r.status?.clusters ?? [];
	const checks = new Map((r.status?.maps ?? []).map((m) => [m.mapId, m.check]));
	const mapIds = [
		...new Set([...checks.keys(), ...clusters.flatMap((c) => c.maps.map((m) => m.mapId))])
	];
	// A retired release has no Fleets to show.
	if (releasePhase(r) === 'Retired') return [];
	const retiring = isRetiring(r);
	const failed = releasePhase(r) === 'Failed';
	return mapIds.flatMap((mapId): ClusterChip[] => {
		const check = checks.get(mapId);
		if (check && check !== 'OK') {
			return [{ key: mapId, tone: 'refused', text: `${mapId} · ${REFUSED_TEXT[check] ?? check}` }];
		}
		const rendered = clusters.some((c) => c.maps.some((m) => m.mapId === mapId));
		if (!clusters.length || (failed && !rendered)) {
			return [{ key: mapId, tone: 'off', text: `${mapId} · OK, not started` }];
		}
		return clusters.map((c) => {
			const key = `${c.name}/${mapId}`;
			const m = c.maps.find((x) => x.mapId === mapId);
			if (!m) return { key, tone: 'off', text: `${c.name} · ${mapId} · no Fleet` };
			const stale = isUnreachable(c, now);
			const seen = formatAgo(c.observedAt, now);
			const parts = [
				c.name,
				mapId,
				`${m.allocated} in use / ${m.ready} ready / ${m.desired}`,
				m.policy,
				seen ? `${stale ? 'last seen' : 'seen'} ${seen}` : '',
				stale ? c.error : ''
			];
			let tone: ChipTone = 'ok';
			if (stale) tone = 'stale';
			else if (c.gate === 'Failed') tone = 'failed';
			else if (retiring) tone = 'retiring';
			else if (c.gate === 'Pending') tone = 'pending';
			return { key, tone, text: parts.filter(Boolean).join(' · ') };
		});
	});
};

export interface ReleaseSections {
	// Live, Retired but still served, and Draining with matches.
	serving: FleetRelease[];
	// Warming and Retiring.
	inProgress: FleetRelease[];
	// Warm, Failed and idle Draining.
	notServing: FleetRelease[];
	// Retired and served by nothing: kept a while as history.
	retired: FleetRelease[];
}

const PHASE_ORDER: Nullable<FleetReleasePhase>[] = [
	'Live',
	'Retired',
	'Draining',
	null,
	'Warming',
	'Retiring',
	'Warm',
	'Failed'
];

/** The shard page's release sections, each sorted by phase, then newest first. */
export const releaseSections = (releases: FleetRelease[]): ReleaseSections => {
	const sections: ReleaseSections = { serving: [], inProgress: [], notServing: [], retired: [] };
	for (const r of releases) {
		const phase = releasePhase(r);
		const served = (r.status?.servedBy ?? []).length > 0;
		if (isRetiring(r) || phase === 'Warming' || phase === null) sections.inProgress.push(r);
		else if (phase === 'Retired') (served ? sections.serving : sections.retired).push(r);
		else if (phase === 'Live' || (phase === 'Draining' && releaseAllocated(r) > 0)) {
			sections.serving.push(r);
		} else sections.notServing.push(r);
	}
	const rank = (r: FleetRelease) =>
		isRetiring(r) ? PHASE_ORDER.indexOf('Retiring') : PHASE_ORDER.indexOf(releasePhase(r));
	const created = (r: FleetRelease) => Date.parse(r.metadata.creationTimestamp ?? '') || 0;
	const order = (a: FleetRelease, b: FleetRelease) => rank(a) - rank(b) || created(b) - created(a);
	Object.values(sections).forEach((list: FleetRelease[]) => list.sort(order));
	return sections;
};

export interface RetireButton {
	label: string;
	force: boolean;
	// The red, last-resort look.
	danger: boolean;
}

export interface RetireDialog {
	title: string;
	// Matches would be ended: players dropped.
	bad: string;
	// Channels serve it: players are on this build now.
	warn: string;
	body: string;
	list: string[];
	footnote: string;
	buttons: RetireButton[];
}

/**
 * The retire dialog's content: what retiring this release does, given whether channels serve it and
 * matches run on it. `endMatches` is the card's "End matches now…" on a release already retiring:
 * the same dialog, without the option to wait.
 */
export const retireDialog = (r: FleetRelease, endMatches = false): RetireDialog => {
	const sha = shortSha(r.spec.version);
	const served = r.status?.servedBy ?? [];
	const allocated = releaseAllocated(r);
	const dialog: RetireDialog = {
		title: `Retire ${sha}?`,
		bad: '',
		warn: '',
		body: '',
		list: [],
		footnote: '',
		buttons: []
	};
	const servedNote = served.length
		? `Served by ${listText(served)}. Players on ${
				served.length === 1 ? 'that channel are' : 'those channels are'
		  } running this build now.`
		: '';
	if (allocated) {
		const them = allocated === 1 ? 'it' : 'them';
		const running = allocated === 1 ? 'A match is running' : `${allocated} matches are running`;
		dialog.title = endMatches
			? `${sha} is retiring: ${running.toLowerCase()}`
			: `Retire ${sha}? ${served.length ? `It's in use, and ${running.toLowerCase()}` : running}`;
		dialog.bad = `${matchCount(
			allocated
		)} running on this release's servers. Ending ${them} deletes ${
			allocated === 1 ? 'its server' : 'their servers'
		} now: those players are dropped from the match.`;
		dialog.warn = servedNote;
		if (!endMatches) {
			dialog.body = `Without ending ${them}, Retire waits for the ${
				allocated === 1 ? 'match' : 'matches'
			} to end, then removes the Fleets.`;
		}
		dialog.buttons = [
			...(endMatches
				? []
				: [
						{
							label: `Retire when the ${allocated === 1 ? 'match ends' : 'matches end'}`,
							force: false,
							danger: false
						}
				  ]),
			{ label: `End ${matchCount(allocated)} and retire`, force: true, danger: true }
		];
		return dialog;
	}
	if (!served.length) {
		dialog.body = 'Its Fleets are removed. You can warm it again later.';
		dialog.buttons = [{ label: 'Retire', force: false, danger: true }];
		return dialog;
	}
	const clusters = (r.status?.clusters ?? []).map((c) => c.name);
	const ready = readyServers(r);
	dialog.title = `Retire ${sha}? It's in use`;
	dialog.warn = servedNote;
	dialog.body = 'Retiring it:';
	dialog.list = [
		`removes its Fleets${clusters.length ? ` in ${listText(clusters)}` : ''} (${plural(
			ready,
			'Ready server'
		)}) once no match is running on them`,
		'new matches for this build start on on-demand servers (slower to start), or fail where the backend has no on-demand fallback',
		`leaves the ${
			served.length === 1 ? 'channel unchanged: it keeps' : 'channels unchanged: they keep'
		} serving ${sha} until a promotion moves ${served.length === 1 ? 'it' : 'them'}`
	];
	dialog.footnote =
		'To get Fleets back later: Warm again from the Retired list, or promote another build.';
	dialog.buttons = [{ label: 'Retire while in use', force: false, danger: true }];
	return dialog;
};

const WARM_PHASES: Nullable<FleetReleasePhase>[] = ['Warming', 'Warm'];

/** The Shards list's Fleets column: "Off", "On", or "On · 2 warm · 1 live · 1 draining". */
export const fleetsColumnText = (shard: ShardSummary): string => {
	if (!shard.config?.fleets) return '—';
	if (!shard.spec?.fleetsEnabled) return 'Off';
	const releases = shard.status?.releases ?? [];
	const count = (match: (p: Nullable<FleetReleasePhase> | undefined) => boolean) =>
		releases.filter((r) => match(r.phase)).length;
	const parts = [
		[count((p) => WARM_PHASES.includes(p ?? null)), 'warm'],
		[count((p) => p === 'Live'), 'live'],
		[count((p) => p === 'Draining'), 'draining']
	] as const;
	const counted = parts.filter(([n]) => n > 0).map(([n, label]) => `${n} ${label}`);
	return ['On', ...counted].join(' · ');
};

export interface ReleaseToast {
	kind: 'success' | 'error';
	message: string;
	shard: string;
}

/** Whether this user started the release's latest change (warmed or retired it). */
export const isReleaseMine = (r: FleetRelease, localName: Nullable<string>): boolean =>
	!!localName && r.metadata.annotations?.[ANNOTATION_CHANGED_BY] === localName;

const releaseToast = (
	r: FleetRelease,
	from: Nullable<FleetReleasePhase> | undefined,
	to: FleetReleasePhase
): Nullable<ReleaseToast> => {
	const { shard } = r.spec;
	const label = releaseLabel(r);
	if (to === 'Warm' && from === 'Warming') {
		return {
			kind: 'success',
			message: `Release warm: ${label} passed the gate on ${shard}`,
			shard
		};
	}
	if (to === 'Failed') {
		return { kind: 'error', message: `Release failed: ${label} on ${shard} has no Fleets`, shard };
	}
	if (to === 'Retired' && r.spec.phase === 'Retired') {
		return { kind: 'success', message: `Release retired: ${label} on ${shard}`, shard };
	}
	return null;
};

/**
 * Tracks release phases between polls, as PromotionTransitions does for promotions, and toasts
 * only the person whose change it was (`changed-by`): who warmed it hears it went Warm or Failed,
 * who retired it hears it's Retired. A release a promotion created carries the promotion's name,
 * so it toasts nobody; the promotion's own toast covers it. The first update is the baseline.
 */
export class ReleaseTransitions {
	private phases: Nullable<Map<string, Nullable<FleetReleasePhase>>> = null;

	update(releases: FleetRelease[], localName: Nullable<string>): ReleaseToast[] {
		const toasts: ReleaseToast[] = [];
		const previous = this.phases;
		const next = new Map<string, Nullable<FleetReleasePhase>>();
		for (const r of releases) {
			const { name } = r.metadata;
			if (name) {
				const phase = releasePhase(r);
				next.set(name, phase);
				const before = previous?.get(name);
				if (previous && phase && phase !== before && isReleaseMine(r, localName)) {
					const toast = releaseToast(r, before, phase);
					if (toast) toasts.push(toast);
				}
			}
		}
		previous?.forEach((phase, name) => {
			if (!next.has(name)) next.set(name, phase);
		});
		this.phases = next;
		return toasts;
	}
}
