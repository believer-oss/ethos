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
					100: '#abefca',
					200: '#75e0ac',
					300: '#38c987',
					400: '#19b070',
					500: '#0d8e5a',
					600: '#0a724b',
					700: '#0b5a3d',
					800: '#0a4a33',
					900: '#042a1e'
				},

				primary: {
					50: '#fafbeb',
					100: '#f0f5cc',
					200: '#e5ec9c',
					300: '#dae163',
					400: '#d5d738',
					500: '#c4bf2a',
					600: '#ac9c22',
					700: '#89741f',
					800: '#735e20',
					900: '#624e21',
					950: '#392a0f'
				}
			}
		}
	}
};

module.exports = config;
