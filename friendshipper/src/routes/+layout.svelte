<script lang="ts">
	import '../app.postcss';
	import { onMount } from 'svelte';
	import {
		Button,
		Card,
		DarkMode,
		Modal,
		Sidebar,
		SidebarDropdownWrapper,
		SidebarGroup,
		SidebarItem,
		SidebarWrapper,
		Spinner
	} from 'flowbite-svelte';
	import {
		BuildingSolid,
		ChevronDownSolid,
		ChevronUpSolid,
		CodeBranchSolid,
		CogOutline,
		ComputerSpeakerSolid,
		DatabaseSolid,
		HomeSolid,
		UserSolid
	} from 'flowbite-svelte-icons';
	import { emit, listen } from '@tauri-apps/api/event';
	import { Canvas } from '@threlte/core';
	import { get } from 'svelte/store';
	import semver from 'semver';
	import { getVersion } from '@tauri-apps/api/app';
	import { invoke } from '@tauri-apps/api/tauri';

	import { ErrorToast, Pizza, ProgressModal, SuccessToast } from '@ethos/core';

	import { goto } from '$app/navigation';
	import { page } from '$app/stores';
	import {
		allModifiedFiles,
		appConfig,
		builds,
		commits,
		dynamicConfig,
		engineWorkflows,
		locks,
		onboardingInProgress,
		playtests,
		projectConfigs,
		repoConfig,
		repoStatus,
		selectedCommit,
		updateDismissed,
		workflows
	} from '$lib/stores';
	import { getPlaytests } from '$lib/playtests';
	import { getBuilds, getWorkflows } from '$lib/builds';
	import { checkLoginRequired, refreshLogin } from '$lib/auth';
	import QuickLaunchModal from '$lib/components/servers/QuickLaunchModal.svelte';
	import PreferencesModal from '$lib/components/preferences/PreferencesModal.svelte';
	import { getAllCommits, getRepoStatus, verifyLocks } from '$lib/repo';
	import { getLatestVersion, openSystemLogsFolder, restart, runUpdate } from '$lib/system';
	import WelcomeModal from '$lib/components/oobe/WelcomeModal.svelte';
	import { getAppConfig, getDynamicConfig, getProjectConfig, getRepoConfig } from '$lib/config';

	// Initialization
	let appVersion = '';
	let initialized = false;
	let loginRequired = true;
	let loginPrompted = false;
	let loadingBuilds = false;
	let startupMessage = 'Initializing Friendshipper';

	// Quick launch stuff
	let quickLaunching = false;
	let quickLaunchServerName = '';

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

	$: conflictsDetected = $repoStatus?.conflicts && $repoStatus?.conflicts.length > 0;

	const spanClass = 'flex-1 ml-3 whitespace-nowrap';
	const sidebarSubItemClass = 'mx-2 my-1 text-sm text-primary-400 dark:text-primary-400';
	const sidebarSubItemInactiveClass =
		'flex items-center justify-between mx-2 my-1 px-2 py-1 text-base font-normal rounded-lg text-primary-400 dark:text-primary-400 bg-secondary-800 dark:bg-space-950 hover:bg-secondary-700 dark:hover:bg-space-900';
	const sidebarSubItemActiveClass =
		'flex items-center justify-between mx-2 my-1 px-2 py-1 text-base font-normal bg-secondary-700 dark:bg-space-900 rounded-lg text-primary-400 dark:text-primary-400 hover:bg-secondary-700 dark:hover:bg-space-900';

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

	/* eslint-disable no-await-in-loop */
	const initialize = async () => {
		initialized = false;
		appVersion = await getVersion();
		for (;;) {
			try {
				await invoke('get_system_status');

				break;
			} catch (_) {
				// wait one second
				await new Promise((resolve) => {
					setTimeout(resolve, 1000);
				});
			}
		}
		try {
			const config = await getAppConfig();
			appConfig.set(config);

			// if the config isn't initialized, we want to push the user
			// to the welcome modal
			if (!config.initialized) {
				return;
			}
		} catch (e) {
			if (e instanceof Error) {
				await emit('error', e);
			}
		}

		loginRequired = await checkLoginRequired();
		if (loginRequired && !loginPrompted) {
			setTimeout(() => {
				loginPrompted = true;
			}, 3000);

			try {
				await refreshLogin();
				await initialize();
			} catch (e) {
				await emit('error', e);
			}
		}

		if (!loginRequired) {
			try {
				const [dynamicConfigResponse, projectConfigResponse, buildsResponse, locksResponse] =
					await Promise.all([
						getDynamicConfig(),
						getProjectConfig(),
						getBuilds(250),
						verifyLocks()
					]);

				projectConfigs.set(projectConfigResponse);
				dynamicConfig.set(dynamicConfigResponse);
				builds.set(buildsResponse);
				locks.set(locksResponse);

				if ($builds.entries && $builds.entries.length > 0) {
					selectedCommit.set($builds.entries[0]);
				} else {
					selectedCommit.set(null);
				}

				if ($appConfig.repoPath !== '') {
					const [repoConfigResponse, repoStatusResponse, commitsResponse] = await Promise.all([
						getRepoConfig(),
						getRepoStatus(),
						getAllCommits()
					]);

					repoConfig.set(repoConfigResponse);
					repoStatus.set(repoStatusResponse);
					commits.set(commitsResponse);
				}

				loadingBuilds = true;

				getWorkflows()
					.then((response) => {
						workflows.set(response.commits);
						loadingBuilds = false;
					})
					.catch((e) => {
						if (e instanceof Error) {
							void emit('error', e);
						}
					});
			} catch (e) {
				await emit('error', e);
			}

			if ($appConfig.engineRepoUrl !== '') {
				getWorkflows(true)
					.then((response) => {
						engineWorkflows.set(response.commits);
					})
					.catch((e) => {
						if (e instanceof Error) {
							void emit('error', e);
						}
					});
			}
		}

		initialized = true;
	};

	onMount(() => {
		// show app window
		const setupAppWindow = async (): Promise<void> => {
			const { appWindow } = await import('@tauri-apps/api/window');
			void appWindow.show();
		};

		void setupAppWindow();

		const refresh = async () => {
			if (!$appConfig.initialized || $onboardingInProgress || loginRequired) return;

			const buildsPromise = getBuilds(250);
			const playtestsPromise = getPlaytests();

			loading = true;

			const selected = get(selectedCommit);

			// There's some backend ID nonsense happening - when we refresh the builds, even if there are a bunch
			// of builds in the list that are the same, they're "different" from Svelte's perspective, so we need
			// to make sure our selected commit is still valid.
			try {
				const updatedBuilds = await buildsPromise;
				if (selected) {
					const found = updatedBuilds.entries.find((build) => build.commit === selected.commit);
					if (found) {
						selectedCommit.set(found);
					}
				}

				builds.set(updatedBuilds);
			} catch (e) {
				await emit('error', e);
			}

			playtests.set(await playtestsPromise);

			if ($appConfig.repoPath !== '') {
				try {
					commits.set(await getAllCommits());
					locks.set(await verifyLocks());
					repoStatus.set(await getRepoStatus());
				} catch (e) {
					await emit('error', e);
				}
			}

			try {
				latest = await getLatestVersion();
				updateAvailable = semver.gt(latest, await getVersion());
			} catch (e) {
				await emit('error', e);
			}

			loading = false;
		};

		const checkAuth = async () => {
			if (!$appConfig.initialized || $onboardingInProgress) return;

			loginRequired = await checkLoginRequired();
		};

		initialize()
			.then(() => {
				void refresh();

				const interval = setInterval(() => {
					void refresh();
				}, refreshInterval);
				const authInterval = setInterval(() => {
					void checkAuth();
				}, refreshInterval);

				showWelcomeModal = !get(appConfig).initialized;

				return () => {
					clearInterval(interval);
					clearInterval(authInterval);
				};
			})
			.catch((e) => {
				if (e instanceof Error) {
					void emit('error', e);
				}
			});
	});

	void listen('error', (e) => {
		hasError = true;
		const error = e.payload as Error;
		if (error.message) {
			errorMessage = error.message;
		} else {
			errorMessage = JSON.stringify(e.payload);
		}
	});

	void listen('success', (e) => {
		successMessage = e.payload as string;
		hasSuccess = true;
	});

	void listen('startup-message', (e) => {
		startupMessage = e.payload as string;
	});

	void listen('login-status', (e) => {
		loginRequired = Boolean(e.payload);
	});

	void listen('open-preferences', () => {
		showPreferencesModal = true;
	});

	void listen('scheme-request-received', (e) => {
		const payload = String(e.payload).split('friendshipper://')[1].replace(/\/$/, '');

		if (payload.startsWith('launch/')) {
			quickLaunching = true;

			// This destructuring syntax is so awful but standard linters seem
			// to prefer it.
			[, quickLaunchServerName] = payload.split('launch/');

			void goto('/');

			return;
		}

		if (payload === 'home') {
			void goto('/');
		} else if (payload === 'playtests') {
			void goto('/playtests');
		}
	});

	let hidePizza = true;

	const toggleVersionText = () => {
		hidePizza = !hidePizza;
	};
</script>

<WelcomeModal bind:showModal={showWelcomeModal} onClose={initialize} />

<PreferencesModal
	bind:showModal={showPreferencesModal}
	bind:requestInFlight={preferencesModalRequestInFlight}
	bind:showProgressModal
	{handleCheckForUpdates}
/>

{#if !initialized}
	{#if loginRequired && loginPrompted}
		<div
			class="flex flex-col p-4 align-middle justify-around h-screen w-full bg-secondary-800 dark:bg-space-950"
		>
			<Card
				class="w-full p-4 sm:p-4 max-w-full flex flex-col align-middle gap-2 bg-secondary-700 dark:bg-space-900 border-0 shadow-none"
			>
				<img src="/assets/7B_5.png" alt="Friendshipper Logo" class="w-24 h-24 mx-auto" />
				<h3 class="text-xl text-white text-center">Log in to Friendshipper!</h3>
				<div class="flex flex-row justify-around align-middle">
					<Button
						class="w-[300px]"
						on:click={async () => {
							try {
								await refreshLogin();
								await initialize();

								loginPrompted = false;
							} catch (e) {
								await emit('error', e);
							}
						}}
						>Log In
					</Button>
				</div>
			</Card>
		</div>
	{:else}
		<div
			class="flex flex-col gap-2 px-12 bg-secondary-700 dark:bg-space-900 items-center w-screen h-screen justify-center"
		>
			<div class="flex items-center gap-2">
				<span class="text-gray-300 text-xl">{startupMessage}...</span>
				<Spinner size="4" />
			</div>
			<Button on:click={openSystemLogsFolder}>Open Logs Folder</Button>
		</div>
	{/if}
{:else}
	<div
		class="flex bg-secondary-800 dark:bg-space-950 h-screen overflow-y-hidden w-full overflow-x-hidden"
	>
		<QuickLaunchModal bind:showModal={quickLaunching} serverName={quickLaunchServerName} />
		<Sidebar
			asideClass="w-56 shadow-md sticky top-0 h-screen"
			activeClass="flex items-center p-2 text-base font-normal text-gray-900 bg-secondary-800 dark:bg-space-950 rounded-lg text-primary-400 dark:text-primary-400 hover:bg-secondary-800 dark:hover:bg-space-950"
			nonActiveClass="flex items-center p-2 text-base font-normal rounded-lg text-primary-400 dark:text-primary-400 hover:bg-secondary-800 dark:hover:bg-space-950"
			{activeUrl}
		>
			<SidebarWrapper class="h-full rounded-none bg-secondary-700 dark:bg-space-900">
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
								class="w-5 h-5 transition duration-75 text-gray-400 dark:text-gray-400 group-hover/item:text-white dark:group-hover/item:text-white"
							/>
						</svelte:fragment>
					</SidebarItem>
					<SidebarItem
						class="group/item"
						label="Playtests"
						href="/playtests"
						active={activeUrl === '/playtests'}
						{spanClass}
					>
						<svelte:fragment slot="icon">
							<UserSolid
								class="w-5 h-5 transition duration-75 text-gray-400 dark:text-gray-400 group-hover/item:text-white dark:group-hover/item:text-white"
							/>
						</svelte:fragment>
					</SidebarItem>
					{#if loadingBuilds}
						<Button
							class="flex gap-3 w-full p-2 justify-start hover:bg-secondary-700 dark:hover:bg-space-900 bg-secondary-700 dark:bg-space-950"
							disabled
						>
							<Spinner class="w-5 h-5 border-none" />
							<span class="font-normal text-base text-gray-400 dark:text-gray-300">Builds</span>
						</Button>
					{:else}
						<SidebarItem
							class="group/item"
							label="Builds"
							href="/builds"
							active={activeUrl === '/builds'}
							{spanClass}
						>
							<svelte:fragment slot="icon">
								{#if loadingBuilds}
									<Spinner class="w-5 h-5 border-none" />
								{:else}
									<BuildingSolid
										class="w-5 h-5 transition duration-75 text-gray-400 dark:text-gray-400 group-hover/item:text-white dark:group-hover/item:text-white"
									/>
								{/if}
							</svelte:fragment>
						</SidebarItem>
					{/if}
					{#if $appConfig.repoPath !== ''}
						<SidebarDropdownWrapper
							label="Source"
							class="group/item text-primary-400 dark:text-primary-400 hover:bg-secondary-800 dark:hover:bg-space-950 rounded-lg"
							ulClass="my-2 rounded-lg py-1 bg-secondary-800 dark:bg-space-950"
						>
							<svelte:fragment slot="icon">
								<CodeBranchSolid
									class="w-5 h-5 transition duration-75 text-gray-400 dark:text-gray-400 group-hover/item:text-white dark:group-hover/item:text-white"
								/>
							</svelte:fragment>
							<svelte:fragment slot="arrowup">
								<ChevronUpSolid class="h-3 w-3 text-white" />
							</svelte:fragment>
							<svelte:fragment slot="arrowdown">
								<ChevronDownSolid class="h-3 w-3 text-white" />
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
									<div class="flex w-full gap-1 pl-2 ms-3 items-center justify-end">
										<span
											class="items-center px-2 text-sm font-medium text-white rounded-full {$allModifiedFiles.length >
											0
												? 'bg-primary-600 dark:bg-primary-600'
												: 'bg-gray-500 dark:bg-gray-500'}"
										>
											{$allModifiedFiles.length}
										</span>
										{#if conflictsDetected}
											<span
												class="items-center px-2 text-sm font-medium text-white rounded-full {$allModifiedFiles.length >
												0
													? 'bg-red-700 dark:bg-red-700'
													: 'bg-gray-500 dark:bg-gray-500'}"
											>
												{$repoStatus?.conflicts.length}
											</span>
										{/if}
									</div>
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
					{#if $dynamicConfig.ludosEnabled && $appConfig.ludosShowUI}
						<SidebarDropdownWrapper
							label="Ludos"
							class="group/item text-primary-400 dark:text-primary-400 hover:bg-secondary-800 dark:hover:bg-space-950 rounded-lg"
							ulClass="my-2 rounded-lg py-1 bg-secondary-800 dark:bg-space-950"
						>
							<svelte:fragment slot="icon">
								<DatabaseSolid
									class="w-5 h-5 transition duration-75 text-gray-400 dark:text-gray-400 group-hover/item:text-white dark:group-hover/item:text-white"
								/>
							</svelte:fragment>
							<svelte:fragment slot="arrowup">
								<ChevronUpSolid class="h-3 w-3 text-white" />
							</svelte:fragment>
							<svelte:fragment slot="arrowdown">
								<ChevronDownSolid class="h-3 w-3 text-white" />
							</svelte:fragment>
							<SidebarItem
								label="Storage"
								activeClass={sidebarSubItemActiveClass}
								nonActiveClass={sidebarSubItemInactiveClass}
								class={sidebarSubItemClass}
								href="/ludos/objects"
								active={activeUrl === '/ludos/objects'}
							/>
						</SidebarDropdownWrapper>
					{/if}
					<SidebarDropdownWrapper
						label="System"
						class="group/item text-primary-400 dark:text-primary-400 hover:bg-secondary-800 dark:hover:bg-space-950 rounded-lg"
						ulClass="my-2 rounded-lg py-1 bg-secondary-800 dark:bg-space-950"
					>
						<svelte:fragment slot="icon">
							<ComputerSpeakerSolid
								class="w-5 h-5 transition duration-75 text-gray-400 dark:text-gray-400 group-hover/item:text-white dark:group-hover/item:text-white"
							/>
						</svelte:fragment>
						<svelte:fragment slot="arrowup">
							<ChevronUpSolid class="h-3 w-3 text-white" />
						</svelte:fragment>
						<svelte:fragment slot="arrowdown">
							<ChevronDownSolid class="h-3 w-3 text-white" />
						</svelte:fragment>
						<SidebarItem
							label="Logs"
							activeClass={sidebarSubItemActiveClass}
							nonActiveClass={sidebarSubItemInactiveClass}
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
								class="mb-1 p-0 w-full hover:bg-secondary-700 dark:hover:bg-space-900 hover:text-primary-500 dark:hover:text-primary-500 active:border-none dark:active:border-none focus:ring-0 dark:focus:ring-0 border-none text-primary-500 dark:text-primary-500 text-center text-sm"
								on:click={toggleVersionText}
								>{(hidePizza && `v${appVersion}`) || 'Have a piece of pizza!'}
							</Button>
						</div>
						<div
							class="w-full h-24 bg-black border rounded border-primary-500 dark:border-primary-500 hover:cursor-grab active:cursor-grabbing mt-2"
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
<ErrorToast bind:show={hasError} {errorMessage} onClose={onErrorDismissed} />
<SuccessToast bind:show={hasSuccess} message={successMessage} onClose={onSuccessDismissed} />
<ProgressModal bind:showModal={showProgressModal} title="Cloning new repo" />
<!-- Hidden dark mode toggle allows us to load the theme immediately, even though the actual toggle is in the preferences modal -->
<DarkMode class="hidden" />

<!-- Update modal, not quite worthy of a component yet -->
<Modal
	open={updateAvailable && !$updateDismissed}
	size="sm"
	defaultClass="bg-secondary-700 dark:bg-space-900 overflow-y-auto"
	bodyClass="!border-t-0"
	on:close={() => {
		updateDismissed.set(true);
	}}
>
	<div class="flex items-center justify-between pr-8">
		<div class="text-white">
			Friendshipper <span class="font-mono text-primary-400">v{latest}</span> is available!
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
