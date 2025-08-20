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
		Spinner,
		Tooltip
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
	import { openSystemLogsFolder, shutdownServer } from '$lib/system';
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
		currentSyncedVersion,
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

	import WelcomeModal from '$lib/components/oobe/WelcomeModal.svelte';
	import {
		getAppConfig,
		getDynamicConfig,
		getProjectConfig,
		getRepoConfig,
		resetConfig
	} from '$lib/config';
	import { handleError, logError, logInfo } from '$lib/utils';
	import { createOktaAuth, setupOktaEventListeners, clearExpiredTokens } from '$lib/okta';
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

	// Welcome modal
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

	// Reset config confirmation at startup
	let showResetConfirmModal = false;

	const handleCancelBackgroundSync = async () => {
		try {
			await cancelDownload();

			backgroundSyncProgress = 0;
			backgroundSyncElapsed = '';
			backgroundSyncRemaining = '';

			await emit('background-sync-cancel');
		} catch (e) {
			await logError('Background sync cancel failed', e);
		}
	};

	const handleResetConfigRequest = () => {
		showResetConfirmModal = true;
	};

	const confirmResetConfig = async () => {
		showResetConfirmModal = false;
		await resetConfig();
	};

	$: conflictsDetected = $repoStatus?.conflicts && $repoStatus?.conflicts.length > 0;

	const spanClass = 'flex-1 ml-3 whitespace-nowrap';
	const sidebarSubItemClass = 'mx-2 my-1 text-sm text-primary-400 dark:text-primary-400';
	const sidebarSubItemInactiveClass =
		'flex items-center justify-between mx-2 my-1 px-2 py-1 text-base font-normal rounded-lg text-primary-400 dark:text-primary-400 bg-secondary-800 dark:bg-space-950 hover:bg-secondary-700 dark:hover:bg-space-900';
	const sidebarSubItemActiveClass =
		'flex items-center justify-between mx-2 my-1 px-2 py-1 text-base font-normal bg-secondary-700 dark:bg-space-900 rounded-lg text-primary-400 dark:text-primary-400 hover:bg-secondary-700 dark:hover:bg-space-900';

	$: activeUrl = $page.url.pathname;

	let showLogin: boolean = true;
	let hasValidTokens: boolean = false;

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
			await logError('Update check failed', e);
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
			await logError('Update failed', e);
		}

		updating = false;
	};

	const refreshRepo = async () => {
		repoStatus.set(await getRepoStatus());
		void logInfo('Files refreshed!');
	};

	const initializeChangeSets = async () => {
		if ($activeProjectConfig === null) {
			await logError('No active project found, unable to load changesets from file.');
			return;
		}

		$changeSets = await loadChangeSet();
	};

	const handleOktaLogout = async () => {
		try {
			// Clear Okta token manager (this clears okta-cache-storage)
			if ($oktaAuth) {
				$oktaAuth.tokenManager.clear();
			}

			hasValidTokens = false;
		} catch (err) {
			await logError('Token logout failed', err);
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
						scopes: ['openid', 'email', 'profile', 'offline_access']
					});
				} else {
					const { tokens } = await $oktaAuth.token.getWithPopup({
						scopes: ['openid', 'email', 'profile', 'offline_access']
					});

					if (tokens && tokens.accessToken) {
						$oktaAuth.tokenManager.setTokens(tokens);

						await emit('access-token-set', tokens.accessToken.accessToken);
						try {
							await logInfo('Calling backend refreshLogin');
							await refreshLogin(tokens.accessToken.accessToken);
							await logInfo('Backend refreshLogin succeeded');
							hasValidTokens = true;
						} catch (backendError) {
							await logError('Backend refreshLogin failed', backendError);
						}
					}
				}
			}

			startupMessage = previousStartupMessage;
		} catch (err) {
			await handleOktaLogout();
			await logError('Okta login failed', err);
		}
	};

	const handleOktaState = async (): Promise<void> => {
		if (!$oktaAuth) {
			return;
		}

		try {
			// First, clear any expired tokens from storage
			await clearExpiredTokens($oktaAuth);
			const oktaTokens = await $oktaAuth.tokenManager.getTokens();

			await logInfo(
				`handleOktaState: Loaded tokens from Okta - hasAccess: ${!!oktaTokens.accessToken}, hasRefresh: ${!!oktaTokens.refreshToken}`
			);

			// If we don't have any tokens, user has to log in
			if (!oktaTokens.accessToken && !oktaTokens.refreshToken) {
				await handleOktaLogin();
				return;
			}

			// If access token exists but is expired, force re-authentication
			// Okta's auto-renewal should handle this, but if we get here the token is expired
			if (oktaTokens.accessToken && $oktaAuth.tokenManager.hasExpired(oktaTokens.accessToken)) {
				await logInfo('handleOktaState: Access token is expired, forcing re-authentication');
				await handleOktaLogin();
				return;
			}

			// Get current tokens after potential refresh
			const currentTokens = await $oktaAuth.tokenManager.getTokens();
			if (
				currentTokens.accessToken &&
				!$oktaAuth.tokenManager.hasExpired(currentTokens.accessToken)
			) {
				// Update backend and set valid state
				await refreshLogin(currentTokens.accessToken.accessToken);
				hasValidTokens = true;
				await emit('access-token-set', currentTokens.accessToken.accessToken);
			} else {
				await handleOktaLogin();
			}
		} catch (error) {
			await logError('handleOktaState: Error loading from Okta token manager', error);
			await handleOktaLogin();
		}
	};

	// Restore tokens from Okta's storage (called after Okta Auth is initialized)
	const restoreTokensFromOkta = async () => {
		try {
			if ($oktaAuth) {
				await logInfo('[STARTUP] Checking for existing tokens in Okta storage');

				// Clear any expired tokens from storage first
				await clearExpiredTokens($oktaAuth);
				const tokens = await $oktaAuth.tokenManager.getTokens();

				if (tokens.accessToken) {
					await logInfo('[STARTUP] Found existing access token in Okta storage');

					// Check if access token is still valid
					if (!$oktaAuth.tokenManager.hasExpired(tokens.accessToken)) {
						await logInfo('[STARTUP] Access token is still valid');

						// Log token info for debugging
						try {
							const accessDecoded = jwtDecode(tokens.accessToken.accessToken);
							const accessLifetimeHours = (accessDecoded.exp - accessDecoded.iat) / 3600;
							await logInfo(
								`[STARTUP] Access token lifetime: ${accessLifetimeHours.toFixed(
									2
								)} hours (expires: ${new Date(accessDecoded.exp * 1000).toISOString()})`
							);

							if (tokens.refreshToken) {
								const { refreshToken } = tokens.refreshToken;
								const tokenParts = refreshToken.split('.').length;
								await logInfo(
									`[STARTUP] Refresh token type: ${tokenParts === 3 ? 'JWT' : 'opaque'}`
								);

								// Enhanced debugging for refresh token
								if (tokenParts === 3) {
									try {
										const refreshDecoded = jwtDecode(refreshToken);
										const refreshLifetimeHours = (refreshDecoded.exp - refreshDecoded.iat) / 3600;
										const refreshExpiresAt = new Date(refreshDecoded.exp * 1000);
										await logInfo(
											`[STARTUP] JWT Refresh token lifetime: ${refreshLifetimeHours.toFixed(
												2
											)} hours (${(refreshLifetimeHours / 24).toFixed(1)} days)`
										);
										await logInfo(
											`[STARTUP] JWT Refresh token expires: ${refreshExpiresAt.toISOString()}`
										);
										await logInfo(
											`[STARTUP] JWT Refresh token raw exp: ${
												refreshDecoded.exp
											} (current: ${Math.floor(Date.now() / 1000)})`
										);
									} catch (_refreshDecodeError) {
										await logInfo('[STARTUP] Could not decode JWT refresh token');
									}
								} else {
									await logInfo(
										'[STARTUP] Opaque refresh token - expiration managed server-side by Okta'
									);
								}
							}
						} catch (_decodeError) {
							await logError('Failed to decode token info at startup', _decodeError);
						}

						// Mark as valid
						hasValidTokens = true;
						await emit('access-token-set', tokens.accessToken.accessToken);
					} else {
						await logInfo('[STARTUP] Access token is expired, will need fresh login');
						// Okta auto-renewal should handle expired tokens automatically
						// If we get here, force fresh authentication
					}
				} else {
					await logInfo('[STARTUP] No existing tokens found in Okta storage');
				}
			} else {
				await logError('[STARTUP] Okta Auth not available during token restoration');
			}
		} catch (_error) {
			await logError('Failed to restore tokens on startup', _error);
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
				await logError('App config initialization failed', e);
			}
		}

		if ($appConfig.serverless) {
			try {
				await refreshLogin('');
				showLogin = false;
			} catch (e) {
				await logError('Serverless login failed', e);
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

				// Start the Okta service for auto-renewal to work
				await $oktaAuth.start();

				// Debounce token renewal to prevent spam (Okta recommended practice)
				let lastRenewalTime = 0;
				const RENEWAL_DEBOUNCE_MS = 5000; // 5 seconds

				// Set up event listeners ONCE after initialization
				setupOktaEventListeners(
					$oktaAuth,
					// On token renewed (debounced to prevent multiple rapid calls)
					(newAccessToken: string) => {
						const now = Date.now();
						if (now - lastRenewalTime < RENEWAL_DEBOUNCE_MS) {
							void (async () => {
								await logInfo('Skipping token renewal - too recent');
							})();
							return;
						}
						lastRenewalTime = now;

						void (async () => {
							await logInfo('Okta automatically renewed access token');
							await emit('access-token-set', newAccessToken);
							try {
								await refreshLogin(newAccessToken);
								hasValidTokens = true;
							} catch (error) {
								await logError('Backend refresh failed after automatic token renewal', error);
							}
						})();
					},
					// On token expired (fallback if auto-renewal fails)
					() => {
						void (async () => {
							await logInfo('Access token expired, forcing re-authentication');
							hasValidTokens = false;
							await handleOktaLogout();
							await handleOktaLogin();
						})();
					}
				);

				// Now that Okta Auth is ready, restore tokens first
				await restoreTokensFromOkta();

				await handleOktaState();
				await initialize();
			} catch (e) {
				await emit('error', e);
			}
		}

		if (hasValidTokens || $appConfig.serverless) {
			try {
				// Initialize the current synced version from localStorage
				currentSyncedVersion.initialize();

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

			loading = true;

			const selected = get(selectedCommit);
			if ($repoConfig == null || $repoConfig?.buildsEnabled) {
				const buildsPromise = getBuilds(250);

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
			}

			if ($repoConfig == null || $repoConfig.serversEnabled) {
				const playtestsPromise = getPlaytests();

				try {
					playtests.set(await playtestsPromise);
				} catch (e) {
					await handleError(e);
				}
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
				// Silent token renewal when app regains focus (handles background renewal failures)
				if ($oktaAuth && hasValidTokens) {
					void (async () => {
						try {
							const tokens = await $oktaAuth.tokenManager.getTokens();
							if (tokens.accessToken && $oktaAuth.tokenManager.hasExpired(tokens.accessToken)) {
								await logInfo('App regained focus - attempting silent token renewal');

								// Try silent renewal first (Okta recommended approach)
								try {
									await $oktaAuth.tokenManager.renew('accessToken');
									const renewedTokens = await $oktaAuth.tokenManager.getTokens();
									if (renewedTokens.accessToken) {
										await emit('access-token-set', renewedTokens.accessToken.accessToken);
										await refreshLogin(renewedTokens.accessToken.accessToken);
										await logInfo('Silent token renewal successful');
									}
								} catch (_renewError) {
									// Silent renewal failed, fallback to full re-auth
									await logInfo('Silent renewal failed, falling back to re-authentication');
									hasValidTokens = false;
									await handleOktaLogout();
									await handleOktaLogin();
									return; // Don't refresh data if auth failed
								}
							}

							// Now refresh data with valid tokens
							const now = new Date().getTime();
							if (now - lastRefresh > refreshInterval) {
								void refresh();
							}
						} catch (error) {
							await logError('Focus change token check failed', error);
						}
					})();
				} else {
					// No auth needed, just refresh normally
					const now = new Date().getTime();
					if (now - lastRefresh > refreshInterval) {
						void refresh();
					}
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

	void listen('access-token-set', (_e) => {
		// Token validity is now managed by Okta's event system
		// hasValidTokens is set directly when tokens are confirmed valid
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
		} else if (payload === 'playtests' && ($repoConfig == null || $repoConfig.serversEnabled)) {
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
	{#if (!initialized || !hasValidTokens) && showLogin}
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
					<span
						class="text-xl {startupMessage.startsWith('App configuration error') ||
						startupMessage.startsWith('Warning:')
							? 'text-red-400'
							: 'text-gray-300'}"
					>
						{startupMessage}{startupMessage.startsWith('App configuration error') ||
						startupMessage.startsWith('Warning:')
							? ''
							: '...'}
					</span>
					{#if !startupMessage.startsWith('App configuration error') && !startupMessage.startsWith('Warning:')}
						<Spinner size="4" />
					{/if}
				</div>
				{#if gitStartupMessage}
					<div class="rounded-md p-2 bg-secondary-800 dark:bg-space-950">
						<code class="text-sm text-gray-300 dark:text-gray-300 m-0">{gitStartupMessage}</code>
					</div>
				{/if}
				<div class="flex gap-2">
					<Button on:click={openSystemLogsFolder}>Open Logs Folder</Button>
					<Button color="red" on:click={handleResetConfigRequest}>Reset Config & Restart</Button>
					<Tooltip
						class="w-auto text-xs text-primary-400 bg-secondary-700 dark:bg-space-900"
						placement="bottom"
						>Restart the app and repeat the onboarding flow
					</Tooltip>
				</div>
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
						{#if $repoConfig == null || $repoConfig.serversEnabled}
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
						{/if}

						{#if $repoConfig == null || $repoConfig.buildsEnabled}
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

<!-- Bootup Reset config confirmation modal -->
<Modal
	open={showResetConfirmModal}
	size="sm"
	defaultClass="bg-secondary-700 dark:bg-space-900 overflow-y-auto"
	bodyClass="!border-t-0"
	backdropClass="fixed mt-8 inset-0 z-40 bg-gray-900 bg-opacity-50 dark:bg-opacity-80"
	dialogClass="fixed mt-8 top-0 start-0 end-0 h-modal md:inset-0 md:h-full z-50 w-full p-4 pb-12 flex"
	on:close={() => {
		showResetConfirmModal = false;
	}}
>
	<div class="flex flex-col gap-4">
		<div class="text-white">
			<h3 class="text-lg font-semibold mb-2">Reset Configuration</h3>
			<p class="text-gray-300">
				Are you sure you want to reset Friendshipper's configuration? This will clear all settings
				and restart the app, requiring you to go through the setup process again.
			</p>
		</div>
		<div class="flex gap-2 justify-end">
			<Button
				color="gray"
				on:click={() => {
					showResetConfirmModal = false;
				}}
			>
				Cancel
			</Button>
			<Button color="red" on:click={confirmResetConfig}>Reset & Restart</Button>
		</div>
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
