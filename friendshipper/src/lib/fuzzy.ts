const WORD_BOUNDARIES = '-_/. ';

/** Subsequence match score, or null when `query` is not a subsequence of `text`. */
export const fuzzyScore = (query: string, text: string): number | null => {
	const q = query.trim().toLowerCase();
	if (q === '') {
		return 0;
	}
	const t = text.toLowerCase();

	let score = 0;
	let pos = 0;
	let prev = -2;
	for (const ch of q) {
		const idx = t.indexOf(ch, pos);
		if (idx === -1) {
			return null;
		}
		score += 1;
		if (idx === prev + 1) {
			score += 5;
		}
		if (idx === 0 || WORD_BOUNDARIES.includes(t[idx - 1])) {
			score += 8;
		}
		prev = idx;
		pos = idx + ch.length;
	}
	return score;
};

/** Filters and ranks `items`; the first field from `fields` gets a +20 bonus. */
export const fuzzyFilter = <T>(items: T[], query: string, fields: (item: T) => string[]): T[] => {
	if (query.trim() === '') {
		return items;
	}

	const scored: { item: T; score: number }[] = [];
	for (const item of items) {
		let best: number | null = null;
		for (const [i, field] of fields(item).entries()) {
			const s = fuzzyScore(query, field);
			if (s !== null) {
				const total = i === 0 ? s + 20 : s;
				if (best === null || total > best) {
					best = total;
				}
			}
		}
		if (best !== null) {
			scored.push({ item, score: best });
		}
	}
	return scored.sort((a, b) => b.score - a.score).map((s) => s.item);
};
