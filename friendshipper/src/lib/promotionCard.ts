// Pure helpers for a promotion's card and the running-promotions tray: its steps with one link
// each, the ended summary, the one-line history entry, and the Steam `public` flip that waits for a
// person. No Tauri imports, so they can be tested on their own.

import type {
	FleetRelease,
	FleetsConfig,
	Nullable,
	Promotion,
	PromotionStep,
	ShardConfigSpec
} from '$lib/types';
import {
	STEP_DEPLOY_BACKEND,
	STEP_FLIP_CHANNEL,
	STEP_SKIPPED,
	STEP_WARM_FLEETS,
	channelKey,
	channelLabel,
	formatAgo,
	formatElapsed,
	formatSpan,
	isActivePromotion,
	isCancelling,
	localTime,
	parseBackendVersion,
	promotionCancel,
	promotionChangedBy,
	promotionPhase,
	promotionPhaseLabel,
	promotionTimeline,
	shortSha,
	stepLabel,
	type StepState
} from '$lib/fleetFormat';
import { releaseStatus } from '$lib/fleetReleases';

// An RFC 3339 UTC time inside a coordinator message.
const RFC3339 = /\d{4}-\d\d-\d\dT[\d:.]+Z/g;

/** A coordinator message with its RFC 3339 times in local time ("tomorrow 19:19"). */
export const localizeTimes = (text: string, now: number): string =>
	text.replace(RFC3339, (at) => localTime(at, now) || at);

// fleet-operator's STEP_NOT_RUN: a step the promotion ended before.
const STEP_NOT_RUN = 'the promotion ended before this step ran';
// fleet-operator's argo::OUTPUT_BUILD_ID, on publish-client and the `public` flip.
const OUTPUT_BUILD_ID = 'build_id';

/** The Steam `public` flip, waiting for a person to set the build live in Steamworks. */
export const waitingForPerson = (p: Promotion): boolean => {
	if (!isActivePromotion(p) || isCancelling(p)) return false;
	const { channel } = p.spec;
	if (channel.kind !== 'steam' || channel.branch?.toLowerCase() !== 'public') return false;
	const step = p.status?.steps.find((s) => s.name === STEP_FLIP_CHANNEL);
	return step?.phase === 'Running' && !step.workflow;
};

/** The Steam build ID the `public` flip waits on, from its step's outputs. */
export const flipBuildId = (p: Promotion): Nullable<string> =>
	p.status?.steps.find((s) => s.name === STEP_FLIP_CHANNEL)?.outputs?.[OUTPUT_BUILD_ID] ?? null;

/** The phase as the card shows it: "Waiting for you" while the `public` flip needs a person. */
export const promotionCardPhase = (p: Promotion): string =>
	waitingForPerson(p) ? 'Waiting for you' : promotionPhaseLabel(p);

/** "Cancelling… by alice, 2 m ago" detail, from the cancel annotations. */
export const cancellingText = (p: Promotion, now: number): string => {
	const cancel = promotionCancel(p);
	if (!isCancelling(p) || !cancel) return '';
	const ago = formatAgo(cancel.requestedAt, now);
	return [cancel.requestedBy ? `by ${cancel.requestedBy}` : '', ago].filter(Boolean).join(', ');
};

/** A Succeeded promotion someone tried to cancel: the cancel came after the switch started. */
export const cancelTooLate = (p: Promotion): boolean =>
	promotionPhase(p) === 'Succeeded' && promotionCancel(p) !== null;

/** The game config, when it isn't the channel's default for promotions. */
export const gameConfigNote = (p: Promotion, config: Nullable<ShardConfigSpec>): string => {
	const used = p.status?.gameConfig;
	if (!used) return '';
	const channel = config?.channels.find((c) => channelKey(c) === channelKey(p.spec.channel));
	return channel?.gameConfig === used ? '' : `Game config ${used}`;
};

const durationMs = (s: PromotionStep): Nullable<number> => {
	const start = s.startedAt ? Date.parse(s.startedAt) : NaN;
	const end = s.finishedAt ? Date.parse(s.finishedAt) : NaN;
	return Number.isNaN(start) || Number.isNaN(end) ? null : end - start;
};

/**
 * How long a step usually takes on this shard: the median over its last five Succeeded
 * promotions. Null with none to go by.
 */
export const typicalStepMs = (history: Promotion[], step: string): Nullable<number> => {
	const times = history
		.filter((p) => promotionPhase(p) === 'Succeeded')
		.sort((a, b) => Date.parse(b.status?.finishedAt ?? '') - Date.parse(a.status?.finishedAt ?? ''))
		.flatMap((p) => {
			const s = p.status?.steps.find((x) => x.name === step && x.phase === 'Succeeded');
			const ms = s ? durationMs(s) : null;
			return ms === null ? [] : [ms];
		})
		.slice(0, 5)
		.sort((a, b) => a - b);
	return times.length ? times[Math.floor((times.length - 1) / 2)] : null;
};

const partSha = (raw: string): string => shortSha(parseBackendVersion(raw)?.sha ?? raw);

/** "platform a → b, content c → d", or "no part changes", from the plan made at acceptance. */
export const backendPlanText = (p: Promotion): string => {
	const parts = p.status?.plan?.backend?.parts ?? [];
	const changed = parts.filter((c) => c.from !== c.to);
	if (!changed.length) return 'no part changes';
	return changed
		.map((c) => `${c.name} ${c.from ? partSha(c.from) : 'unknown'} → ${partSha(c.to)}`)
		.join(', ');
};

export type StepRowState = StepState | 'stopped' | 'notRun';

export interface StepRow {
	name: string;
	label: string;
	state: StepRowState;
	// What the row says after the label: why it was skipped, what it waits for, how it failed.
	detail: string;
	// How long it took or has run; empty when it hasn't started.
	elapsed: string;
	// For a pending step: "usually ~3 m".
	typical: string;
	// The step's own link: its Argo workflow once started, or warm-fleets' release once it exists.
	workflow: Nullable<string>;
	release: Nullable<string>;
}

// A step's Workflow stopped by a cancel: Argo's message for the Stop strategy.
const STOPPED = /\bstop/i;

const isStoppedByCancel = (p: Promotion, s: PromotionStep | undefined): boolean =>
	!!s &&
	promotionPhase(p) === 'Cancelled' &&
	(s.phase === 'Failed' || s.phase === 'Error') &&
	(STOPPED.test(s.message) ||
		s.message.startsWith('cancel requested by') ||
		s.message === 'deleted');

export interface StepContext {
	// This shard's promotions, for the typical durations.
	history: Promotion[];
	releases: FleetRelease[];
	fleets: Nullable<FleetsConfig> | undefined;
	now: number;
}

// A step has started once it has a start time and wasn't skipped: only then does its Workflow exist.
const hasStarted = (step: { startedAt?: Nullable<string>; state: string }, row: StepRowState) =>
	!!step.startedAt && step.state !== 'pending' && step.state !== 'skipped' && row !== 'notRun';

/** The card's step rows, one link each, with the backend plan on the deploy-backend row. */
export const promotionStepRows = (p: Promotion, ctx: StepContext): StepRow[] => {
	const steps = p.status?.steps ?? [];
	return promotionTimeline(p).map((step, i) => {
		const raw = steps.at(i);
		let rowState: StepRowState = step.state;
		let detail = localizeTimes(step.message, ctx.now);
		if (isStoppedByCancel(p, raw)) {
			rowState = 'stopped';
			detail = 'stopped by the cancel';
		} else if (step.phase === STEP_SKIPPED && step.message === STEP_NOT_RUN) {
			rowState = 'notRun';
			detail = 'not run';
		} else if (step.name === STEP_DEPLOY_BACKEND && p.status?.plan?.backend) {
			if (step.state === 'skipped') detail = `skipped: ${detail || 'no deploy'}`;
			else if (step.state !== 'failed') detail = backendPlanText(p);
		} else if (step.state === 'skipped') {
			detail = detail ? `skipped: ${detail}` : 'skipped';
		}
		if (step.name === STEP_WARM_FLEETS && step.state === 'running') {
			const release = ctx.releases.find((r) => r.metadata.name === step.fleetRelease);
			if (release) detail = releaseStatus(release, ctx.fleets, [p], ctx.now).text;
		}
		const end = step.finishedAt ? Date.parse(step.finishedAt) : ctx.now;
		const typicalMs = step.state === 'pending' ? typicalStepMs(ctx.history, step.name) : null;
		return {
			name: step.name,
			label: step.label,
			state: rowState,
			detail,
			elapsed: step.startedAt
				? formatElapsed(step.startedAt, Number.isNaN(end) ? ctx.now : end)
				: '',
			typical: typicalMs === null ? '' : `usually ~${formatSpan(typicalMs)}`,
			workflow: hasStarted(step, rowState) ? step.workflow || null : null,
			release: step.name === STEP_WARM_FLEETS ? p.status?.fleetRelease ?? null : null
		};
	});
};

// The summary's change items, as fleet-operator's steps::summary writes them.
const SUMMARY_START =
	/: (?=backend deployed|release \S+ (?:kept|live)|client uploaded|channel flipped|nothing changed outside the cluster)/g;
const NOTHING_CHANGED = 'nothing changed outside the cluster';

/** A finished promotion's message split into its cause and its summary ("<cause>: <summary>"). */
export const splitEndMessage = (message: string): { cause: string; summary: string } => {
	let at = -1;
	for (const m of message.matchAll(SUMMARY_START)) at = m.index;
	return at < 0
		? { cause: message, summary: '' }
		: { cause: message.slice(0, at), summary: message.slice(at + 2) };
};

const lowerFirst = (s: string) => s.charAt(0).toLowerCase() + s.slice(1);

/**
 * A Failed or Cancelled promotion's summary in two lines: what reached players (or that nothing
 * did) and what didn't happen, then what it left behind. A kept release's TTL is shown as its
 * expiry, when the release is known.
 */
export const endSummary = (p: Promotion, releases: FleetRelease[], now: number): string[] => {
	const phase = promotionPhase(p);
	if (phase !== 'Failed' && phase !== 'Cancelled') return [];
	const { summary } = splitEndMessage(p.status?.message ?? '');
	if (!summary) return [];
	if (summary === NOTHING_CHANGED)
		return ['Nothing reached players: nothing changed outside the cluster.'];
	const dot = summary.indexOf('. ');
	const [done, notDone] = dot < 0 ? [summary, ''] : [summary.slice(0, dot), summary.slice(dot + 2)];
	const items = done.split('; ');
	const kept = items.filter((item) => item.includes(' kept, '));
	const changed = items.filter((item) => !kept.includes(item));
	const lines = [
		changed.length
			? `Changed: ${changed.join('; ')}${notDone ? `. Not done: ${lowerFirst(notDone)}` : ''}.`
			: `Nothing reached players: ${lowerFirst(notDone) || 'nothing changed'}.`
	];
	if (kept.length) {
		const left = kept.map((item) => {
			const name = /^release (\S+) kept/.exec(item)?.[1];
			const until = releases.find((r) => r.metadata.name === name)?.status?.warmUntil;
			return until
				? `release ${name} kept, expires ${localTime(until, now)} unless used`
				: localizeTimes(item, now);
		});
		lines.push(`Left: ${left.join('; ')}.`);
	}
	return lines;
};

/** Shown expanded in the history: running, waiting for a person, or not finished. */
export const isExpandedByDefault = (p: Promotion): boolean =>
	isActivePromotion(p) || waitingForPerson(p);

const ICONS: Record<string, string> = {
	Succeeded: '●',
	Failed: '✕',
	Rejected: '✕',
	Cancelled: '■'
};

export interface HistoryLine {
	icon: string;
	tone: 'ok' | 'bad' | 'muted';
	channel: string;
	versions: string;
	who: string;
	when: string;
	took: string;
	outcome: string;
}

const reachedPlayers = (p: Promotion): boolean =>
	(p.status?.changes ?? []).some(
		(c) => c.kind === 'ChannelFlipped' || c.kind === 'BackendDeployed'
	);

const deploySkipped = (p: Promotion): boolean => {
	const step = p.status?.steps.find((s) => s.name === STEP_DEPLOY_BACKEND);
	return !step || step.phase === STEP_SKIPPED;
};

/** The phrase saying how a finished promotion ended. */
export const historyOutcome = (p: Promotion): string => {
	const phase = promotionPhase(p);
	const flipped = (p.status?.changes ?? []).some((c) => c.kind === 'ChannelFlipped');
	const backend = deploySkipped(p) ? 'backend skipped' : 'backend deployed';
	if (phase === 'Succeeded') {
		return cancelTooLate(p) ? `cancel came too late · completed · ${backend}` : backend;
	}
	if (phase === 'Cancelled') {
		const stopped = promotionStepRows(p, { history: [], releases: [], fleets: null, now: 0 }).find(
			(r) => r.state === 'stopped'
		);
		const during = stopped ? `during ${stopped.label}` : 'before it started';
		return `Cancelled ${during} · ${
			reachedPlayers(p) ? 'changes reached players' : 'nothing reached players'
		}`;
	}
	const { cause } = splitEndMessage(p.status?.message ?? '');
	if (phase === 'Failed')
		return `Failed: ${cause} · ${flipped ? 'channel switched' : 'channel unchanged'}`;
	return `${phase ?? 'Submitted'}: ${p.status?.message ?? ''}`;
};

/** One line for a finished promotion in the history. */
export const historyLine = (p: Promotion, now: number): HistoryLine => {
	const phase = promotionPhase(p) ?? '';
	const start = p.status?.startedAt;
	const end = p.status?.finishedAt;
	let tone: HistoryLine['tone'] = 'muted';
	if (phase === 'Succeeded') tone = 'ok';
	else if (phase === 'Failed' || phase === 'Rejected') tone = 'bad';
	const from = p.status?.previousVersion;
	return {
		icon: ICONS[phase] ?? '○',
		tone,
		channel: channelLabel(p.spec.channel),
		versions: from ? `${shortSha(from)} → ${shortSha(p.spec.version)}` : shortSha(p.spec.version),
		who: promotionChangedBy(p),
		when: formatAgo(end ?? p.metadata.creationTimestamp, now),
		took: start && end ? formatElapsed(start, Date.parse(end)) : '',
		outcome: localizeTimes(historyOutcome(p), now)
	};
};

/**
 * The tray's second line: "<step> · <who> · <elapsed> · <step detail>", the detail last since it can
 * run long. A cancelling promotion leads with "cancelling".
 */
export const trayDetail = (p: Promotion, now: number): string => {
	const current = p.status?.currentStep;
	const step = p.status?.steps.find((s) => s.name === current);
	let what = current ? stepLabel(current) : promotionPhaseLabel(p);
	if (waitingForPerson(p)) what = 'waiting for you';
	if (isCancelling(p) && current) what = `cancelling during ${what}`;
	const detail = step?.message ? localizeTimes(step.message, now) : '';
	const since = formatElapsed(p.status?.startedAt ?? p.metadata.creationTimestamp, now);
	return [what, promotionChangedBy(p), since, detail].filter(Boolean).join(' · ');
};
