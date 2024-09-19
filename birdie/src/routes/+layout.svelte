<script lang="ts">
	import '../app.postcss';
	import { onMount } from 'svelte';
	import {
		Accordion,
		AccordionItem,
		Button,
		Img,
		Modal,
		Sidebar,
		SidebarDropdownItem,
		SidebarDropdownWrapper,
		SidebarGroup,
		SidebarItem,
		SidebarWrapper,
		Spinner
	} from 'flowbite-svelte';
	import {
		CloseSolid,
		CodeBranchSolid,
		CogOutline,
		ComputerSpeakerSolid,
		HomeSolid,
		MinusOutline,
		WindowOutline
	} from 'flowbite-svelte-icons';
	import { emit, listen } from '@tauri-apps/api/event';
	import { Canvas } from '@threlte/core';
	import semver from 'semver';
	import { getVersion } from '@tauri-apps/api/app';
	import { get } from 'svelte/store';
	import { invoke } from '@tauri-apps/api/tauri';
	import { readTextFile } from '@tauri-apps/api/fs';

	import { ErrorToast, Pizza, ProgressModal, SuccessToast } from '@ethos/core';

	import { appWindow } from '@tauri-apps/api/window';
	import { fs } from '@tauri-apps/api';
	import { page } from '$app/stores';
	import {
		commits,
		appConfig,
		repoStatus,
		updateDismissed,
		allModifiedFiles,
		locks,
		changeSets
	} from '$lib/stores';
	import PreferencesModal from '$lib/components/preferences/PreferencesModal.svelte';
	import { getLatestVersion, getLogPath, restart, runUpdate } from '$lib/system';
	import WelcomeModal from '$lib/components/oobe/WelcomeModal.svelte';
	import { getAppConfig } from '$lib/config';
	import { getAllCommits, getRepoStatus, verifyLocks } from '$lib/repo';
	import { CHANGE_SETS_PATH } from '$lib/consts';

	// Initialization
	let appVersion = '';
	let initialized = false;
	let initializationLog = '';
	let startupMessage = 'Initializing Birdie';

	// Welcome Modal
	let showWelcomeModal = false;

	// Preferences Modal
	let showPreferencesModal = false;
	let showProgressModal = false;
	let preferencesModalRequestInFlight = false;

	// Update available
	let updating = false;
	let latest = '';
	let updateAvailable = false;

	const spanClass = 'flex-1 ml-3 whitespace-nowrap';
	const sidebarSubItemClass = 'my-1 pl-8 text-sm dark:text-primary-400 dark:hover:bg-secondary-700';
	const sidebarSubItemInactiveClass =
		'flex items-center justify-between my-1 px-2 py-1 text-base font-normal rounded-lg dark:text-primary-400 dark:hover:bg-secondary-700';
	const sidebarSubItemActiveClass =
		'flex items-center justify-between my-1 px-2 py-1 text-base font-normal dark:bg-secondary-700 rounded-lg dark:text-primary-400 dark:hover:bg-secondary-700';

	$: activeUrl = $page.url.pathname;

	const refreshInterval = 30 * 1000;

	let loading = false;
	const loadingText = 'Refreshing data...';

	// error states
	let hasError = false;
	let errorMessage = '';
	const onErrorDismissed = () => {
		hasError = false;
		errorMessage = '';
	};

	// success states
	let hasSuccess = false;
	let successMessage = '';
	const onSuccessDismissed = () => {
		hasSuccess = false;
		successMessage = '';
	};

	const handleCheckForUpdates = async () => {
		latest = await getLatestVersion();
		updateAvailable = semver.gt(latest, await getVersion());

		if (updateAvailable) {
			showPreferencesModal = false;
			updateDismissed.set(false);
		}
	};

	const handleUpdateClicked = async () => {
		updating = true;

		try {
			await runUpdate();
			updateAvailable = false;

			await restart();
		} catch (e) {
			await emit('error', e);
		}

		updating = false;
	};

	const initializeChangeSets = async () => {
		if (await fs.exists(CHANGE_SETS_PATH, { dir: fs.BaseDirectory.LocalData })) {
			const changeSetsResponse = await fs.readTextFile(CHANGE_SETS_PATH, {
				dir: fs.BaseDirectory.LocalData
			});
			changeSets.set(JSON.parse(changeSetsResponse));
		}
	};

	/* eslint-disable no-await-in-loop */
	const initialize = async () => {
		appVersion = await getVersion();
		for (;;) {
			try {
				await invoke('get_system_status');
				break;
			} catch (_) {
				const path = await getLogPath();

				// get current YYYY-MM-DD in UTC
				const date = new Date();
				const year = date.getUTCFullYear();

				// get month as 2 digits
				const month = (1 + date.getUTCMonth()).toString().padStart(2, '0');

				// get day as 2 digits
				const day = date.getUTCDate().toString().padStart(2, '0');

				// get hour as 2 digits
				const hour = date.getUTCHours().toString().padStart(2, '0');

				try {
					const log = await readTextFile(
						`${path}/Birdie-${appVersion}.${year}-${month}-${day}-${hour}.log`
					);
					// set initialization log to last 50 lines of log
					initializationLog = log.split('\n').slice(-50).join('\n');
				} catch (e) {
					await emit('error', e);
				}
			}

			// wait one second
			await new Promise((resolve) => {
				setTimeout(resolve, 1000);
			});
		}

		try {
			const config = await getAppConfig();
			appConfig.set(config);
			if (config.repoPath !== '') {
				repoStatus.set(await getRepoStatus());
				commits.set(await getAllCommits());
				locks.set(await verifyLocks());

				await initializeChangeSets();
			}
		} catch (e) {
			await emit('error', e);
		}

		initialized = true;
	};

	onMount(() => {
		// show app window
		const setupAppWindow = async (): Promise<void> => {
			await appWindow.show();
		};
		void setupAppWindow();

		const refresh = async () => {
			loading = true;

			try {
				latest = await getLatestVersion();

				updateAvailable = semver.gt(latest, await getVersion());

				$locks = await verifyLocks();
				$repoStatus = await getRepoStatus();
			} catch (e) {
				await emit('error', e);
			}

			loading = false;
		};

		initialize()
			.then(() => {
				void refresh();

				const interval = setInterval(refresh, refreshInterval);

				showWelcomeModal = !get(appConfig).initialized;

				return () => {
					clearInterval(interval);
				};
			})
			.catch((e) => {
				if (e instanceof Error) {
					void emit('error', e);
				}
			});
	});

	void listen('error', (e) => {
		const error = e.payload as Error;
		hasError = true;
		errorMessage = String(error.message);
	});

	void listen('success', (e) => {
		successMessage = e.payload as string;
		hasSuccess = true;
	});

	void listen('startup-message', (e) => {
		startupMessage = e.payload as string;
	});

	void listen('open-preferences', () => {
		showPreferencesModal = true;
	});

	let hidePizza = true;

	const toggleVersionText = () => {
		hidePizza = !hidePizza;
	};
</script>

<WelcomeModal bind:showModal={showWelcomeModal} onClose={() => emit('refresh-files')} />

<PreferencesModal
	bind:showModal={showPreferencesModal}
	bind:requestInFlight={preferencesModalRequestInFlight}
	bind:showProgressModal
	{handleCheckForUpdates}
/>

<div class="flex flex-col h-screen w-screen border border-primary-900 overflow-hidden rounded-md">
	<div
		class="flex justify-between items-center gap-1 w-full h-8 bg-secondary-800 dark:bg-space-950 border-b border-opacity-50 border-dotted border-primary-500"
		data-tauri-drag-region
	>
		<div class="pl-2 flex gap-2 items-center pointer-events-none">
			<Img imgClass="w-5 h-5" src="/assets/icon.png" /><span class="text-gray-300">birdie</span>
		</div>
		<div class="pr-2 flex gap-2 justify-end">
			<Button
				outline
				color="dark"
				size="xs"
				class="p-1 my-1 hover:bg-secondary-800 text-gray-400 dark:hover:bg-space-950 border-0 focus-within:ring-0 dark:focus-within:ring-0 focus-within:bg-secondary-800 dark:focus-within:bg-space-950"
				on:click={async () => {
					await appWindow.minimize();
				}}><MinusOutline class="h-4 w-4" /></Button
			>
			<Button
				outline
				color="dark"
				size="xs"
				class="p-1 my-1 hover:bg-secondary-800 text-gray-400 dark:hover:bg-space-950 border-0 focus-within:ring-0 dark:focus-within:ring-0 focus-within:bg-secondary-800 dark:focus-within:bg-space-950"
				on:click={async () => {
					await appWindow.toggleMaximize();
				}}><WindowOutline class="h-4 w-4" /></Button
			>
			<Button
				outline
				color="dark"
				size="xs"
				class="p-1 my-1 hover:bg-secondary-800 text-gray-400 dark:hover:bg-space-950 border-0 focus-within:ring-0 dark:focus-within:ring-0 focus-within:bg-secondary-800 dark:focus-within:bg-space-950"
				on:click={async () => {
					await appWindow.hide();
				}}><CloseSolid class="h-4 w-4" /></Button
			>
		</div>
	</div>
	{#if !initialized}
		<div
			class="flex flex-col gap-2 px-12 dark:bg-secondary-800 items-center w-screen h-full justify-center"
		>
			<div class="flex items-center gap-2">
				<span class="text-gray-300 text-xl">{startupMessage}...</span>
				<Spinner size="4" />
			</div>
			{#if initializationLog !== ''}
				<Accordion
					activeClass="dark:hover:bg-secondary-700 focus:ring-0 dark:border-gray-300 text-white overflow-auto py-2"
					inactiveClass="dark:hover:bg-secondary-700 dark:border-gray-300 text-white py-2"
					class="w-full dark:border-gray-300"
				>
					<AccordionItem class="w-full" borderSharedClass="dark:border-gray-300">
						<div slot="header" class="flex items-center justify-between text-center w-full pr-2">
							<span class="text-xs text-gray-300 font-mono w-full"
								>Click here to see startup logs</span
							>
						</div>
						<div
							class="bg-secondary-800 text-gray-300 font-mono w-full max-h-[60vh] overflow-y-auto p-2"
						>
							<code class="whitespace-pre-wrap truncate">{initializationLog}</code>
						</div>
					</AccordionItem>
				</Accordion>
			{/if}
		</div>
	{:else}
		<div class="flex dark:bg-secondary-700 h-full overflow-y-hidden w-full overflow-x-hidden">
			<Sidebar
				asideClass="w-56 shadow-md sticky top-0 h-full"
				activeClass="flex items-center p-2 text-base font-normal text-gray-900 dark:bg-secondary-700 rounded-lg dark:text-primary-400 dark:hover:bg-secondary-700"
				nonActiveClass="flex items-center p-2 text-base font-normal rounded-lg dark:text-primary-400 dark:hover:bg-secondary-700"
				{activeUrl}
			>
				<SidebarWrapper class="h-full rounded-none dark:bg-secondary-800">
					<SidebarGroup>
						<SidebarItem
							class="group/item"
							label="Home"
							href="/"
							active={activeUrl === '/'}
							{spanClass}
						>
							<svelte:fragment slot="icon">
								<HomeSolid
									class="w-5 h-5 transition duration-75 dark:text-gray-400 dark:group-hover/item:text-white"
								/>
							</svelte:fragment>
						</SidebarItem>
						{#if $appConfig.repoPath !== ''}
							<SidebarDropdownWrapper
								label="Source"
								class="group/item dark:text-primary-400 dark:hover:bg-secondary-700 rounded-lg"
								ulClass="py-1"
							>
								<svelte:fragment slot="icon">
									<CodeBranchSolid
										class="w-5 h-5 transition duration-75 dark:text-gray-400 dark:group-hover/item:text-white"
									/>
								</svelte:fragment>
								<SidebarItem
									label="Submit"
									activeClass={sidebarSubItemActiveClass}
									nonActiveClass={sidebarSubItemInactiveClass}
									spanClass={sidebarSubItemClass}
									href="/source/submit"
									active={activeUrl === '/source/submit'}
								>
									<svelte:fragment slot="subtext">
										<span
											class="items-center px-2 ms-3 text-sm font-medium text-white rounded-full {$allModifiedFiles.length >
											0
												? 'bg-primary-600 dark:bg-primary-600'
												: 'bg-gray-500 dark:bg-gray-500'}"
										>
											{$allModifiedFiles.length}
										</span>
									</svelte:fragment>
								</SidebarItem>
								<SidebarItem
									label="Commits"
									activeClass={sidebarSubItemActiveClass}
									nonActiveClass={sidebarSubItemInactiveClass}
									spanClass={sidebarSubItemClass}
									href="/source/history"
									active={activeUrl === '/source/history'}
								/>
								<SidebarItem
									label="Locks"
									activeClass={sidebarSubItemActiveClass}
									nonActiveClass={sidebarSubItemInactiveClass}
									spanClass={sidebarSubItemClass}
									href="/source/locks"
									active={activeUrl === '/source/locks'}
								>
									<svelte:fragment slot="subtext">
										<span
											class="items-center px-2 ms-3 text-sm font-medium text-white rounded-full {$locks
												.ours.length > 0
												? 'bg-primary-600 dark:bg-primary-600'
												: 'bg-gray-500 dark:bg-gray-500'}"
										>
											{$locks.ours.length}
										</span>
									</svelte:fragment>
								</SidebarItem>
								<SidebarItem
									label="Diagnostics"
									activeClass={sidebarSubItemActiveClass}
									nonActiveClass={sidebarSubItemInactiveClass}
									spanClass={sidebarSubItemClass}
									href="/source/diagnostics"
									active={activeUrl === '/source/diagnostics'}
								/>
							</SidebarDropdownWrapper>
						{/if}
						<SidebarDropdownWrapper
							label="System"
							class="group/item dark:text-primary-400 dark:hover:bg-secondary-700 rounded-lg"
							ulClass="py-1"
						>
							<svelte:fragment slot="icon">
								<ComputerSpeakerSolid
									class="w-5 h-5 transition duration-75 dark:text-gray-400 dark:group-hover/item:text-white"
								/>
							</svelte:fragment>
							<SidebarDropdownItem
								label="Logs"
								activeClass={sidebarSubItemActiveClass}
								class={sidebarSubItemClass}
								href="/system/logs"
								active={activeUrl === '/system/logs'}
							/>
						</SidebarDropdownWrapper>
					</SidebarGroup>
					<div class="top-[100vh] sticky">
						<div class="h-4 w-full mt-2">
							{#if loading}
								<div class="flex items-center justify-center h-full w-full gap-2">
									<Spinner size="4" />
									<span class="text-gray-400 text-xs">{loadingText}</span>
								</div>
							{/if}
						</div>
						<div class="flex flex-col">
							<div class="flex mt-2">
								<Button
									class="!p-2 active:border-none focus:outline-none"
									label="Preferences"
									on:click={() => {
										showPreferencesModal = true;
									}}
									bind:disabled={preferencesModalRequestInFlight}
								>
									{#if preferencesModalRequestInFlight}
										<Spinner class="h-6 w-6 border-none" />
									{:else}
										<CogOutline class="h-6 w-6 border-none" />
									{/if}
								</Button>
								<Button
									outline
									class="mb-1 p-0 w-full dark:hover:bg-secondary-800 dark:hover:text-primary-500 dark:active:border-none dark:focus:ring-0 border-none dark:text-primary-500 text-center text-sm"
									on:click={toggleVersionText}
									>{(hidePizza && `v${appVersion}`) || 'Have a piece of pizza!'}
								</Button>
							</div>
							<div
								class="w-full h-24 bg-black border rounded dark:border-primary-500 hover:cursor-grab active:cursor-grabbing mt-2"
								class:hidePizza
							>
								<Canvas>
									<Pizza />
								</Canvas>
							</div>
						</div>
					</div>
				</SidebarWrapper>
			</Sidebar>

			<div class="px-4 mx-auto w-full h-full overflow-hidden">
				<main class="w-full h-full flex flex-col pb-2 overflow-hidden">
					<slot class="overflow-hidden" />
				</main>
			</div>
		</div>
	{/if}
</div>
<ErrorToast bind:show={hasError} {errorMessage} onClose={onErrorDismissed} />
<SuccessToast bind:show={hasSuccess} message={successMessage} onClose={onSuccessDismissed} />
<ProgressModal bind:showModal={showProgressModal} title="Cloning new repo" />

<!-- Update modal, not quite worthy of a component yet -->
<Modal
	open={updateAvailable && !$updateDismissed}
	size="sm"
	defaultClass="dark:bg-secondary-800 overflow-y-auto"
	bodyClass="!border-t-0"
	backdropClass="fixed mt-8 inset-0 z-40 bg-gray-900 bg-opacity-50 dark:bg-opacity-80"
	dialogClass="fixed mt-8 top-0 start-0 end-0 h-modal md:inset-0 md:h-full z-50 w-full p-4 pb-12 flex"
	on:close={() => {
		updateDismissed.set(true);
	}}
>
	<div class="flex items-center justify-between pr-8">
		<div class="text-white">
			Birdie <span class="font-mono text-primary-400">v{latest}</span> is available!
		</div>
		<Button disabled={updating} color="green" class="flex gap-2" on:click={handleUpdateClicked}
			>Upgrade
			{#if updating}
				<Spinner color="white" class="h-4 w-4 border-none" />
			{/if}
		</Button>
	</div>
</Modal>

<style>
	.hidePizza {
		display: none;
	}

	:global(::-webkit-scrollbar) {
		width: 5px;
		height: 5px;
	}

	:global(::-webkit-scrollbar-track) {
		background: theme('colors.secondary.700');
	}

	:global(::-webkit-scrollbar-thumb) {
		background: theme('colors.primary.500');
		border-radius: 10px;
	}

	:global(::-webkit-scrollbar-corner) {
		background: theme('colors.secondary.700');
	}
</style>
