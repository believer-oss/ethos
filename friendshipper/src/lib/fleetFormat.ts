// Pure helpers for showing fleet objects (shards, channels, promotions). No Tauri imports, so
// they can be tested on their own.

import type {
	ActiveBuild,
	BackendParts,
	BackendStatus,
	ChannelRef,
	ChannelStatus,
	Nullable,
	Promotion,
	PromotionCancel,
	PromotionPhase,
	PromotionSpec,
	ShardDetail,
	ShardSummary
} from '$lib/types';

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

export const shortSha = (sha: Nullable<string> | undefined, length = 7): string =>
	(sha ?? '').slice(0, length);

/** "content abc1234 · built 9 h ago" */
export const formatBackendPart = (part: BackendPart, now: number): string => {
	if (!part.version) return `${part.name} ${part.raw}`;
	const age = formatAge(part.version.builtAt, now);
	return `${part.name} ${shortSha(part.version.sha)}${age ? ` · built ${age} ago` : ''}`;
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
		? `last successful observation ${formatAge(observedAt, now)} ago`
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
	const age = formatAge(channel.since, now);
	if (!age) return '';
	if (channel.source === 'promotion') return `recorded from a promotion ${age} ago`;
	return channel.kind === 'launcher'
		? `pointer changed ${age} ago`
		: `build first seen on the branch ${age} ago`;
};

export const channelKey = (channel: ChannelRef): string =>
	`${channel.kind}/${channel.kind === 'steam' ? channel.branch : channel.metadataPath}`;

export const channelName = (channel: ChannelRef): string =>
	(channel.kind === 'steam' ? channel.branch : channel.metadataPath) ?? '';

/** "steam: test" or "launcher: stable-1.0" */
export const channelLabel = (channel: ChannelRef): string =>
	`${channel.kind}: ${channelName(channel)}`;

/** What a Steam channel line shows: "test: abc1234", "build 12345678" or "no build". */
export const steamChannelText = (channel: ChannelStatus): string => {
	const name = channel.branch ?? '';
	if (channel.version) return `${name}: ${shortSha(channel.version)}`;
	if (channel.buildId) return `${name}: build ${channel.buildId}`;
	return `${name}: no build`;
};

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

export const promotionWorkflow = (p: Promotion): Nullable<string> =>
	p.status?.steps.find((s) => !!s.workflow)?.workflow || null;

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
			// Only Steam channels report one.
			current: observed?.gameConfig ?? (isSteam ? 'unknown' : 'not reported'),
			requested: spec.gameConfig ?? configured?.gameConfig ?? 'channel default'
		},
		{ label: 'Deploy backend', current: '', requested: yesNo(spec.deployBackend) }
	];
	if (isSteam) {
		// Unset leaves it to the promotion workflow's default, which signs.
		const sign = spec.sign === undefined ? 'Yes (default)' : yesNo(spec.sign);
		rows.push({ label: 'Sign', current: '', requested: sign });
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
		return {
			kind: 'error',
			message: `${promotionPhaseLabel(p)}: ${describe(p)}`,
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
