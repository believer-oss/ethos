<script lang="ts">
	import { Button, Spinner, Tooltip } from 'flowbite-svelte';
	import { ArrowLeftOutline, RefreshOutline } from 'flowbite-svelte-icons';
	import { emit } from '@tauri-apps/api/event';
	import { onMount } from 'svelte';
	import { getFileHistory, getRepoStatus, listRepoDirectory } from '$lib/repo';
	import { appConfig, repoStatus } from '$lib/stores';
	import { openUrl } from '$lib/utils';
	import type { FileHistoryRevision, RepoDirectoryEntry, RepoDirectoryListing } from '$lib/types';
	import RepositoryBreadcrumb from './RepositoryBreadcrumb.svelte';
	import RepositoryFileList from './RepositoryFileList.svelte';
	import FileHistoryPanel from '$lib/components/FileHistoryPanel.svelte';
	import { repositoryViewState } from './state';

	// Navigation state lives in a module-scope singleton so it survives leaving and returning
	// to this route within the app lifetime.
	let { currentPath } = repositoryViewState;
	let visitedStack: string[] = [...repositoryViewState.visitedStack];
	let listing: RepoDirectoryListing | null = null;
	const { listingCache } = repositoryViewState;

	let { selectedPath } = repositoryViewState;
	let revisions: FileHistoryRevision[] = [...repositoryViewState.revisions];
	let selectedDisplayName: string = repositoryViewState.displayName;

	$: repositoryViewState.currentPath = currentPath;
	$: repositoryViewState.visitedStack = visitedStack;
	$: repositoryViewState.selectedPath = selectedPath;
	$: repositoryViewState.revisions = revisions;
	$: repositoryViewState.displayName = selectedDisplayName;

	let listingLoading = false;
	let historyLoading = false;
	let refreshing = false;

	const loadDirectory = async (targetPath: string, useCache: boolean = true) => {
		listingLoading = true;
		try {
			if (useCache && listingCache.has(targetPath)) {
				listing = listingCache.get(targetPath) ?? null;
			} else {
				const result = await listRepoDirectory(targetPath);
				listingCache.set(targetPath, result);
				listing = result;
			}
			currentPath = targetPath;
		} catch (e) {
			await emit('error', e);
		} finally {
			listingLoading = false;
		}
	};

	const navigateInto = async (entry: RepoDirectoryEntry) => {
		if (entry.kind !== 'directory') return;
		visitedStack = [...visitedStack, currentPath];
		await loadDirectory(entry.path);
	};

	const navigateTo = async (targetPath: string) => {
		if (targetPath === currentPath) return;
		visitedStack = [...visitedStack, currentPath];
		await loadDirectory(targetPath);
	};

	const goBack = async () => {
		if (visitedStack.length === 0) return;
		const prev = visitedStack[visitedStack.length - 1];
		visitedStack = visitedStack.slice(0, -1);
		await loadDirectory(prev);
	};

	const handleSelectFile = async (entry: RepoDirectoryEntry) => {
		if (entry.kind !== 'file') return;
		selectedPath = entry.path;
		revisions = [];
		selectedDisplayName = '';
		historyLoading = true;
		try {
			const response = await getFileHistory(entry.path);
			revisions = response.revisions;
			selectedDisplayName = response.displayName ?? '';
		} catch (e) {
			await emit('error', e);
		} finally {
			historyLoading = false;
		}
	};

	const handleOpenInExplorer = async (entry: RepoDirectoryEntry) => {
		const repoRoot = $appConfig.repoPath;
		if (!repoRoot) {
			await emit('error', 'Repo path is not configured.');
			return;
		}
		const fullPath =
			entry.kind === 'directory'
				? `${repoRoot}/${entry.path}`
				: `${repoRoot}/${entry.path.split('/').slice(0, -1).join('/')}`;
		try {
			await openUrl(fullPath);
		} catch (e) {
			await emit('error', e);
		}
	};

	const handleRefresh = async () => {
		refreshing = true;
		try {
			// Invalidate cache and pull fresh status (populates modified_upstream from last fetch).
			listingCache.clear();
			repoStatus.set(await getRepoStatus());
			await loadDirectory(currentPath, false);
			if (selectedPath) {
				historyLoading = true;
				try {
					const response = await getFileHistory(selectedPath);
					revisions = response.revisions;
					selectedDisplayName = response.displayName ?? '';
				} finally {
					historyLoading = false;
				}
			}
		} catch (e) {
			await emit('error', e);
		} finally {
			refreshing = false;
		}
	};

	onMount(async () => {
		// Re-fetch the current folder so state reflects any changes made elsewhere in the app,
		// but keep the preserved currentPath/visitedStack/selectedPath from module scope.
		await loadDirectory(currentPath, false);
		if (selectedPath) {
			historyLoading = true;
			try {
				const response = await getFileHistory(selectedPath);
				revisions = response.revisions;
				selectedDisplayName = response.displayName ?? '';
			} catch (_) {
				// Leave previously fetched revisions if this file no longer exists.
			} finally {
				historyLoading = false;
			}
		}
	});

	$: entries = listing?.entries ?? [];
</script>

<div class="flex items-center justify-between gap-2">
	<div class="flex items-center gap-2">
		<p class="text-2xl my-2 text-primary-400 dark:text-primary-400">Repository</p>
		<Button
			size="xs"
			disabled={visitedStack.length === 0 || listingLoading}
			on:click={goBack}
			class="!p-1.5"
		>
			<ArrowLeftOutline class="w-4 h-4" />
		</Button>
		<Tooltip
			class="w-auto bg-secondary-600 dark:bg-space-800 font-semibold shadow-2xl"
			placement="bottom">Back</Tooltip
		>
		<Button
			size="xs"
			disabled={refreshing || listingLoading}
			on:click={handleRefresh}
			class="!p-1.5"
		>
			{#if refreshing}
				<Spinner size="4" />
			{:else}
				<RefreshOutline class="w-4 h-4" />
			{/if}
		</Button>
		<Tooltip
			class="w-auto bg-secondary-600 dark:bg-space-800 font-semibold shadow-2xl"
			placement="bottom">Refresh listing and status</Tooltip
		>
	</div>
	<div class="text-sm text-gray-300">
		On branch: <span class="text-primary-400 font-normal">{$repoStatus?.branch ?? ''}</span>
	</div>
</div>

<div class="pb-2">
	<RepositoryBreadcrumb path={currentPath} onNavigate={navigateTo} />
</div>

<div class="grid grid-cols-3 gap-4 h-full overflow-hidden">
	<div class="col-span-2 h-full overflow-hidden">
		<RepositoryFileList
			path={currentPath}
			{entries}
			loading={listingLoading}
			{selectedPath}
			onNavigateDirectory={navigateInto}
			onSelectFile={handleSelectFile}
			onOpenDirectory={handleOpenInExplorer}
		/>
	</div>
	<div class="col-span-1 h-full overflow-hidden">
		<FileHistoryPanel
			filePath={selectedPath}
			displayName={selectedDisplayName}
			{revisions}
			loading={historyLoading}
			onReverted={handleRefresh}
		/>
	</div>
</div>
