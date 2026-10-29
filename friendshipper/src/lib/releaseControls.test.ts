import { get } from 'svelte/store';
import { describe, expect, it } from 'vitest';
import { appConfig, releaseControlsEnabled } from '$lib/stores';
import type { AppConfig } from '$lib/types';

describe('releaseControlsEnabled', () => {
	it('is off unless the preference is set', () => {
		appConfig.set(<AppConfig>{});
		expect(get(releaseControlsEnabled)).toBe(false);

		appConfig.set(<AppConfig>{ releaseControlsEnabled: false });
		expect(get(releaseControlsEnabled)).toBe(false);
	});

	it('follows the preference', () => {
		appConfig.set(<AppConfig>{ releaseControlsEnabled: true });
		expect(get(releaseControlsEnabled)).toBe(true);
	});
});
