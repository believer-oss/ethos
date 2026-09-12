import type { ObjectCountResponse } from '$lib/types';

/// Coarse "how long ago" text for the paused indicator.
///
/// `now` is injected rather than read from the clock inside, so the caller can tick it on a timer
/// and keep the text live without re-fetching, and so the function stays pure.
///
/// Deliberately hand-rolled rather than pulling in a date library: the app already formats
/// durations by hand (see `formatUptime` in PreferencesModal), and this only ever needs day/hour
/// granularity.
export const formatPackedAge = (lastPacked: string | null, now: number = Date.now()): string => {
	if (!lastPacked) {
		return 'never';
	}

	const then = new Date(lastPacked).getTime();
	if (Number.isNaN(then)) {
		return 'unknown';
	}

	const seconds = Math.floor((now - then) / 1000);
	// Clock skew, or a run that finished moments ago, can make this marginally negative.
	if (seconds < 60) {
		return 'just now';
	}

	const minutes = Math.floor(seconds / 60);
	if (minutes < 60) {
		return `${minutes} minute${minutes === 1 ? '' : 's'} ago`;
	}

	const hours = Math.floor(minutes / 60);
	if (hours < 24) {
		return `${hours} hour${hours === 1 ? '' : 's'} ago`;
	}

	const days = Math.floor(hours / 24);
	return `${days} day${days === 1 ? '' : 's'} ago`;
};

/// Full annotation for the sidebar banner, which has room for the detail.
///
/// The loose count is what makes the timestamp actionable: "last packed 6 days ago" says nothing
/// about whether the developer needs to act, while the pair does.
///
/// The count is as of the last fetch rather than live: re-reading it costs a `count-objects` walk,
/// whereas re-deriving the age from an absolute timestamp is free. Only the age ticks.
export const formatFreshness = (
	freshness: ObjectCountResponse | null,
	now: number = Date.now()
): string => {
	if (!freshness) {
		return '';
	}

	const age = formatPackedAge(freshness.lastPacked, now);
	const loose = freshness.looseCount.toLocaleString();
	return `repo last packed ${age} · ${loose} loose objects`;
};
