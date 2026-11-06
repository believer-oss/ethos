import { describe, expect, it } from 'vitest';
import { clockTime, errorText, withPollError, withoutPollErrors } from '$lib/pollErrors';

describe('poll error toasts', () => {
	const at = new Date(2026, 10, 4, 5, 20);

	it('shows one toast per failing poll, with when it started', () => {
		const first = withPollError([], 'builds', new Error('Couldn’t reach the local service'), at, 1);
		expect(first).toEqual([
			{ id: 1, source: 'builds', message: 'Couldn’t reach the local service (at 05:20)' }
		]);
		// The next failure of the same poll doesn't add another.
		const later = new Date(2026, 10, 4, 5, 21);
		expect(withPollError(first, 'builds', { message: 'other' }, later, 2)).toBe(first);
		expect(withPollError(first, 'playtests', 'down', later, 2)).toHaveLength(2);
	});

	it('clears a poll’s toast once it succeeds, and leaves the others', () => {
		const action = { id: 0, message: 'Save failed' };
		const errors = withPollError([action], 'builds', 'down', at, 1);
		expect(withoutPollErrors(errors, 'builds')).toEqual([action]);
		expect(withoutPollErrors([action], 'builds')).toEqual([action]);
	});

	it('reads the text of any error payload', () => {
		expect(errorText('plain')).toBe('plain');
		expect(errorText({ message: 'from Rust', status_code: 500 })).toBe('from Rust');
		expect(errorText({ status_code: 500 })).toBe('{"status_code":500}');
		expect(clockTime(new Date(2026, 0, 1, 9, 5))).toBe('09:05');
	});
});
