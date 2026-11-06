// Pure helpers for the promote form: whether the build is a rollback for the channel, what happens
// to the build's release, and the steps the promotion will run. No Tauri imports, so they can be
// tested on their own.

import type {
	ChannelRef,
	CommitWorkflowInfo,
	FleetRelease,
	FleetsConfig,
	Nullable,
	Promotion,
	PromotionPlan
} from '$lib/types';
import {
	backendParts,
	channelKey,
	formatAgo,
	promotionPhase,
	shortSha,
	type BackendPart
} from '$lib/fleetFormat';
import { partTimes } from '$lib/shardPage';
import { isRetiring, releasePhase, releaseStatus } from '$lib/fleetReleases';

export interface Rollback {
	// The build the channel served before `replacedBy`, which is the requested one.
	version: string;
	replacedBy: string;
	// When `replacedBy` took over.
	until: Nullable<string>;
}

/**
 * Whether promoting `version` to the channel goes back to a build it served before, as its retained
 * promotions record: a Succeeded promotion there replaced `version`. The newest such wins. The build
 * the channel serves now is never a rollback.
 */
export const rollbackOf = (
	promotions: Promotion[],
	shard: string,
	channel: ChannelRef,
	version: string,
	current: Nullable<string> | undefined
): Nullable<Rollback> => {
	if (!version || version === current) return null;
	const key = channelKey(channel);
	const replaced = promotions
		.filter(
			(p) =>
				p.spec.shard === shard &&
				channelKey(p.spec.channel) === key &&
				promotionPhase(p) === 'Succeeded' &&
				p.status?.previousVersion === version &&
				p.spec.version !== version
		)
		.sort(
			(a, b) =>
				(Date.parse(b.status?.finishedAt ?? '') || 0) -
				(Date.parse(a.status?.finishedAt ?? '') || 0)
		)
		.at(0);
	if (!replaced) return null;
	return {
		version,
		replacedBy: replaced.spec.version,
		until: replaced.status?.finishedAt ?? null
	};
};

/** "↩ Rollback. abc12345 is the build this channel served before def67890 (until 2 d ago)." */
export const rollbackText = (r: Rollback, now: number): string => {
	const until = r.until ? formatAgo(r.until, now) : '';
	return `↩ Rollback. ${shortSha(r.version)} is the build this channel served before ${shortSha(
		r.replacedBy
	)}${until ? ` (until ${until})` : ''}.`;
};

export interface ReleasePlan {
	text: string;
	// Fleets are ready without a warm-up.
	ready: boolean;
	// Why Promote is blocked unless the warm gate is skipped; empty when it isn't.
	blocks: string;
}

const WARM_UP = 'about 3–4 min';

/** What happens to the build's release on this shard when it's promoted. */
export const releasePlan = (
	release: Nullable<FleetRelease> | undefined,
	fleets: Nullable<FleetsConfig> | undefined,
	now: number
): ReleasePlan => {
	if (!release) return { text: `A new release warms (${WARM_UP})`, ready: false, blocks: '' };
	if (isRetiring(release)) {
		return {
			text: 'Release Retiring: waits for it to finish retiring, then warms afresh',
			ready: false,
			blocks: ''
		};
	}
	switch (releasePhase(release)) {
		case 'Warm':
		case 'Live':
			return { text: 'Fleets ready now', ready: true, blocks: '' };
		case 'Draining':
			return { text: 'Release Draining: reused at once, no warm-up', ready: true, blocks: '' };
		case 'Retired': {
			const ago = formatAgo(release.status?.phaseSince, now);
			return {
				text: `Release Retired${ago ? ` ${ago}` : ''}: recreated, ${WARM_UP} warm-up`,
				ready: false,
				blocks: ''
			};
		}
		case 'Failed': {
			const why = releaseStatus(release, fleets, [], now).text;
			return {
				text: `Release Failed: ${why}`,
				ready: false,
				blocks:
					"This build's release has failed, so the warm gate can't pass. Fix the cause, or skip the warm gate."
			};
		}
		default:
			return { text: 'Release Warming: the gate waits for it', ready: false, blocks: '' };
	}
};

/**
 * "Steps: backend deploy · Fleets ready now · publish client (~3 m) · switch the channel (~10 s)."
 * `fleets` is null when the shard has no Fleets on.
 */
export const stepsLine = (
	deployBackend: boolean,
	fleets: Nullable<{ plan: ReleasePlan; skipGate: boolean }>
): string => {
	const parts = [`backend ${deployBackend ? 'deploy' : 'skipped'}`];
	if (fleets) {
		let warm = `warm ~3–4 min`;
		if (fleets.plan.ready) warm = 'ready now';
		else if (fleets.skipGate) warm = 'warm alongside (gate skipped)';
		parts.push(`Fleets ${warm}`);
	}
	parts.push('publish client (~3 m)', 'switch the channel (~10 s)');
	return `Steps: ${parts.join(' · ')}.`;
};

export type PartDirection = 'newer' | 'older' | 'same' | 'unknown';

export interface PartRow {
	name: string;
	direction: PartDirection;
	// "shard now abc12345, built 2 d ago, deployed 1 d ago"
	shard: string;
	// "this build def67890, built 3 h ago"
	build: string;
}

const sideText = (
	label: string,
	part: Nullable<BackendPart>,
	promotions: Promotion[],
	now: number
): string => {
	if (!part) return `${label} none`;
	const sha = part.version ? shortSha(part.version.sha) : part.raw;
	const times = partTimes(part, promotions, now);
	return `${label} ${sha}${times ? `, ${times.replace(' · ', ', ')}` : ''}`;
};

/**
 * The backend parts a promotion would deploy, each next to the shard's: newer or older by build
 * time (the version's prefix, UTC), or unchanged. Deploy times only where a promotion recorded one.
 */
export const backendDiff = (
	plan: Nullable<PromotionPlan> | undefined,
	promotions: Promotion[],
	now: number
): PartRow[] => {
	if (!plan?.manifest) return [];
	const shard = backendParts(plan.shardBackend);
	return backendParts(plan.manifest.backend).map((build) => {
		const current = shard.find((p) => p.name === build.name) ?? null;
		let direction: PartDirection = 'unknown';
		if (current?.raw === build.raw) direction = 'same';
		else {
			const from = current?.version?.builtAt?.getTime();
			const to = build.version?.builtAt?.getTime();
			if (from !== undefined && to !== undefined) {
				if (to > from) direction = 'newer';
				else if (to < from) direction = 'older';
			}
		}
		return {
			name: build.name,
			direction,
			shard: sideText('shard now', current, promotions, now),
			build: sideText('this build', build, [], now)
		};
	});
};

/** The parts deploying this build would take backwards. */
export const olderParts = (rows: PartRow[]): string[] =>
	rows.filter((r) => r.direction === 'older').map((r) => r.name);

/**
 * Deploy backend's default: on only when the build's manifest was read and nothing would go
 * backwards. Off for a rollback or an older part, and off when the manifest can't be read: the
 * coordinator deploys every part that differs, newer or older, so with nothing to compare against,
 * deploying is left to the person to choose.
 */
export const deployBackendDefault = (
	plan: Nullable<PromotionPlan>,
	rollback: boolean,
	older: string[]
): boolean => !!plan?.manifest && !rollback && older.length === 0;

/** Said next to Deploy backend when the build's manifest can't be read. */
export const BACKEND_UNAVAILABLE =
	"Backend comparison unavailable: this build's manifest can't be read";

/**
 * "main · pushed 3 h ago": the build's own line, from its Builds entry. Its time is the entry's
 * creationTimestamp, when CI picked the commit up: the workflows' own times arrive as clock times
 * without a date. Without a time the line is just the branch.
 */
export const buildLine = (entry: Nullable<CommitWorkflowInfo> | undefined, now: number): string => {
	if (!entry) return '';
	const ago = formatAgo(entry.creationTimestamp, now);
	return [entry.branch ?? '', ago ? `pushed ${ago}` : ''].filter(Boolean).join(' · ');
};
