// Pure helpers for the shard page's layout: the header's backend summary, a "Now" card per channel,
// the shard-level problems shown once above them, the collapsed Fleet settings line, and the
// Shards list's attention marker. No Tauri imports, so they can be tested on their own.

import type {
	ChannelRef,
	ChannelStatus,
	FleetRelease,
	Nullable,
	Promotion,
	ShardDetail,
	ShardSummary
} from '$lib/types';
import {
	busyPromotion,
	channelKey,
	formatSpan,
	formatAgo,
	gateStableForMs,
	gateTimeoutMs,
	promotionChangedBy,
	promotionPhase,
	shortSha,
	type BackendPart
} from '$lib/fleetFormat';
import {
	gateCannotPassText,
	isUnreachable,
	listText,
	releasePhase,
	releaseStatus,
	type StatusTone
} from '$lib/fleetReleases';
import { buildMapChoices, policyText } from '$lib/fleetSizing';

/** "platform abc12345 · content def67890": the header's one-line backend summary. */
export const backendSummary = (parts: BackendPart[]): string =>
	parts.map((p) => `${p.name} ${p.version ? shortSha(p.version.sha) : p.raw}`).join(' · ');

/**
 * When a part was deployed: only where a retained promotion recorded deploying it (its plan took
 * the part to this version, and the backend deploy happened). Null otherwise: nothing else records
 * deploys, and the backend's launch time also moves on a crash or reschedule.
 */
export const partDeployedAt = (promotions: Promotion[], part: BackendPart): Nullable<string> => {
	const times = promotions.flatMap((p) => {
		const moved = p.status?.plan?.backend?.parts.some(
			(c) => c.name === part.name && c.to === part.raw && c.from !== c.to
		);
		const at = p.status?.changes?.find((c) => c.kind === 'BackendDeployed')?.at;
		return moved && at ? [at] : [];
	});
	return times.sort((a, b) => Date.parse(b) - Date.parse(a))[0] ?? null;
};

/** "built 9 h ago · deployed 2 h ago", the deploy only where recorded. */
export const partTimes = (part: BackendPart, promotions: Promotion[], now: number): string => {
	const built = part.version?.builtAt ? `built ${formatAgo(part.version.builtAt, now)}` : '';
	const at = partDeployedAt(promotions, part);
	return [built, at ? `deployed ${formatAgo(at, now)}` : ''].filter(Boolean).join(' · ');
};

/** The channel's build before its current one: the latest Succeeded promotion's previousVersion. */
export const previousVersion = (
	promotions: Promotion[],
	shard: string,
	channel: ChannelRef,
	current: Nullable<string> | undefined
): Nullable<string> => {
	const key = channelKey(channel);
	const latest = promotions
		.filter(
			(p) =>
				p.spec.shard === shard &&
				channelKey(p.spec.channel) === key &&
				promotionPhase(p) === 'Succeeded'
		)
		.sort(
			(a, b) =>
				(Date.parse(b.status?.finishedAt ?? '') || 0) -
				(Date.parse(a.status?.finishedAt ?? '') || 0)
		)
		.at(0);
	if (!latest || latest.spec.version !== current) return null;
	const previous = latest.status?.previousVersion;
	return previous && previous !== current ? previous : null;
};

/** The release for a build on this shard, not counting one being deleted. */
export const releaseFor = (
	releases: FleetRelease[],
	version: Nullable<string> | undefined
): Nullable<FleetRelease> =>
	(version && releases.find((r) => r.spec.version === version && !r.metadata.deletionTimestamp)) ||
	null;

export interface FleetsHealth {
	tone: StatusTone;
	text: string;
}

/**
 * Fleets health for the build a channel serves; null when the shard has no Fleets. A build with no
 * Fleets gets a short line: the shard banner says it once for every channel, and Warm again is on
 * the release card.
 */
export const channelFleets = (
	detail: ShardDetail,
	version: Nullable<string> | undefined,
	now: number
): Nullable<FleetsHealth> => {
	const fleets = detail.config?.fleets;
	if (!fleets || !version) return null;
	if (!detail.spec?.fleetsEnabled) return { tone: 'info', text: 'Fleets off.' };
	const release = releaseFor(detail.releases, version);
	if (!release) {
		return { tone: 'warn', text: 'No Fleets: no release for this build · matches start on demand' };
	}
	if (releasePhase(release) === 'Retired') {
		const ago = formatAgo(release.status?.phaseSince, now);
		return {
			tone: 'warn',
			text: `No Fleets: release retired${ago ? ` ${ago}` : ''} · matches start on demand`
		};
	}
	const status = releaseStatus(release, fleets, detail.promotions, now);
	return { tone: status.tone, text: status.text };
};

export interface ChannelNow {
	ref: ChannelRef;
	observed: Nullable<ChannelStatus>;
	// "promoted by alice", or "via promotion <name>" when it isn't retained.
	promotedBy: string;
	fleets: Nullable<FleetsHealth>;
	running: Nullable<Promotion>;
	// The build to roll back to, when the channel's previous one is known.
	previous: Nullable<string>;
}

/** One "Now" card per channel: configured ones, then any observed channel no longer configured. */
export const channelsNow = (detail: ShardDetail, now: number): ChannelNow[] => {
	const observed = detail.status?.channels ?? [];
	const refs: ChannelRef[] = [
		...(detail.config?.channels ?? []),
		...observed.filter((o) => !(detail.config?.channels ?? []).some((c) => channelKey(c) === o.key))
	];
	return refs.map((ref) => {
		const seen = observed.find((o) => o.key === channelKey(ref)) ?? null;
		const by = seen?.promotion ?? '';
		const promotion = detail.promotions.find((p) => p.metadata.name === by);
		let promotedBy = '';
		if (promotion) promotedBy = `promoted by ${promotionChangedBy(promotion)}`;
		else if (by) promotedBy = `via promotion ${by}`;
		return {
			ref,
			observed: seen,
			promotedBy,
			fleets: channelFleets(detail, seen?.version, now),
			running: busyPromotion(detail.promotions, detail.name, ref),
			previous: previousVersion(detail.promotions, detail.name, ref, seen?.version)
		};
	});
};

export interface ShardProblem {
	tone: StatusTone;
	text: string;
}

/**
 * Problems that concern the whole shard, shown once above the channel cards: a gate that can never
 * pass, a cluster no release can reach, and a channel whose build has no Live release while Fleets
 * are on.
 */
export const shardProblems = (detail: ShardDetail, now: number): ShardProblem[] => {
	const problems: ShardProblem[] = [];
	const gate = detail.releases
		.flatMap((r) => r.status?.conditions ?? [])
		.find((c) => c.type === 'ConfigValid' && c.status === 'False' && c.reason === 'GateCannotPass');
	if (gate) problems.push({ tone: 'bad', text: gateCannotPassText(gate) });

	const active = detail.releases.filter((r) => releasePhase(r) !== 'Retired');
	for (const cluster of detail.config?.fleets?.clusters ?? []) {
		const seen = active.flatMap(
			(r) => r.status?.clusters.filter((c) => c.name === cluster.name) ?? []
		);
		if (seen.length && seen.every((c) => isUnreachable(c, now))) {
			const ages = seen.map((c) => formatAgo(c.observedAt, now)).filter(Boolean);
			problems.push({
				tone: 'bad',
				text: `Can't reach ${cluster.name}${
					ages.length ? `: counts are from ${ages[0]}` : ''
				}. Its Fleets keep running on their own.`
			});
		}
	}

	if (detail.config?.fleets && detail.spec?.fleetsEnabled) {
		// One line per build, however many channels serve it.
		const byVersion = new Map<string, string[]>();
		for (const channel of detail.status?.channels ?? []) {
			const release = releaseFor(detail.releases, channel.version);
			const phase = release ? releasePhase(release) : null;
			if (channel.version && phase !== 'Live' && phase !== 'Draining') {
				byVersion.set(channel.version, [...(byVersion.get(channel.version) ?? []), channel.key]);
			}
		}
		for (const [version, keys] of byVersion) {
			const release = releaseFor(detail.releases, version);
			const serve = `${listText(keys)} ${keys.length === 1 ? 'serves' : 'serve'} ${shortSha(
				version
			)}`;
			problems.push({
				tone: 'warn',
				text:
					release && releasePhase(release) === 'Retired'
						? `${serve}, whose release retired: its matches start on on-demand servers, or fail where the backend has no fallback. Warm it again from its release card, or promote another build.`
						: `${serve}, which has no Live release: its matches start on on-demand servers.`
			});
		}
	}
	return problems;
};

/**
 * The collapsed Fleet settings: "2 of 15 maps · warm buffer 2 · live Buffer 20% · min 1 · 1 cluster
 * · gate 1 Ready for 2 m, timeout 20 m".
 */
export const fleetSettingsLine = (detail: ShardDetail): string => {
	const fleets = detail.config?.fleets;
	const maps = detail.spec?.maps ?? [];
	const total = buildMapChoices(detail.releases)?.mapIds.length;
	const clusters = fleets?.clusters.length ?? 0;
	return [
		`${maps.length}${total ? ` of ${total}` : ''} map${(total ?? maps.length) === 1 ? '' : 's'}`,
		detail.spec?.warm?.bufferSize !== undefined
			? `warm buffer ${detail.spec.warm.bufferSize}`
			: 'no warm buffer',
		`live ${detail.spec?.live?.default ? policyText(detail.spec.live.default) : 'default sizing'}`,
		`${clusters} cluster${clusters === 1 ? '' : 's'}`,
		`gate ${fleets?.gate.minReady ?? 1} Ready for ${formatSpan(
			gateStableForMs(fleets)
		)}, timeout ${formatSpan(gateTimeoutMs(fleets))}`
	].join(' · ');
};

/** Fleet settings start expanded only when Fleets are off and not set up. */
export const settingsStartOpen = (detail: ShardDetail): boolean =>
	!detail.spec?.fleetsEnabled && !(detail.spec?.maps ?? []).length;

/**
 * Why the Shards list's Fleets column needs attention: a channel's build with no Live release
 * while Fleets are on, or a release that failed or is retired but still served. Null when fine.
 */
export const fleetsAttention = (shard: ShardSummary): Nullable<string> => {
	const releases = shard.status?.releases ?? [];
	const reasons: string[] = [];
	const on = !!shard.config?.fleets && !!shard.spec?.fleetsEnabled;
	for (const channel of on ? shard.status?.channels ?? [] : []) {
		const phase = releases.find((r) => r.version === channel.version)?.phase;
		if (channel.version && phase !== 'Live' && phase !== 'Draining') {
			reasons.push(`${channel.key} has no Fleets for its build`);
		}
	}
	const failed = releases.filter((r) => r.phase === 'Failed').map((r) => r.name);
	if (failed.length) reasons.push(`failed: ${failed.join(', ')}`);
	if (releases.some((r) => r.phase === 'Retired' && r.servedBy.length)) {
		reasons.push('a retired release is still served');
	}
	return reasons.length ? `${reasons.join('; ')}.` : null;
};
