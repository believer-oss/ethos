const plugin = require('tailwindcss/plugin');
/** @type {import('tailwindcss').Config}*/
const config = {
	content: [
		'./src/**/*.{html,js,svelte,ts}',
		'./node_modules/flowbite-svelte/**/*.{html,js,svelte,ts}'
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
			colors: {}
		}
	}
};

module.exports = config;
