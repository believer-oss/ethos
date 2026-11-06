// Pure helpers for showing fleet objects (shards, channels, promotions). No Tauri imports, so
// they can be tested on their own.

import type {
	ActiveBuild,
	BackendParts,
	BackendStatus,
	ChannelRef,
	ChannelStatus,
	FleetsConfig,
	Nullable,
	Promotion,
	PromotionCancel,
	PromotionChange,
	PromotionPhase,
	PromotionSpec,
	PromotionStep,
	ShardDetail,
	ShardSummary
} from '$lib/types';
import { clockTime } from '$lib/pollErrors';

// Items whose observedAt is older than this are shown as stale: 3 coordinator poll intervals.
export const STALE_AFTER_MS = 3 * 60 * 1000;

// Polling cadence for fleet objects: fast while anything is promoting, slow otherwise.
export const ACTIVE_POLL_MS = 5_000;
export const IDLE_POLL_MS = 30_000;

export const RELEASE_CONTROLS_TOOLTIP = 'Enable Release controls in Preferences';

const BACKEND_PART_NAMES: (keyof BackendParts)[] = ['platform', 'portal', 'content', 'config'];

export interface BackendVersion {
	raw: string;
	// When the part's artifact was built, parsed from the `<yyyy-mm-dd-hh-mm-ss>` segment; null if
	// unparseable. Not a deploy time: a deploy leaves it unchanged, and nothing reports one per part.
	builtAt: Nullable<Date>;
	release: string;
	branch: string;
	sha: string;
}

export interface BackendPart {
	name: keyof BackendParts;
	version: Nullable<BackendVersion>;
	raw: string;
}

// The build time segment carries no zone; it is UTC.
const parseBuildTime = (value: string): Nullable<Date> => {
	const m = /^(\d{4})-(\d{2})-(\d{2})-(\d{2})-(\d{2})-(\d{2})$/.exec(value);
	if (!m) return null;
	const [, y, mo, d, h, mi, s] = m.map(Number);
	return new Date(Date.UTC(y, mo - 1, d, h, mi, s));
};

/**
 * Parses `<build time>_<release>_<branch>_<sha>`, the same way as fleet-api's `BackendVersion`:
 * the first two segments never contain `_`, the SHA is the last segment, and the branch is
 * everything between, so it may contain `_`.
 */
export const parseBackendVersion = (raw: string): Nullable<BackendVersion> => {
	const first = raw.indexOf('_');
	if (first < 0) return null;
	const second = raw.indexOf('_', first + 1);
	if (second < 0) return null;
	const last = raw.lastIndexOf('_');
	if (last <= second) return null;
	const [built, release, branch, sha] = [
		raw.slice(0, first),
		raw.slice(first + 1, second),
		raw.slice(second + 1, last),
		raw.slice(last + 1)
	];
	if (!built || !release || !branch || !sha) return null;
	return { raw, builtAt: parseBuildTime(built), release, branch, sha };
};

/** The parts that are set, in a fixed order. */
export const backendParts = (parts: Nullable<BackendParts> | undefined): BackendPart[] =>
	BACKEND_PART_NAMES.flatMap((name) => {
		const raw = parts?.[name];
		return raw ? [{ name, raw, version: parseBackendVersion(raw) }] : [];
	});

/**
 * The part with the newest build. Not necessarily the last one deployed: nothing reports when a
 * part was deployed, and a deploy can install an older build.
 */
export const newestBuiltPart = (parts: BackendPart[]): Nullable<BackendPart> => {
	let best: Nullable<BackendPart> = null;
	for (const part of parts) {
		const at = part.version?.builtAt?.getTime() ?? -Infinity;
		const bestAt = best?.version?.builtAt?.getTime() ?? -Infinity;
		if (!best || at > bestAt) best = part;
	}
	return best;
};

const UNITS_MS: Record<string, number> = {
	ns: 1e-6,
	us: 1e-3,
	µs: 1e-3,
	ms: 1,
	s: 1000,
	m: 60_000,
	h: 3_600_000
};

/** A Go-style duration ("30m", "1h30m", "2.5s") in milliseconds; null if it doesn't parse. */
export const parseDuration = (text: Nullable<string> | undefined): Nullable<number> => {
	if (!text) return null;
	const part = /([0-9]+(?:\.[0-9]+)?)(ns|us|µs|ms|s|m|h)/gy;
	let total = 0;
	let consumed = 0;
	for (;;) {
		const match = part.exec(text);
		if (!match) break;
		total += Number(match[1]) * UNITS_MS[match[2]];
		consumed = part.lastIndex;
	}
	return consumed === text.length && consumed > 0 ? total : null;
};

// The local backend fills every unset duration from the coordinator's defaults
// (fleet_api::fleet_defaults), so these read the values in force; 0 only without a fleets section.
export const retireAfterIdleMs = (fleets: Nullable<FleetsConfig> | undefined): number =>
	parseDuration(fleets?.retireAfterIdle) ?? 0;

export const warmTtlMs = (fleets: Nullable<FleetsConfig> | undefined): number =>
	parseDuration(fleets?.warmTtl) ?? 0;

export const gateTimeoutMs = (fleets: Nullable<FleetsConfig> | undefined): number =>
	parseDuration(fleets?.gate.timeout) ?? 0;

export const gateStableForMs = (fleets: Nullable<FleetsConfig> | undefined): number =>
	parseDuration(fleets?.gate.stableFor) ?? 0;

/** "1 h 5 m", "12 m", "45 s": a span of time, for countdowns. */
export const formatSpan = (ms: number): string => {
	const seconds = Math.max(0, Math.round(ms / 1000));
	if (seconds < 60) return `${seconds} s`;
	const minutes = Math.floor(seconds / 60);
	if (minutes < 60) return `${minutes} m`;
	const hours = Math.floor(minutes / 60);
	return minutes % 60 ? `${hours} h ${minutes % 60} m` : `${hours} h`;
};

/**
 * A shard's Fleets config in one line: "Clusters: <cluster> · at most 50 servers per Fleet · gate 1
 * Ready for 2 m, timeout 20 m".
 */
export const fleetsConfigLine = (fleets: Nullable<FleetsConfig> | undefined): string =>
	[
		`Clusters: ${(fleets?.clusters ?? []).map((c) => c.name).join(', ') || 'none'}`,
		fleets?.limits.maxReplicasPerFleet
			? `at most ${fleets.limits.maxReplicasPerFleet} servers per Fleet`
			: '',
		fleets
			? `gate ${fleets.gate.minReady ?? 1} Ready for ${formatSpan(
					gateStableForMs(fleets)
			  )}, timeout ${formatSpan(gateTimeoutMs(fleets))}`
			: ''
	]
		.filter(Boolean)
		.join(' · ');

/** Compact age: "now", "12 m", "9 h", "3 d". */
export const formatAge = (from: Nullable<string | Date> | undefined, now: number): string => {
	if (!from) return '';
	const at = typeof from === 'string' ? Date.parse(from) : from.getTime();
	if (Number.isNaN(at)) return '';
	const minutes = Math.floor(Math.max(0, now - at) / 60_000);
	if (minutes < 1) return 'now';
	if (minutes < 60) return `${minutes} m`;
	const hours = Math.floor(minutes / 60);
	if (hours < 48) return `${hours} h`;
	return `${Math.floor(hours / 24)} d`;
};

/** An age followed by "ago": "just now", "12 m ago"; empty when there is no time. */
export const formatAgo = (from: Nullable<string | Date> | undefined, now: number): string => {
	const age = formatAge(from, now);
	if (!age) return '';
	return age === 'now' ? 'just now' : `${age} ago`;
};

const MONTHS = ['Jan', 'Feb', 'Mar', 'Apr', 'May', 'Jun', 'Jul', 'Aug', 'Sep', 'Oct', 'Nov', 'Dec'];

/** A time in the local zone, by its day relative to now's: "19:19", "tomorrow 19:19", "7 Nov 19:19". */
export const localTime = (at: Nullable<string | Date> | undefined, now: number): string => {
	if (!at) return '';
	const date = typeof at === 'string' ? new Date(at) : at;
	if (Number.isNaN(date.getTime())) return '';
	const dayOf = (d: Date) => new Date(d.getFullYear(), d.getMonth(), d.getDate()).getTime();
	const days = Math.round((dayOf(date) - dayOf(new Date(now))) / 86_400_000);
	const time = clockTime(date);
	if (days === 0) return time;
	if (days === 1) return `tomorrow ${time}`;
	if (days === -1) return `yesterday ${time}`;
	return `${date.getDate()} ${MONTHS[date.getMonth()]} ${time}`;
};

/** Elapsed time for a running thing: "45 s", "3 m 10 s", "1 h 5 m". */
export const formatElapsed = (from: Nullable<string> | undefined, now: number): string => {
	if (!from) return '';
	const at = Date.parse(from);
	if (Number.isNaN(at)) return '';
	const seconds = Math.floor(Math.max(0, now - at) / 1000);
	if (seconds < 60) return `${seconds} s`;
	const minutes = Math.floor(seconds / 60);
	if (minutes < 60) return `${minutes} m ${seconds % 60} s`;
	return `${Math.floor(minutes / 60)} h ${minutes % 60} m`;
};

export const shortSha = (sha: Nullable<string> | undefined, length = 8): string =>
	(sha ?? '').slice(0, length);

/** "content abc12345 · built 9 h ago" */
export const formatBackendPart = (part: BackendPart, now: number): string => {
	if (!part.version) return `${part.name} ${part.raw}`;
	const ago = formatAgo(part.version.builtAt, now);
	return `${part.name} ${shortSha(part.version.sha)}${ago ? ` · built ${ago}` : ''}`;
};

/**
 * Whether an item's data is stale. `observedAt` is the last *successful* observation (a failed one
 * leaves it and sets `error`), so its age is how old the data is.
 */
export const isStale = (observedAt: Nullable<string> | undefined, now: number): boolean => {
	if (!observedAt) return true;
	const at = Date.parse(observedAt);
	return Number.isNaN(at) || now - at > STALE_AFTER_MS;
};

const lastSuccess = (observedAt: Nullable<string> | undefined, now: number): string =>
	observedAt
		? `last successful observation ${formatAgo(observedAt, now)}`
		: 'never observed successfully';

/** Why an observed item gets a warning icon, or null when it doesn't. */
export const observationWarning = (
	item: { error?: string; observedAt?: Nullable<string> } | null | undefined,
	now: number
): Nullable<string> => {
	if (!item) return null;
	if (item.error) {
		return isStale(item.observedAt, now)
			? `${item.error}; ${lastSuccess(item.observedAt, now)}`
			: item.error;
	}
	if (isStale(item.observedAt, now)) {
		const text = lastSuccess(item.observedAt, now);
		return text.charAt(0).toUpperCase() + text.slice(1);
	}
	return null;
};

/**
 * The backend's warning, noting an accepted promotion (Pending or Running) that deploys the backend.
 * It says only that the promotion is in progress, not that it caused the warning.
 */
export const backendWarning = (
	backend: Nullable<BackendStatus> | undefined,
	promotions: Promotion[],
	shard: string,
	now: number
): Nullable<string> => {
	const warning = observationWarning(backend, now);
	if (!warning) return null;
	const deploying = promotions.find(
		(p) =>
			p.spec.shard === shard &&
			p.spec.deployBackend &&
			(p.status?.phase === 'Pending' || p.status?.phase === 'Running')
	);
	if (!deploying) return warning;
	return `${warning}; a promotion that deploys the backend is in progress (${
		deploying.metadata.name ?? 'unnamed'
	})`;
};

/**
 * What a channel's `since` means, for a hover. Launcher: when its pointer was written (the object's
 * S3 LastModified). Steam: when the coordinator first saw the build on the branch, up to a poll late,
 * or its first observation for a build already live then; no Steam API says when a build went live.
 * With `source: promotion`, when a Promotion's success was recorded.
 */
export const channelSinceText = (channel: ChannelStatus, now: number): string => {
	const ago = formatAgo(channel.since, now);
	if (!ago) return '';
	if (channel.source === 'promotion') return `recorded from a promotion ${ago}`;
	return channel.kind === 'launcher'
		? `pointer changed ${ago}`
		: `build first seen on the branch ${ago}`;
};

export const channelKey = (channel: ChannelRef): string =>
	`${channel.kind}/${channel.kind === 'steam' ? channel.branch : channel.metadataPath}`;

export const channelName = (channel: ChannelRef): string =>
	(channel.kind === 'steam' ? channel.branch : channel.metadataPath) ?? '';

/** "steam: test" or "launcher: stable-1.0" */
export const channelLabel = (channel: ChannelRef): string =>
	`${channel.kind}: ${channelName(channel)}`;

/** What a Steam channel line shows when it has no SHA: "build 12345678" or "no build". */
export const steamBuildText = (channel: ChannelStatus): string =>
	channel.buildId ? `build ${channel.buildId}` : 'no build';

export const promotionPhase = (p: Promotion): Nullable<PromotionPhase> => p.status?.phase ?? null;

/** Pending, Running, or not picked up yet (no status): the channel is busy. */
export const isActivePromotion = (p: Promotion): boolean => {
	const phase = promotionPhase(p);
	return phase === null || phase === 'Pending' || phase === 'Running';
};

// fleet_api::reasons::RETRYING: not picked up yet because the build manifest can't be read.
export const REASON_RETRYING = 'Retrying';
// fleet_api::reasons::DELETED: cancelled because someone deleted the Promotion by hand.
export const REASON_DELETED = 'Deleted';

// fleet_api::ANNOTATION_CHANGED_BY: who created the promotion.
const ANNOTATION_CHANGED_BY = 'fleet.believer.dev/changed-by';
// fleet_api::ANNOTATION_CANCEL_REQUESTED_AT and _BY. The time's presence is the request.
const ANNOTATION_CANCEL_REQUESTED_AT = 'fleet.believer.dev/cancel-requested-at';
const ANNOTATION_CANCEL_REQUESTED_BY = 'fleet.believer.dev/cancel-requested-by';

/** The request to cancel, if someone has made one (fleet_api's Promotion::cancel_request). */
export const promotionCancel = (p: Promotion): Nullable<PromotionCancel> => {
	const annotations: Partial<Record<string, string>> = p.metadata.annotations ?? {};
	const requestedAt = annotations[ANNOTATION_CANCEL_REQUESTED_AT];
	if (requestedAt === undefined) return null;
	const requestedBy = annotations[ANNOTATION_CANCEL_REQUESTED_BY]?.trim();
	return { requestedAt, requestedBy: requestedBy || null };
};

/** Cancel requested, and the coordinator hasn't finished the promotion yet. */
export const isCancelling = (p: Promotion): boolean =>
	isActivePromotion(p) && promotionCancel(p) !== null;

/**
 * The phase as shown. A promotion that finished despite a cancel shows its real phase; the
 * coordinator's message says the cancel came too late.
 */
export const promotionPhaseLabel = (p: Promotion): string => {
	if (isCancelling(p)) return 'Cancelling…';
	const phase = promotionPhase(p);
	if (phase === 'Cancelled') {
		const cancel = promotionCancel(p);
		if (cancel) return cancel.requestedBy ? `Cancelled by ${cancel.requestedBy}` : 'Cancelled';
		if (p.status?.reason === REASON_DELETED) return 'Cancelled (deleted)';
	}
	return phase ?? 'Submitted';
};

/** Why a submitted promotion is still waiting, when the coordinator is retrying it. */
export const retryingMessage = (p: Promotion): Nullable<string> =>
	promotionPhase(p) === null && p.status?.reason === REASON_RETRYING
		? p.status.message || 'Retrying'
		: null;

export const promotionChangedBy = (p: Promotion): string =>
	p.metadata.annotations?.[ANNOTATION_CHANGED_BY] ?? 'unknown';

export const promotionNote = (p: Promotion): string =>
	p.metadata.annotations?.['fleet.believer.dev/note'] ?? '';

// fleet_api's step names and the step phases the coordinator writes besides Argo's.
export const STEP_DEPLOY_BACKEND = 'deploy-backend';
export const STEP_WARM_FLEETS = 'warm-fleets';
export const STEP_PUBLISH_CLIENT = 'publish-client';
export const STEP_FLIP_CHANNEL = 'flip-channel';
export const STEP_MARK_LIVE = 'mark-live';
export const STEP_PENDING = 'Pending';
export const STEP_SKIPPED = 'Skipped';

/**
 * The workflow to link from a promotion's summary: the running step's, else the last step that had
 * one. A promotion run as one workflow has a single `promote` step.
 */
export const promotionWorkflow = (p: Promotion): Nullable<string> => {
	const steps = p.status?.steps ?? [];
	const current = steps.find((s) => s.name === p.status?.currentStep);
	if (current?.workflow) return current.workflow;
	const started = steps.filter((s) => !!s.workflow && s.phase !== STEP_PENDING);
	return started[started.length - 1]?.workflow || null;
};

const STEP_LABELS: Record<string, string> = {
	[STEP_DEPLOY_BACKEND]: 'Deploy backend',
	[STEP_WARM_FLEETS]: 'Warm fleets',
	[STEP_PUBLISH_CLIENT]: 'Publish client',
	[STEP_FLIP_CHANNEL]: 'Flip channel',
	[STEP_MARK_LIVE]: 'Mark live',
	promote: 'Promote'
};

export const stepLabel = (name: string): string => STEP_LABELS[name] ?? name;

/** Where a promotion is, in a few words: "Cancelling…", the running step, or its phase. */
export const promotionProgressText = (p: Promotion): string => {
	if (isCancelling(p) || !p.status?.currentStep) return promotionPhaseLabel(p);
	return stepLabel(p.status.currentStep);
};

/** "deploying backend" etc., for the tray and the Shards list's Next column. */
export const currentStepText = (p: Promotion): string => {
	const step = p.status?.currentStep;
	if (!step) return promotionPhaseLabel(p).toLowerCase();
	return stepLabel(step).toLowerCase();
};

export type StepState = 'pending' | 'running' | 'succeeded' | 'failed' | 'skipped';

export interface TimelineStep {
	name: string;
	label: string;
	state: StepState;
	phase: string;
	message: string;
	workflow: Nullable<string>;
	fleetRelease: Nullable<string>;
	startedAt: Nullable<string>;
	finishedAt: Nullable<string>;
}

const stepState = (step: PromotionStep): StepState => {
	switch (step.phase) {
		case 'Succeeded':
			return 'succeeded';
		case 'Failed':
		case 'Error':
			return 'failed';
		case STEP_SKIPPED:
			return 'skipped';
		case 'Running':
			return 'running';
		default:
			return 'pending';
	}
};

/** The promotion's steps as listed (every step from acceptance on, skipped ones included). */
export const promotionTimeline = (p: Promotion): TimelineStep[] =>
	(p.status?.steps ?? []).map((step) => ({
		name: step.name,
		label: stepLabel(step.name),
		state: stepState(step),
		phase: step.phase || STEP_PENDING,
		message: step.message,
		workflow: step.workflow || null,
		fleetRelease: step.fleetRelease ?? null,
		startedAt: step.startedAt ?? null,
		finishedAt: step.finishedAt ?? null
	}));

/**
 * What cancelling does now, for the confirmation: it depends on the running step (contract: cancel
 * rules per step). "Already finished" applies per step, so a finished flip stands.
 */
export const cancelEffect = (p: Promotion): string => {
	if (promotionPhase(p) === null || promotionPhase(p) === 'Pending' || !p.status?.currentStep) {
		return 'Nothing has run yet, so nothing is submitted and the promotion ends Cancelled.';
	}
	switch (p.status.currentStep) {
		case STEP_DEPLOY_BACKEND:
			return 'The backend deploy is running. If it has reached the deploy itself, it finishes first (it can’t be stopped), then the promotion ends Cancelled; otherwise its workflow is stopped.';
		case STEP_WARM_FLEETS:
			return 'Nothing is stopped: the release keeps warming and retires on its own unless something uses it. No client is published and the channel is unchanged.';
		case STEP_PUBLISH_CLIENT:
			return 'The client upload is stopped. Nothing is live on the channel yet.';
		case STEP_FLIP_CHANNEL:
		case STEP_MARK_LIVE:
			return 'The channel flip may already have happened. A flip can’t be undone: if it completed, the promotion ends Succeeded.';
		default:
			return 'The running step’s workflow is stopped.';
	}
};

const CHANGE_LABELS: Record<string, string> = {
	ClientUploaded: 'Client uploaded',
	ChannelFlipped: 'Channel flipped'
};

// FleetReleaseWarming's detail: "<release>[, created afresh in place of the retired one], retires on
// its TTL[ at <time>] unless used".
const WARMED_DETAIL =
	/^([^,]+)(, created afresh in place of the retired one)?, retires on its TTL(?: at (\S+))? unless used$/;

const changeText = (c: PromotionChange, now: number): string => {
	if (c.kind === 'FleetReleaseWarming') {
		const warmed = WARMED_DETAIL.exec(c.detail);
		if (!warmed) return `Release warmed: ${c.detail}`;
		const [, name, afresh, until] = warmed;
		const expires = until ? `expires ${localTime(until, now)}` : 'expires on its TTL';
		const replaced = afresh ? ', in place of the retired one' : '';
		return `Release ${name} warmed${replaced} (${expires} unless used)`;
	}
	if (c.kind === 'FleetReleaseLive') return `Release ${c.detail} went live`;
	return `${CHANGE_LABELS[c.kind] ?? c.kind}: ${c.detail}`;
};

/**
 * What the promotion changed, in order, as past events: "Client uploaded: …", "Release … went live".
 * The backend deploy is left out: the deploy-backend step's row shows it, with the part changes.
 */
export const promotionChanges = (p: Promotion, now: number): string[] =>
	(p.status?.changes ?? [])
		.filter((c) => c.kind !== 'BackendDeployed')
		.map((c) => changeText(c, now));

/** Whether a promotion to this shard waits for the warm gate, and why or why not. */
export const gatePlan = (
	shard: Nullable<ShardSummary> | undefined,
	skip: boolean
): { applies: boolean; text: string } => {
	if (!shard?.config?.fleets)
		return { applies: false, text: 'This shard has no Fleets: no warm gate.' };
	if (!shard.spec?.fleetsEnabled)
		return { applies: false, text: 'Fleets are off on this shard: no warm gate.' };
	const timeout = formatSpan(gateTimeoutMs(shard.config.fleets));
	if (skip) {
		return {
			applies: false,
			text: 'The warm gate is skipped: the release warms alongside, and the client publishes without waiting.'
		};
	}
	return {
		applies: true,
		text: `Waits for the build's Fleets to pass the warm gate (up to ${timeout}) before publishing the client.`
	};
};

export interface MapPreflight {
	mapId: string;
	ok: boolean;
	text: string;
}

const MAP_CHECK_TEXTS: Record<string, string> = {
	OK: 'the build has its level',
	NotInBuild: 'not in the build’s maps: the release fails',
	NotCooked: 'the build doesn’t contain its level: the release fails'
};

/**
 * The map check per fleet map, as far as Friendshipper can tell: if a release for this build
 * already exists, its check; otherwise whether the build lists the map, by its manifest's map IDs
 * (`manifestMapIds`) or else its release's. An empty list says nothing: older manifests have none.
 * The coordinator checks the build manifest itself when it warms the release.
 */
export const mapPreflight = (
	detail: Nullable<ShardDetail> | undefined,
	version: string,
	manifestMapIds: string[] = []
): MapPreflight[] => {
	if (!detail?.config?.fleets || !detail.spec?.fleetsEnabled) return [];
	const release = detail.releases.find((r) => r.spec.version === version);
	const buildMapIds = manifestMapIds.length ? manifestMapIds : release?.status?.buildMapIds;
	return (detail.spec.maps ?? []).map((mapId) => {
		const checked = release?.status?.maps.find((m) => m.mapId === mapId);
		if (checked) {
			return {
				mapId,
				ok: checked.check === 'OK',
				text: MAP_CHECK_TEXTS[checked.check] ?? checked.check
			};
		}
		if (buildMapIds?.length && !buildMapIds.includes(mapId)) {
			return { mapId, ok: false, text: MAP_CHECK_TEXTS.NotInBuild };
		}
		if (buildMapIds?.includes(mapId)) {
			return { mapId, ok: true, text: 'in the build’s maps: its level is checked when warming' };
		}
		return { mapId, ok: true, text: 'checked against the build’s manifest when warming' };
	});
};

/**
 * When the Promotion was submitted (its creationTimestamp). `status.startedAt` is later: when the
 * coordinator accepted it, unset while it is still being retried.
 */
export const promotionSubmittedAt = (p: Promotion): Nullable<string> =>
	p.metadata.creationTimestamp ?? null;

/** The running promotion holding a shard's channel, if any. */
export const busyPromotion = (
	promotions: Promotion[],
	shard: string,
	channel: ChannelRef
): Nullable<Promotion> =>
	promotions.find(
		(p) =>
			isActivePromotion(p) &&
			p.spec.shard === shard &&
			channelKey(p.spec.channel) === channelKey(channel)
	) ?? null;

export interface PreviewRow {
	label: string;
	// A tooltip on the label, for what the cells can't say.
	labelTitle?: string;
	current: string;
	requested: string;
}

export interface PromotionPreview {
	rows: PreviewRow[];
	// Why the promotion can't go ahead on this data; empty when it can.
	blocked: string;
}

const yesNo = (value: boolean): string => (value ? 'Yes' : 'No');

/**
 * The promote confirmation: the channel's current state next to the requested one, from a fresh
 * fetch of the shard. `detail` is null when the shard is gone.
 */
export const promotionPreview = (
	detail: Nullable<ShardDetail>,
	spec: PromotionSpec,
	now: number
): PromotionPreview => {
	const key = channelKey(spec.channel);
	const configured = detail?.config?.channels.find((c) => channelKey(c) === key);
	const observed = detail?.status?.channels.find((c) => c.key === key);
	const isSteam = spec.channel.kind === 'steam';

	let current = 'nothing known';
	if (observed?.version) {
		const since = channelSinceText(observed, now);
		current = since ? `${shortSha(observed.version)} · ${since}` : shortSha(observed.version);
	} else if (observed?.buildId) {
		current = `Steam build ${observed.buildId}`;
	}

	const rows: PreviewRow[] = [
		{ label: 'Version', current, requested: shortSha(spec.version) },
		{
			label: 'Game config',
			// Only Steam channels report one; a launcher channel shows its default for promotions.
			labelTitle: isSteam
				? undefined
				: "Launcher channels don't report their game config: Current is the channel's default for promotions.",
			current:
				observed?.gameConfig ?? (isSteam ? 'unknown' : configured?.gameConfig ?? 'channel default'),
			requested: spec.gameConfig ?? configured?.gameConfig ?? 'channel default'
		},
		{ label: 'Deploy backend', current: '', requested: yesNo(spec.deployBackend) }
	];
	if (isSteam) {
		// Unset leaves it to the promotion workflow's default, which signs.
		const sign = spec.sign === undefined ? 'Yes (default)' : yesNo(spec.sign);
		rows.push({ label: 'Sign', current: '', requested: sign });
	}
	if (detail?.config?.fleets) {
		const gate = gatePlan(detail, !!spec.skipFleetGate);
		let requested = gate.applies ? 'Waits for Fleets' : 'No gate';
		if (spec.skipFleetGate) requested = `Skipped: ${spec.skipFleetGate.reason.trim()}`;
		rows.push({ label: 'Warm gate', current: '', requested });
	}

	let blocked = '';
	const busy = detail ? busyPromotion(detail.promotions, spec.shard, spec.channel) : null;
	if (!detail) {
		blocked = `Shard ${spec.shard} no longer exists.`;
	} else if (!detail.config) {
		blocked = `Shard ${spec.shard} no longer has a ShardConfig.`;
	} else if (!configured) {
		blocked = `${channelLabel(spec.channel)} is no longer configured on ${spec.shard}.`;
	} else if (spec.deployBackend && !detail.config.promotion.allowDeployBackend) {
		blocked = `${spec.shard} no longer allows deploying the backend.`;
	} else if (spec.skipFleetGate && !spec.skipFleetGate.reason.trim()) {
		blocked = 'Skipping the warm gate needs a reason.';
	} else if (busy) {
		blocked = `${channelLabel(spec.channel)} is busy: ${
			busy.metadata.name ?? 'a promotion'
		} is ${promotionPhaseLabel(busy).replace(/…$/, '').toLowerCase()}.`;
	}
	return { rows, blocked };
};

/** "N promotion(s) running" */
export const runningPromotionsText = (count: number): string =>
	`${count} promotion${count === 1 ? '' : 's'} running`;

/**
 * Active Builds rows from shard status, for the build list's "promoted to" badges when the
 * coordinator switch is on. One row per channel that has a version.
 */
export const activeBuildsFromShards = (shards: ShardSummary[]): ActiveBuild[] =>
	shards.flatMap((shard) =>
		(shard.status?.channels ?? []).map((channel) => ({
			displayName: `${shard.displayName} (${channelLabel(channel)})`,
			steamBranch: channel.branch,
			sha: channel.version || undefined,
			// Not channel.since: that is when the coordinator first saw the version, not a deploy time.
			deployedAt: undefined,
			status: channel.version ? ('resolved' as const) : ('tbd' as const),
			error: channel.error || undefined
		}))
	);

export interface PromotionToast {
	kind: 'success' | 'error';
	message: string;
	shard: string;
}

const TERMINAL_TOAST_PHASES: PromotionPhase[] = ['Succeeded', 'Failed', 'Rejected', 'Cancelled'];

const describe = (p: Promotion): string =>
	`${p.spec.shard} ${channelLabel(p.spec.channel)} → ${shortSha(p.spec.version)}`;

const toastFor = (p: Promotion, phase: PromotionPhase): PromotionToast => {
	if (phase === 'Succeeded') {
		return { kind: 'success', message: `Promotion succeeded: ${describe(p)}`, shard: p.spec.shard };
	}
	if (phase === 'Cancelled') {
		// The coordinator's summary says what changed and how it's left, then what didn't.
		const summary = p.status?.message ? `: ${p.status.message}` : '';
		return {
			kind: 'error',
			message: `${promotionPhaseLabel(p)}: ${describe(p)}${summary}`,
			shard: p.spec.shard
		};
	}
	const why = p.status?.message || p.status?.reason || 'no reason given';
	return {
		kind: 'error',
		message: `Promotion ${phase.toLowerCase()}: ${describe(p)}: ${why}`,
		shard: p.spec.shard
	};
};

/**
 * Whether this user hears how a promotion finished: they created it, or they asked to cancel it
 * (which covers a result that came despite the cancel). `localName` is the name Friendshipper
 * writes in those annotations; it is compared exactly, and without one nobody is anyone's.
 */
export const isPromotionMine = (p: Promotion, localName: Nullable<string>): boolean => {
	if (!localName) return false;
	const annotations: Partial<Record<string, string>> = p.metadata.annotations ?? {};
	if (annotations[ANNOTATION_CHANGED_BY] === localName) return true;
	return (
		annotations[ANNOTATION_CANCEL_REQUESTED_AT] !== undefined &&
		annotations[ANNOTATION_CANCEL_REQUESTED_BY] === localName
	);
};

/**
 * The bottom-bar tray of everyone's running promotions: for release managers (Release controls on)
 * with the switch on. Everyone else sees running promotions on the Shards pages.
 */
export const showPromotionTray = (switchOn: boolean, releaseControlsEnabled: boolean): boolean =>
	switchOn && releaseControlsEnabled;

/**
 * Tracks promotion phases between polls and reports the transitions worth a toast. Only
 * promotions that changed while the app was open toast. The first update is the baseline and
 * never toasts; after it, a known promotion toasts when its phase changes, and a name first seen
 * later counts as started while the app was open, so it toasts even if first seen finished. No
 * server timestamps are compared with the local clock.
 */
export class PromotionTransitions {
	private phases: Nullable<Map<string, Nullable<PromotionPhase>>> = null;

	/** Toasts only promotions that are `localName`'s (see isPromotionMine); all are tracked. */
	update(promotions: Promotion[], localName: Nullable<string>): PromotionToast[] {
		const toasts: PromotionToast[] = [];
		const previous = this.phases;
		const next = new Map<string, Nullable<PromotionPhase>>();
		for (const p of promotions) {
			const { name } = p.metadata;
			const phase = promotionPhase(p);
			if (name) {
				next.set(name, phase);
				const changed = previous !== null && (!previous.has(name) || previous.get(name) !== phase);
				const finished = phase !== null && TERMINAL_TOAST_PHASES.includes(phase);
				if (phase && finished && changed && isPromotionMine(p, localName)) {
					toasts.push(toastFor(p, phase));
				}
			}
		}
		// Keep names that dropped out of a poll (e.g. pruned), so one coming back isn't new.
		previous?.forEach((phase, name) => {
			if (!next.has(name)) next.set(name, phase);
		});
		this.phases = next;
		return toasts;
	}
}

/** Gating for write controls: visible but disabled until Release controls is on. */
export const writeControl = (
	releaseControlsEnabled: boolean | undefined
): { disabled: boolean; tooltip: string } =>
	releaseControlsEnabled
		? { disabled: false, tooltip: '' }
		: { disabled: true, tooltip: RELEASE_CONTROLS_TOOLTIP };
