import { writable } from 'svelte/store';

export type Theme =
	| 'dark'
	| 'light'
	| 'ayu'
	| 'catppuccin'
	| 'gruvbox'
	| 'hacker'
	| 'nord'
	| 'solarized'
	| 'synthwave'
	| 'tokyonight';

export const themes: { value: Theme; name: string }[] = [
	{ value: 'dark', name: 'Dark' },
	{ value: 'light', name: 'Light' },
	{ value: 'ayu', name: 'Ayu' },
	{ value: 'catppuccin', name: 'Catppuccin' },
	{ value: 'gruvbox', name: 'Gruvbox' },
	{ value: 'hacker', name: 'Hacker' },
	{ value: 'nord', name: 'Nord' },
	{ value: 'solarized', name: 'Solarized' },
	{ value: 'synthwave', name: "SynthWave '84" },
	{ value: 'tokyonight', name: 'Tokyo Night' }
];

// Keep in sync with the inline script in app.html, which applies the theme before first paint.
const STORAGE_KEY = 'color-theme';
const DEFAULT_THEME: Theme = 'dark';

const isTheme = (value: string | null): value is Theme =>
	themes.some((theme) => theme.value === value);

const loadTheme = (): Theme => {
	try {
		const stored = localStorage.getItem(STORAGE_KEY);
		return isTheme(stored) ? stored : DEFAULT_THEME;
	} catch {
		return DEFAULT_THEME;
	}
};

const applyTheme = (theme: Theme) => {
	const root = document.documentElement;
	// Every theme but light is a dark palette, so they keep the `dark` class for Tailwind's dark: variants.
	root.classList.toggle('dark', theme !== 'light');
	themes.forEach(({ value }) => {
		if (value !== 'dark' && value !== 'light') root.classList.toggle(value, value === theme);
	});
};

export const theme = writable<Theme>(loadTheme());

theme.subscribe((value) => {
	applyTheme(value);
	try {
		localStorage.setItem(STORAGE_KEY, value);
	} catch {
		// Theme still applies for this session.
	}
});
