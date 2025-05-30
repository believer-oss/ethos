<script lang="ts">
	import '../app.postcss';
	import { onMount } from 'svelte';
	import {
		Button,
		DarkMode,
		Img,
		Modal,
		Progressbar,
		Sidebar,
		SidebarDropdownWrapper,
		SidebarGroup,
		SidebarItem,
		SidebarWrapper,
		Spinner
	} from 'flowbite-svelte';
	import {
		BuildingSolid,
		ChevronDownOutline,
		ChevronUpOutline,
		CloseOutline,
		CodeBranchSolid,
		CogOutline,
		ComputerSpeakerSolid,
		HomeSolid,
		MinusOutline,
		UserSolid,
		WindowOutline,
		ServerOutline
	} from 'flowbite-svelte-icons';
	import { emit, listen } from '@tauri-apps/api/event';
	import { Canvas } from '@threlte/core';
	import { get } from 'svelte/store';
	import { getVersion } from '@tauri-apps/api/app';
	import { type } from '@tauri-apps/plugin-os';
	import { invoke } from '@tauri-apps/api/core';

	import { ErrorToast, Pizza, ProgressModal, SuccessToast } from '@ethos/core';
	import { getCurrentWebviewWindow } from '@tauri-apps/api/webviewWindow';
	import { check, type DownloadEvent } from '@tauri-apps/plugin-updater';
	import { jwtDecode } from 'jwt-decode';
	import { relaunch } from '@tauri-apps/plugin-process';
	import { goto } from '$app/navigation';
	import { page } from '$app/stores';
	import {
		activeProjectConfig,
		allModifiedFiles,
		appConfig,
		backgroundSyncInProgress,
		builds,
		changeSets,
		commits,
		dynamicConfig,
		engineWorkflows,
		oktaAuth,
		onboardingInProgress,
		playtests,
		projectConfigs,
		repoConfig,
		repoStatus,
		selectedCommit,
		showPreferences,
		updateDismissed,
		workflows
	} from '$lib/stores';
	import { getPlaytests } from '$lib/playtests';
	import { cancelDownload, getBuilds, getWorkflows } from '$lib/builds';
	import { refreshLogin } from '$lib/auth';
	import QuickLaunchModal from '$lib/components/servers/QuickLaunchModal.svelte';
	import PreferencesModal from '$lib/components/preferences/PreferencesModal.svelte';
	import {
		getAllCommits,
		getRepoStatus,
		SkipDllCheck,
		AllowOfflineCommunication,
		loadChangeSet
	} from '$lib/repo';
	import { openSystemLogsFolder, shutdownServer } from '$lib/system';
	import WelcomeModal from '$lib/components/oobe/WelcomeModal.svelte';
	import { getAppConfig, getDynamicConfig, getProjectConfig, getRepoConfig } from '$lib/config';
	import { handleError } from '$lib/utils';
	import { createOktaAuth, isTokenExpired } from '$lib/okta';
	import { browser } from '$app/environment';

	const appWindow = getCurrentWebviewWindow();

	// Initialization
	let appVersion = '';
	let initialized = false;
	let loadingBuilds = false;
	let startupMessage = 'Initializing Friendshipper';
	let gitStartupMessage = '';

	// Refresh timer
	let lastRefresh = new Date().getTime();

	// Quick launch stuff
	let quickLaunching = false;
	let quickLaunchServerName = '';

	// Welcome Modal
	let showWelcomeModal = false;

	// Preferences Modal
	let showProgressModal = false;
	let progressModalTitle: string = '';
	let preferencesModalRequestInFlight = false;

	// Update available
	let updating = false;
	let latest = '';
	let updateAvailable = false;
	let updateProgress = 0;

	// Background sync
	let backgroundSyncProgress = 0;
	let backgroundSyncElapsed = '';
	let backgroundSyncRemaining = '';

	const handleCancelBackgroundSync = async () => {
		try {
			await cancelDownload();

			backgroundSyncProgress = 0;
			backgroundSyncElapsed = '';
			backgroundSyncRemaining = '';

			await emit('background-sync-cancel');
		} catch (e) {
			await emit('error', e);
		}
	};

	$: conflictsDetected = $repoStatus?.conflicts && $repoStatus?.conflicts.length > 0;

	const spanClass = 'flex-1 ml-3 whitespace-nowrap';
	const sidebarSubItemClass = 'mx-2 my-1 text-sm text-primary-400 dark:text-primary-400';
	const sidebarSubItemInactiveClass =
		'flex items-center justify-between mx-2 my-1 px-2 py-1 text-base font-normal rounded-lg text-primary-400 dark:text-primary-400 bg-secondary-800 dark:bg-space-950 hover:bg-secondary-700 dark:hover:bg-space-900';
	const sidebarSubItemActiveClass =
		'flex items-center justify-between mx-2 my-1 px-2 py-1 text-base font-normal bg-secondary-700 dark:bg-space-900 rounded-lg text-primary-400 dark:text-primary-400 hover:bg-secondary-700 dark:hover:bg-space-900';

	$: activeUrl = $page.url.pathname;

	let tokenRefreshInterval: ReturnType<typeof setInterval> | undefined;
	let accessToken: string | null = null;
	let refreshToken: string | null = null;
	let showLogin: boolean = true;

	const refreshInterval = 60 * 1000;

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
		try {
			const update = await check();

			if (update !== null) {
				updateAvailable = true;
				latest = update?.version ?? '';

				$showPreferences = false;
				updateDismissed.set(false);
			}
		} catch (e) {
			await emit('error', e);
		}
	};

	const handleUpdateClicked = async () => {
		updating = true;

		try {
			let downloadSize = 0;
			let downloaded = 0;
			const update = await check();
			if (update) {
				await update.download((e: DownloadEvent) => {
					switch (e.event) {
						case 'Started':
							downloadSize = e.data.contentLength ?? 0;
							break;
						case 'Progress':
							downloaded += e.data.chunkLength;
							updateProgress = Math.round((downloaded / downloadSize) * 100);
							break;
						case 'Finished':
							updateProgress = 0;
							break;
						default:
							break;
					}
				});
				await shutdownServer();
				await update.install();

				await relaunch();

				updateAvailable = false;
			}
		} catch (e) {
			await emit('error', e);
		}

		updating = false;
	};

	const refreshRepo = async () => {
		repoStatus.set(await getRepoStatus());
		void emit('success', 'Files refreshed!');
	};

	const initializeChangeSets = async () => {
		if ($activeProjectConfig === null) {
			await emit('error', 'No active project found, unable to load changesets from file.');
			return;
		}

		$changeSets = await loadChangeSet();
	};

	const handleOktaLogout = async () => {
		try {
			localStorage.removeItem('oktaRefreshToken');
			localStorage.removeItem('oktaAccessToken');
			localStorage.clear();
			accessToken = null;
			refreshToken = null;
		} catch (err) {
			await emit('error', err);
		}
	};

	const tryOktaRefresh = async () => {
		if (!$oktaAuth) return;

		const { tokens } = await $oktaAuth.token.getWithoutPrompt({
			scopes: ['openid', 'email', 'profile']
		});

		if (tokens && tokens.accessToken) {
			$oktaAuth?.tokenManager.setTokens(tokens);

			await emit('access-token-set', tokens.accessToken.accessToken);
			await refreshLogin(tokens.accessToken.accessToken);

			if (tokens.refreshToken?.refreshToken) {
				localStorage.setItem('oktaRefreshToken', tokens.refreshToken?.refreshToken);
			}
		}
	};

	const handleOktaLogin = async () => {
		try {
			const previousStartupMessage = startupMessage;
			startupMessage = 'Logging in with Okta...';

			// Initiate the redirect flow
			if (browser && $oktaAuth) {
				const osType = type();

				if (osType === 'macos') {
					await $oktaAuth.token.getWithRedirect({
						issuer: $appConfig.oktaConfig.issuer,
						clientId: $appConfig.oktaConfig.clientId,
						redirectUri: `${window.location.origin}/auth/callback`,
						pkce: true,
						scopes: ['openid', 'email', 'profile']
					});
				} else {
					const { tokens } = await $oktaAuth.token.getWithPopup({
						scopes: ['openid', 'email', 'profile']
					});

					if (tokens && tokens.accessToken) {
						$oktaAuth.tokenManager.setTokens(tokens);

						await emit('access-token-set', tokens.accessToken.accessToken);
						if (tokens.refreshToken?.refreshToken) {
							localStorage.setItem('oktaRefreshToken', tokens.refreshToken?.refreshToken);
						}

						await refreshLogin(tokens.accessToken.accessToken);
					}
				}
			}

			startupMessage = previousStartupMessage;
		} catch (err) {
			await handleOktaLogout();
			await emit('error', err);
		}
	};

	const refreshOktaOrLogout = async () => {
		if (refreshToken && !isTokenExpired(refreshToken) && $oktaAuth) {
			try {
				await $oktaAuth.session.refresh();

				const tokens = await $oktaAuth.tokenManager.getTokens();

				await refreshLogin(tokens.accessToken?.accessToken);

				if (tokens.accessToken) {
					accessToken = tokens.accessToken.accessToken;
					localStorage.setItem('oktaAccessToken', accessToken);
				} else {
					await handleOktaLogout();
				}
			} catch (error) {
				await emit('error', error);
				await handleOktaLogout();
			}
		} else {
			try {
				await tryOktaRefresh();
			} catch (_) {
				try {
					await handleOktaLogout();
					await handleOktaLogin();
				} catch (e) {
					await emit('error', e);
				}
			}
		}
	};

	// If we have an access token, starts a background process that will attempt to refresh the token before it expires
	const startOktaTokenRefreshProcess = async () => {
		if (tokenRefreshInterval) clearTimeout(tokenRefreshInterval);

		if (accessToken) {
			const decodedToken: { exp: number } = jwtDecode(accessToken);
			const expirationTime = decodedToken.exp * 1000;
			const currentTime = Date.now();
			const timeUntilExpiry = expirationTime - currentTime;
			// For now lets refresh five minutes before it expires to ensure it stays active
			const fiveMinutes = 5 * 60 * 1000;

			// If we are already in that buffer zone, attempt the refresh and restart this process
			if (timeUntilExpiry <= fiveMinutes) {
				await refreshOktaOrLogout();
				await startOktaTokenRefreshProcess();
			} else {
				// Otherwise lets start the process
				const refreshTime = Math.max(timeUntilExpiry - fiveMinutes, 0);
				tokenRefreshInterval = setTimeout(() => {
					void (async () => {
						await refreshOktaOrLogout();
						await startOktaTokenRefreshProcess();
					})();
				}, refreshTime);
			}
		}
	};

	const handleOktaState = async (): Promise<void> => {
		accessToken = localStorage.getItem('oktaAccessToken');
		refreshToken = localStorage.getItem('oktaRefreshToken');

		// If we dont have any tokens or just an expired refresh token, user has to log in to Okta
		if ((!accessToken && !refreshToken) || (!accessToken && isTokenExpired(refreshToken))) {
			await handleOktaLogin();
		}

		// If we have no access token but an un-expired refresh token, try to refresh
		if (!accessToken || isTokenExpired(accessToken)) {
			await refreshOktaOrLogout();
		}

		if (accessToken) {
			// Only start the process if we have a valid access token
			await refreshLogin(accessToken);
			await startOktaTokenRefreshProcess();
		}
	};

	/* eslint-disable no-await-in-loop */
	const initialize = async () => {
		if (initialized) return;

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

			// if we don't have a server url, set initialized to false
			if (!config.serverUrl) {
				config.initialized = false;
			}

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

		if ($appConfig.serverless) {
			try {
				await refreshLogin('');
				showLogin = false;
			} catch (e) {
				await emit('error', e);
			}
			// Wait until the dynamic config is available
			for (;;) {
				try {
					const dynamicConfigResponse = await getDynamicConfig();
					if (!dynamicConfigResponse.playtestRegions.length) {
						throw new Error('waiting');
					}
					break;
				} catch (_) {
					// wait one second before retrying
					await new Promise((resolve) => {
						setTimeout(() => {
							resolve(null);
						}, 1000);
					});
				}
			}
		}

		if (!$oktaAuth && !$appConfig.serverless) {
			try {
				$oktaAuth = createOktaAuth($appConfig.oktaConfig.issuer, $appConfig.oktaConfig.clientId);
				await handleOktaState();
				await initialize();
			} catch (e) {
				await emit('error', e);
			}
		}

		if (accessToken || $appConfig.serverless) {
			try {
				const [dynamicConfigResponse, projectConfigResponse, buildsResponse] = await Promise.all([
					getDynamicConfig(),
					getProjectConfig(),
					getBuilds(250)
				]);

				projectConfigs.set(projectConfigResponse);
				dynamicConfig.set(dynamicConfigResponse);
				builds.set(buildsResponse);

				if ($builds.entries && $builds.entries.length > 0) {
					selectedCommit.set($builds.entries[0]);
				} else {
					selectedCommit.set(null);
				}

				if ($appConfig.repoPath !== '') {
					const [repoConfigResponse, repoStatusResponse, commitsResponse] = await Promise.all([
						getRepoConfig(),
						getRepoStatus(SkipDllCheck.False, AllowOfflineCommunication.False),
						getAllCommits(),
						initializeChangeSets()
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
			await appWindow.show();
		};
		void setupAppWindow();

		const unlisten = listen('startup-message', (e) => {
			startupMessage = e.payload as string;
		});

		const refresh = async () => {
			if (!$appConfig.initialized || $onboardingInProgress || $showPreferences) return;

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
					const found = updatedBuilds.entries?.find((build) => build.commit === selected.commit);
					if (found) {
						selectedCommit.set(found);
					}
				}

				builds.set(updatedBuilds);
			} catch (e) {
				await emit('error', e);
			}

			try {
				playtests.set(await playtestsPromise);
			} catch (e) {
				await handleError(e);
			}

			if ($appConfig.repoPath !== '') {
				try {
					// let's assume if the window isn't focused, someone is working in the editor
					// which will also be attempting to run status updates
					if (await appWindow.isFocused()) {
						commits.set(await getAllCommits());
						repoStatus.set(await getRepoStatus());

						lastRefresh = new Date().getTime();
					}
				} catch (e) {
					await emit('error', e);
				}
			}

			try {
				const update = await check();
				latest = update?.version ?? '';
				updateAvailable = update !== null;
			} catch (e) {
				await emit('error', e);
			}

			loading = false;
		};

		void appWindow.onFocusChanged(({ payload: focused }) => {
			if (focused) {
				const now = new Date().getTime();
				if (now - lastRefresh > refreshInterval) {
					void refresh();
				}
			}
		});

		initialize()
			.then(() => {
				showWelcomeModal = !get(appConfig).initialized;
			})
			.catch((e) => {
				if (e instanceof Error) {
					void emit('error', e);
				}
			});

		return () => {
			void unlisten.then((f) => {
				f();
			});
		};
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

	void listen('git-log', (event) => {
		// git-log "Updating files: 1%" etc too long, filter out and show static string
		if (event.payload.startsWith('Updating files: ')) {
			gitStartupMessage = 'Updating files...';
		} else {
			gitStartupMessage = event.payload as string;
		}
	});

	void listen('background-sync-start', () => {
		backgroundSyncInProgress.set(true);

		backgroundSyncProgress = 0;
		backgroundSyncElapsed = '';
		backgroundSyncRemaining = '';

		void listen('longtail-sync-progress', (event) => {
			const captures = event.payload as { progress: string; elapsed: string; remaining: string };
			backgroundSyncProgress = parseFloat(captures.progress.replace('%', ''));
			backgroundSyncElapsed = captures.elapsed;
			backgroundSyncRemaining = captures.remaining;
		});
	});

	void listen('background-sync-end', () => {
		backgroundSyncInProgress.set(false);
	});

	void listen('access-token-set', (e) => {
		localStorage.setItem('oktaAccessToken', e.payload as string);
		accessToken = e.payload as string;
	});

	void listen('success', (e) => {
		successMessage = e.payload as string;
		hasSuccess = true;
	});

	void listen('git-refresh', () => {
		void refreshRepo();
	});

	void listen('open-preferences', () => {
		$showPreferences = true;
	});

	void listen('scheme-request-received', (e) => {
		const payload = String(e.payload).split('friendshipper://')[1].replace(/\/$/, '');

		if (payload.startsWith('launch/')) {
			quickLaunching = true;

			// This destructuring syntax is so awful but standard linters seem
			// to prefer it.
			[, quickLaunchServerName] = payload.split('launch/');

			void goto('/');
		} else if (payload === 'home') {
			void goto('/');
		} else if (payload === 'playtests') {
			void goto('/playtests');
		} else if (payload.startsWith('builds/')) {
			const [, group, commitSha, name] = payload.split('/');

			void goto('/builds');

			// need to wait for the page to be open so it has a chance to respond to this event
			setTimeout(() => {
				void emit('build-deep-link', { group, commitSha, name });
			}, 100);
		}
	});

	let hidePizza = true;

	const toggleVersionText = () => {
		hidePizza = !hidePizza;
	};
</script>

{#key $appConfig}
	<WelcomeModal bind:showModal={showWelcomeModal} currentConfig={$appConfig} onClose={initialize} />
{/key}

<PreferencesModal
	bind:showModal={$showPreferences}
	bind:requestInFlight={preferencesModalRequestInFlight}
	bind:showProgressModal
	bind:progressModalTitle
	{handleCheckForUpdates}
/>

<div class="flex flex-col h-screen w-screen border border-primary-900 overflow-hidden rounded-md">
	<div
		class="flex justify-between items-center gap-1 w-full h-8 bg-secondary-800 dark:bg-space-950 border-b border-opacity-50 border-dotted border-primary-500"
		data-tauri-drag-region
	>
		<div class="pl-2 flex gap-2 items-center pointer-events-none">
			<Img imgClass="w-5 h-5" src="/assets/icon.png" /><span class="text-gray-300"
				>friendshipper</span
			>
		</div>
		<div class="pr-2 flex gap-2 justify-end">
			<Button
				outline
				color="dark"
				size="xs"
				class="p-1 my-1 hover:bg-secondary-800 text-gray-400 dark:hover:bg-space-950 border-0 focus-within:ring-0 dark:focus-within:ring-0 focus-within:bg-secondary-800 dark:focus-within:bg-space-950"
				on:click={async () => {
					await appWindow.minimize();
				}}
			>
				<MinusOutline class="h-4 w-4" />
			</Button>
			<Button
				outline
				color="dark"
				size="xs"
				class="p-1 my-1 hover:bg-secondary-800 text-gray-400 dark:hover:bg-space-950 border-0 focus-within:ring-0 dark:focus-within:ring-0 focus-within:bg-secondary-800 dark:focus-within:bg-space-950"
				on:click={async () => {
					await appWindow.toggleMaximize();
				}}
			>
				<WindowOutline class="h-4 w-4" />
			</Button>
			<Button
				outline
				color="dark"
				size="xs"
				class="p-1 my-1 hover:bg-secondary-800 text-gray-400 dark:hover:bg-space-950 border-0 focus-within:ring-0 dark:focus-within:ring-0 focus-within:bg-secondary-800 dark:focus-within:bg-space-950"
				on:click={async () => {
					await appWindow.hide();
				}}
			>
				<CloseOutline class="h-4 w-4" />
			</Button>
		</div>
	</div>
	{#if (!initialized || !accessToken) && showLogin}
		{#if initialized}
			<div>
				<div
					class="flex flex-col gap-2 px-12 bg-secondary-700 dark:bg-space-900 items-center w-screen h-screen justify-center"
					data-tauri-drag-region
				>
					<button
						class="bg-blue-500 hover:bg-blue-700 text-white font-bold py-2 px-4 rounded"
						on:click={handleOktaLogin}
					>
						Login With Okta
					</button>
				</div>
			</div>
		{:else}
			<div
				class="flex flex-col gap-2 px-12 bg-secondary-700 dark:bg-space-900 items-center w-screen h-screen justify-center"
			>
				<div class="flex items-center gap-2">
					<span class="text-gray-300 text-xl">{startupMessage}...</span>
					<Spinner size="4" />
				</div>
				{#if gitStartupMessage}
					<div class="rounded-md p-2 bg-secondary-800 dark:bg-space-950">
						<code class="text-sm text-gray-300 dark:text-gray-300 m-0">{gitStartupMessage}</code>
					</div>
				{/if}
				<Button on:click={openSystemLogsFolder}>Open Logs Folder</Button>
			</div>
		{/if}
	{:else}
		<div
			class="flex bg-secondary-800 dark:bg-space-950 h-full overflow-y-hidden w-full overflow-x-hidden"
		>
			<QuickLaunchModal bind:showModal={quickLaunching} serverName={quickLaunchServerName} />
			<Sidebar
				asideClass="w-56 shadow-md sticky top-0 h-full"
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
						<SidebarItem
							class="group/item"
							label="Servers"
							href="/servers"
							active={activeUrl === '/servers'}
							{spanClass}
						>
							<svelte:fragment slot="icon">
								<ServerOutline
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
									<ChevronUpOutline class="h-5 w-5 text-white" />
								</svelte:fragment>
								<svelte:fragment slot="arrowdown">
									<ChevronDownOutline class="h-5 w-5 text-white" />
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
											class="items-center px-2 ms-3 text-sm font-medium text-white rounded-full {$repoStatus &&
											$repoStatus?.locksOurs.length > 0
												? 'bg-primary-600 dark:bg-primary-600'
												: 'bg-gray-500 dark:bg-gray-500'}"
										>
											{$repoStatus?.locksOurs.length}
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
							class="group/item text-primary-400 dark:text-primary-400 hover:bg-secondary-800 dark:hover:bg-space-950 rounded-lg"
							ulClass="my-2 rounded-lg py-1 bg-secondary-800 dark:bg-space-950"
						>
							<svelte:fragment slot="icon">
								<ComputerSpeakerSolid
									class="w-5 h-5 transition duration-75 text-gray-400 dark:text-gray-400 group-hover/item:text-white dark:group-hover/item:text-white"
								/>
							</svelte:fragment>
							<svelte:fragment slot="arrowup">
								<ChevronUpOutline class="h-5 w-5 text-white" />
							</svelte:fragment>
							<svelte:fragment slot="arrowdown">
								<ChevronDownOutline class="h-5 w-5 text-white" />
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
										$showPreferences = true;
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

			<div class="flex flex-col mx-auto w-full h-full overflow-hidden">
				<main class="w-full h-full flex flex-col px-4 pb-2 overflow-hidden">
					<slot class="overflow-hidden" />
				</main>
			</div>
		</div>
	{/if}
	{#if $backgroundSyncInProgress}
		<div
			class="flex gap-1 items-center bg-secondary-700 dark:bg-space-900 h-6 max-h-6 w-full py-1 px-2 z-50"
		>
			<code class="text-xs text-gray-400 dark:text-gray-400">Syncing... </code>
			<Spinner size="2" />
			<Progressbar progress={backgroundSyncProgress} size="h-1" />
			<code class="text-xs text-gray-400 dark:text-gray-400 text-nowrap">
				{backgroundSyncElapsed} / {backgroundSyncRemaining}
			</code>
			<Button
				outline
				color="dark"
				size="xs"
				class="p-1 my-1 hover:bg-secondary-800 text-gray-400 dark:hover:bg-space-950 border-0 focus-within:ring-0 dark:focus-within:ring-0 focus-within:bg-secondary-800 dark:focus-within:bg-space-950"
				on:click={handleCancelBackgroundSync}
			>
				<CloseOutline class="h-3 w-3" />
			</Button>
		</div>
	{/if}
</div>
<ErrorToast bind:show={hasError} {errorMessage} onClose={onErrorDismissed} />
<SuccessToast bind:show={hasSuccess} message={successMessage} onClose={onSuccessDismissed} />
<ProgressModal bind:showModal={showProgressModal} title={progressModalTitle} />
<!-- Hidden dark mode toggle allows us to load the theme immediately, even though the actual toggle is in the preferences modal -->
<DarkMode class="hidden" />

<!-- Update modal, not quite worthy of a component yet -->
<Modal
	open={updateAvailable && !$updateDismissed}
	size="sm"
	defaultClass="bg-secondary-700 dark:bg-space-900 overflow-y-auto"
	bodyClass="!border-t-0"
	backdropClass="fixed mt-8 inset-0 z-40 bg-gray-900 bg-opacity-50 dark:bg-opacity-80"
	dialogClass="fixed mt-8 top-0 start-0 end-0 h-modal md:inset-0 md:h-full z-50 w-full p-4 pb-12 flex"
	on:close={() => {
		updateDismissed.set(true);
	}}
>
	<div class="flex flex-col gap-2">
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
		{#if updateProgress > 0}
			<div class="flex items-center justify-between">
				<Progressbar progress={updateProgress} size="h-1" />
			</div>
		{/if}
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
		border-radius: 12px;
	}

	:global(::-webkit-scrollbar-thumb) {
		background: theme('colors.primary.500');
		border-radius: 10px;
	}

	:global(::-webkit-scrollbar-corner) {
		background: theme('colors.secondary.700');
		border-radius: 12px;
	}
</style>
