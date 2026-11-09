const plugin = require('tailwindcss/plugin');
const colors = require('tailwindcss/colors');
const flowbite = require('flowbite/plugin')();

const themeColor = (name, shades) =>
	Object.fromEntries(
		shades.map((shade) => [shade, `rgb(var(--color-${name}-${shade}) / <alpha-value>)`])
	);

// flowbite's base styles read gray from the theme, including into a data-URI SVG where CSS variables
// can't resolve, so give it the static default palette.
const flowbitePlugin = plugin(
	(api) =>
		flowbite.handler({
			...api,
			theme: (path, defaultValue) =>
				path.startsWith('colors.gray.')
					? colors.gray[path.slice('colors.gray.'.length)]
					: api.theme(path, defaultValue)
		}),
	flowbite.config
);

/** @type {import('tailwindcss').Config}*/
const config = {
	content: [
		'./src/**/*.{html,js,svelte,ts}',
		'./node_modules/flowbite-svelte/**/*.{html,js,svelte,ts}',
		'../core/ui/src/**/*.{html,js,svelte,ts}'
	],

	plugins: [
		plugin(function ({ addBase }) {
			addBase({
				html: { fontSize: '14px' }
			});
		}),
		flowbitePlugin
	],

	darkMode: 'class',

	theme: {
		extend: {
			// Values live in app.postcss as CSS variables so themes can swap them.
			colors: {
				white: 'rgb(var(--color-white) / <alpha-value>)',
				gray: themeColor('gray', [50, 100, 200, 300, 400, 500, 600, 700, 800, 900, 950]),
				// flowbite-svelte
				secondary: themeColor('secondary', [50, 100, 200, 300, 400, 500, 600, 700, 800]),
				primary: themeColor('primary', [100, 200, 300, 400, 500, 600, 700, 800, 900, 950]),
				space: themeColor('space', [50, 100, 200, 300, 400, 500, 600, 700, 800, 900, 950])
			}
		}
	}
};

module.exports = config;
