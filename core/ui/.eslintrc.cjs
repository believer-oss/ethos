/**
 * @type {import('eslint').Linter.Config}
 */
module.exports = {
	root: true,
	extends: [
		'eslint:recommended',
		'airbnb-base',
		'airbnb-typescript/base',
		'plugin:@typescript-eslint/eslint-recommended',
		'plugin:@typescript-eslint/recommended-requiring-type-checking',
		'plugin:@typescript-eslint/strict-type-checked',
		'plugin:svelte/recommended',
		'prettier'
	],
	env: {
		browser: true,
		es2017: true,
		node: true
	},
	parser: '@typescript-eslint/parser',
	plugins: ['@typescript-eslint'],
	parserOptions: {
		sourceType: 'module',
		ecmaVersion: 2020,
		project: './tsconfig.lint.json',
		extraFileExtensions: ['.svelte']
	},
	globals: {
		svelte: 'readonly',
		$$Generic: 'readonly'
	},
	rules: {
		// AirBnB rules that don't work well with our config files
		'import/no-extraneous-dependencies': 'off',
		'import/extensions': 'off',
		'import/no-unresolved': 'off',

		// Simply do not care about these
		'import/prefer-default-export': 'off',
		'no-restricted-syntax': 'off',

		// Make sure we support skipping _ for unused vars
		'@typescript-eslint/no-unused-vars': [
			'error',
			{
				args: 'all',
				argsIgnorePattern: '^_',
				caughtErrors: 'all',
				caughtErrorsIgnorePattern: '^_',
				destructuredArrayIgnorePattern: '^_',
				varsIgnorePattern: '^_',
				ignoreRestSiblings: true
			}
		]
	},
	overrides: [
		{
			files: ['*.svelte'],
			extends: ['plugin:svelte/recommended'],
			parser: 'svelte-eslint-parser',
			parserOptions: {
				sourceType: 'module',
				ecmaVersion: 'latest',
				parser: '@typescript-eslint/parser',
				extraFileExtensions: ['.svelte'],
				tsconfigRootDir: __dirname,
				project: './tsconfig.lint.json'
			},
			settings: {
				svelte: {
					ignoreWarnings: [
						'@typescript-eslint/no-unsafe-assignment', // reduce false positives
						'@typescript-eslint/no-unsafe-member-access' // reduce false positives
					]
				}
			},
			rules: {
				// incompatible with svelte's generic props
				'@typescript-eslint/no-unsafe-assignment': 'off',
				'@typescript-eslint/no-unsafe-member-access': 'off',

				// not consistent in svelte files
				'@typescript-eslint/no-unsafe-argument': 'off',
				'@typescript-eslint/no-unsafe-call': 'off',
				'@typescript-eslint/no-unnecessary-condition': 'off',

				// AirBnB rules that don't work well with Svelte
				'no-sequences': 'off',
				'import/no-extraneous-dependencies': 'off',
				'import/extensions': 'off',
				'import/no-unresolved': 'off',
				'no-void': ['error', { allowAsStatement: true }],
				'import/no-mutable-exports': 'off',

				// This one feels bad, but this rule triggers for $ expressions,
				// so for now it's gotta go.
				'@typescript-eslint/no-unused-expressions': 'off',

				// Svelte
				'svelte/no-dupe-use-directives': 'error',
				'svelte/no-dom-manipulating': 'error',
				'svelte/no-export-load-in-svelte-module-in-kit-pages': 'error',
				'svelte/no-store-async': 'error',
				'svelte/require-store-callbacks-use-set-param': 'error',
				'svelte/no-target-blank': 'error',
				'svelte/no-reactive-functions': 'error',
				'svelte/no-reactive-literals': 'error',
				'svelte/no-useless-mustaches': 'error',
				'svelte/require-optimized-style-attribute': 'error',
				'svelte/require-stores-init': 'error',

				'no-trailing-spaces': 'off', // superseded
				'svelte/no-trailing-spaces': 'error',

				// Stylistic
				'svelte/derived-has-same-inputs-outputs': 'error',
				'svelte/html-closing-bracket-spacing': 'error',
				'svelte/html-quotes': 'error',
				'svelte/mustache-spacing': 'error',
				'svelte/no-extra-reactive-curlies': 'error',
				'svelte/no-spaces-around-equal-signs-in-attribute': 'error',
				'svelte/prefer-class-directive': 'error',
				'svelte/prefer-style-directive': 'error',
				'svelte/shorthand-attribute': 'error',
				'svelte/shorthand-directive': 'error',
				'svelte/spaced-html-comment': 'error'
			}
		}
	]
};
