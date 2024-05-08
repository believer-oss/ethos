const plugin = require('tailwindcss/plugin');
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
		require('flowbite/plugin')
	],

	darkMode: 'class',

	theme: {
		extend: {
			colors: {
				// flowbite-svelte
				secondary: {
					50: '#e9b8b9',
					100: '#dd8f93',
					200: '#cc676d',
					300: '#b64855',
					400: '#9e3a48',
					500: '#80313f',
					600: '#6e2d3a',
					700: '#55212B',
					800: '#3c151c'
				},

				primary: {
					100: '#fef9ec',
					200: '#fceec9',
					300: '#f9da8e',
					400: '#f6c55c',
					500: '#f3aa2c',
					600: '#ec8a14',
					700: '#d1660e',
					800: '#ae470f',
					900: '#8d3713',
					950: '#742e13'
				},

				space: {
					50: '#f4f7f7',
					100: '#e4e9e9',
					200: '#cbd5d6',
					300: '#a7b7b9',
					400: '#7b9295',
					500: '#60777a',
					600: '#536367',
					700: '#475457',
					800: '#3f484b',
					900: '#2f3537',
					950: '#22282a'
				}
			}
		}
	}
};

module.exports = config;
